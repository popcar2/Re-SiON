//! Converters to SiON MML: Standard MIDI Files, tracker modules (MOD, S3M, XM, IT) and VGM/VGZ chip logs.
//! Every reader produces a `score::Score`; `arrange` and `mml` turn that into minified MML.

mod arrange;
mod chips;
mod fm;
pub mod midi;
mod mml;
mod pcm;
mod play;
mod sample;
mod score;
mod tracker;
mod vgm;

pub use arrange::Options;

pub struct Converted {
    /// Minified MML.
    pub mml: String,
    /// The same song written plainly (one command per event): the audio reference for `mml`.
    pub plain: String,
    pub tracks: usize,
    pub notes: usize,
    /// How long the song plays (once through), and for a looping song how long the loop is: renders of it
    /// should stop somewhere, since it never ends.
    pub secs: f64,
    pub loop_secs: Option<f64>,
    /// What the conversion could not carry over (unsupported chips, effects...), for the user.
    pub warnings: Vec<String>,
}

/// Input formats, recognized from the file's contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format { Midi, Vgm, Module }

pub fn detect(data: &[u8]) -> Option<Format> {
    if data.starts_with(b"Vgm ") { return Some(Format::Vgm) }
    if data.starts_with(&[0x1f, 0x8b]) { return vgm::gunzip(data).ok().filter(|d| d.starts_with(b"Vgm ")).map(|_| Format::Vgm) }
    if tracker::detect(data).is_some() { return Some(Format::Module) }
    if data.windows(4).any(|w| w == b"MThd") { return Some(Format::Midi) }
    None
}

/// Convert any supported file.
pub fn convert(data: &[u8], opt: &Options) -> Result<Converted, String> {
    match detect(data) {
        Some(Format::Midi) => midi_to_mml(data, opt),
        Some(Format::Vgm) => { let (score, w) = vgm::to_score(data, opt.exact_pcm)?; Ok(write(&score, opt, w)) }
        Some(Format::Module) => Ok(module_to_mml(&tracker::load(data)?, opt)),
        None => Err("unrecognized file (supported: MIDI, MOD, S3M, XM, IT, VGM/VGZ)".into()),
    }
}

fn module_to_mml(m: &tracker::Module, opt: &Options) -> Converted {
    let (score, w, _) = play::to_score(m, opt.wavetables);
    write(&score, opt, w)
}

/// A tracker module mixed straight from its samples, as the converter performs it (interleaved stereo at
/// `sion::RATE`): what its `%7` conversion should sound like.
pub fn reference(data: &[u8]) -> Result<Vec<f32>, String> {
    if detect(data) != Some(Format::Module) { return Err("not a tracker module".into()) }
    let m = tracker::load(data)?;
    let (_, _, tr) = play::to_score(&m, false);
    Ok(play::render(&m, &tr))
}

/// How closely `got` matches `want` up to overall volume: signal-to-error ratio in dB (infinite when identical
/// up to scale). Compares the overlapping length.
pub fn snr(want: &[f32], got: &[f32]) -> f64 {
    let n = want.len().min(got.len());
    let (mut wg, mut ww) = (0f64, 0f64);
    for i in 0..n { wg += want[i] as f64 * got[i] as f64; ww += want[i] as f64 * want[i] as f64 }
    let gg: f64 = got[..n].iter().map(|v| *v as f64 * *v as f64).sum();
    if ww == 0.0 { return if gg < 1e-12 { f64::INFINITY } else { f64::NEG_INFINITY } }
    let g = wg / ww;
    let err: f64 = (0..n).map(|i| (got[i] as f64 - g * want[i] as f64).powi(2)).sum();
    10.0 * (g * g * ww / err).log10()
}

pub fn midi_to_mml(data: &[u8], opt: &Options) -> Result<Converted, String> {
    let smf = midi::parse(data)?;
    Ok(write(&midi::to_score(&smf), opt, vec![]))
}

fn write(score: &score::Score, opt: &Options, warnings: Vec<String>) -> Converted {
    let mut song = arrange::build(score, opt);
    let secs = score.seconds(score.notes.iter().map(|n| n.end).fold(score.end, u64::max));
    let loop_secs = score.lp.map(|l| secs - score.seconds(l));
    // (A looping song renders once through, plus a second of its loop.)
    let limit = loop_secs.map_or(60.0 * 30.0, |_| secs + 1.0);
    let mut lens = mml::Lengths::new();
    let mut plain: Vec<_> = song.tracks.iter().map(|t| arrange::tokens(t, false, &lens)).collect();
    // The volume estimate can't know how loud SiON's waveforms and FM voices really come out: if the result
    // would clip, turn the master volume down until it doesn't.
    // (Rendered samples are limited to ±1, so a clipping peak only says "too loud": step down until it fits.)
    if opt.gain.is_none() {
        for _ in 0..8 {
            let (peak, x) = (peak(&mml::plain(&song.header, &plain, &lens), limit), song.master());
            if peak <= 0.98 || x <= 1 { break }
            song.set_master(((x as f32 * if peak >= 0.999 { 0.75 } else { 0.95 / peak }) as i32).min(x - 1));
            plain = song.tracks.iter().map(|t| arrange::tokens(t, false, &lens)).collect();
        }
    }
    let toks: Vec<_> = song.tracks.iter().map(|t| arrange::tokens(t, true, &lens)).collect();
    Converted {
        plain: mml::plain(&song.header, &plain, &lens),
        mml: mml::minify(&song.header, &toks, &mut lens),
        tracks: song.tracks.len(),
        notes: song.notes,
        secs,
        loop_secs,
        warnings,
    }
}

/// Loudest sample of the rendered MML (at most `secs` seconds).
fn peak(mml: &str, secs: f64) -> f32 {
    let (mut song, mut buf, mut peak, mut frames) = (sion::Song::new(mml), vec![0f32; 8192], 0f32, 0usize);
    loop {
        buf.fill(0.0);
        let more = song.render(&mut buf);
        peak = buf.iter().fold(peak, |p, s| p.max(s.abs()));
        frames += buf.len() / 2;
        if !more || frames as f64 >= sion::RATE as f64 * secs { return peak }
    }
}

#[cfg(test)]
mod tests;
