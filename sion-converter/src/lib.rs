//! Converters to SiON MML: Standard MIDI Files, tracker modules (MOD, S3M, XM, IT), VGM/VGZ, GYM and S98 chip
//! logs, NSF/NSFE/NSF2 NES music, GBS Game Boy music and SPC SNES music.
//! Every reader produces a `score::Score`; `arrange` turns that into tracks, which `pretty` lays out by the score's
//! sections (a tracker's patterns, bars for the other formats), or which `mml` minifies (`--exact` modules).

mod arrange;
mod bytes;
mod chips;
mod cpu;
mod fm;
mod gbs;
mod logs;
pub mod midi;
mod mml;
mod nsf;
mod pcm;
mod play;
mod pretty;
mod sample;
mod score;
mod sm83;
mod spc;
mod spc700;
mod tracker;
mod vgm;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use arrange::Options;

pub struct Converted {
    /// The MML: minified, or laid out by the source's sections (see `pretty`).
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
pub enum Format { Midi, Vgm, Gym, S98, Nsf, Gbs, Spc, Module }

pub fn detect(data: &[u8]) -> Option<Format> {
    if data.starts_with(b"Vgm ") { return Some(Format::Vgm) }
    if data.starts_with(&[0x1f, 0x8b]) { return vgm::gunzip(data).ok().filter(|d| d.starts_with(b"Vgm ")).map(|_| Format::Vgm) }
    if logs::detect_s98(data) { return Some(Format::S98) }
    if data.starts_with(b"GYMX") { return Some(Format::Gym) }
    if nsf::detect(data) { return Some(Format::Nsf) }
    if gbs::detect(data) { return Some(Format::Gbs) }
    if spc::detect(data) { return Some(Format::Spc) }
    if tracker::detect(data).is_some() { return Some(Format::Module) }
    if data.windows(4).any(|w| w == b"MThd") { return Some(Format::Midi) }
    // (A GYM without its header is only commands: recognized last, by being well formed.)
    if logs::detect_gym(data) { return Some(Format::Gym) }
    None
}

/// Convert any supported file.
pub fn convert(data: &[u8], opt: &Options) -> Result<Converted, String> {
    match detect(data) {
        Some(Format::Midi) => midi_to_mml(data, opt),
        Some(Format::Module) => Ok(module_to_mml(&tracker::load(data)?, opt)),
        Some(f) => {
            let (mut score, w) = match f {
                Format::Vgm => vgm::to_score(data, opt.exact_pcm)?,
                Format::Gym | Format::S98 => {
                    let (v, mut w) = if f == Format::Gym { logs::gym_to_vgm(data)? } else { logs::s98_to_vgm(data)? };
                    let (score, more) = vgm::to_score(&v, opt.exact_pcm)?;
                    w.extend(more);
                    (score, w)
                }
                Format::Nsf => nsf::to_score(data, opt.exact_pcm, opt.exact_rate, opt.track)?,
                Format::Gbs => gbs::to_score(data, opt.exact_rate, opt.track)?,
                _ => spc::to_score(data)?,
            };
            // (Chip logs have no bars of their own: the tempo makes a row a 1/16 note, so a line is 16 rows.)
            score.set_bars(&[]);
            Ok(write(&score, opt, w))
        }
        None => Err("unrecognized file (supported: MIDI, MOD, S3M, XM, IT, VGM/VGZ, GYM, S98, NSF/NSFE/NSF2, GBS, SPC)".into()),
    }
}

fn module_to_mml(m: &tracker::Module, opt: &Options) -> Converted {
    let (score, w, _) = play::to_score(m, opt.wavetables, !opt.exact);
    write(&score, opt, w)
}

/// A tracker module mixed straight from its samples, as the converter performs it (interleaved stereo at
/// `sion::RATE`): what its `%7` conversion should sound like.
pub fn reference(data: &[u8]) -> Result<Vec<f32>, String> {
    if detect(data) != Some(Format::Module) { return Err("not a tracker module".into()) }
    let m = tracker::load(data)?;
    let (_, _, tr) = play::to_score(&m, false, false);
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
    let secs = score.seconds(score.last());
    let loop_secs = score.lp.map(|l| secs - score.seconds(l));
    // (A looping song renders once through, plus a second of its loop.)
    let limit = loop_secs.map_or(60.0 * 30.0, |_| secs + 1.0);
    let mut lens = mml::Lengths::new();
    let mut plain: Vec<_> = song.tracks.iter().map(|t| arrange::tokens(t, None, &lens, None, false)).collect();
    let mut plain_mml = mml::plain(&song.header, &plain, &lens);
    // The volume estimate can't know how loud SiON's waveforms and FM voices really come out: if the result
    // would clip, turn the master volume down until it doesn't.
    // (Rendered samples are limited to ±1, so a clipping peak only says "too loud": step down until it fits.)
    if opt.gain.is_none() {
        for _ in 0..8 {
            let (peak, x) = (peak(&plain_mml, limit), song.master());
            if peak <= 0.98 || x <= 1 { break }
            song.set_master(((x as f32 * if peak >= 0.999 { 0.75 } else { 0.95 / peak }) as i32).min(x - 1));
            plain = song.tracks.iter().map(|t| arrange::tokens(t, None, &lens, None, false)).collect();
            plain_mml = mml::plain(&song.header, &plain, &lens);
        }
    }
    // (The plain MML keeps the data as it was made, so a check against it covers the compacted data too.)
    // (The written MML plays the notes' per-frame runs from tables, the plain one as slurs: checked equal.)
    let env = song.envelopes(false);
    if !env.header.is_empty() {
        plain = song.tracks.iter().enumerate().map(|(i, t)| arrange::tokens(t, None, &lens, Some((&env, i)), false)).collect();
        plain_mml = mml::plain(&song.header, &plain, &lens);
    }
    let mut written = |env: &arrange::Envelopes| -> String {
        let header: Vec<String> = song.header.iter().chain(&env.header).map(|h| mml::compact(h)).collect();
        let toks_of = |gate: Option<u32>| -> Vec<Vec<mml::Tok>> { song.tracks.iter().enumerate().map(|(i, t)| arrange::tokens(t, gate, &lens, Some((env, i)), true)).collect() };
        if score.sections.is_empty() {
            let toks = toks_of(Some(0));
            return mml::minify(&header, &toks, &mut lens);
        }
        // (Sources that key notes off before the next one: gate times rather than a rest after every note.)
        let toks = toks_of(score.gates.then_some(GATE_STEADY));
        let tracks: Vec<(String, &[_])> = song.tracks.iter().zip(&toks).enumerate().map(|(i, (t, toks))| {
            let name = score.names.get(t.ch as usize).filter(|n| !n.is_empty()).map_or(String::new(), |n| format!(", {n}"));
            (format!("Track {}{name}", i + 1), &toks[..])
        }).collect();
        pretty::write(&score.title, &header, &tracks, &song.sections, song.lp, score.vmax(), song.frame(), &mut lens)
    };
    // (Notes keeping the table before them across lines read better, but can stop lines from repeating: the
    // shorter of the two.)
    let mut mml = written(&env);
    if !env.header.is_empty() {
        let other = written(&song.envelopes(true));
        if other.len() < mml.len() { mml = other }
    }
    Converted {
        plain: plain_mml,
        mml,
        tracks: song.tracks.len(),
        notes: song.notes,
        secs,
        loop_secs,
        warnings,
    }
}

/// What a change of gate (`q`, `@q`) costs in a readable track, over its characters: each line restates it.
const GATE_STEADY: u32 = 6;

/// Render MML in chunks of interleaved stereo samples, passing each to `f`, until the song ends or `secs`
/// seconds are rendered (the last chunk may reach past that).
pub fn render_chunks(mml: &str, secs: f64, f: impl FnMut(&[f32])) { render_song(sion::Song::new(mml), secs, f) }

/// `render_chunks`, for a song already made.
pub fn render_song(mut song: sion::Song, secs: f64, mut f: impl FnMut(&[f32])) {
    let (mut buf, mut frames) = (vec![0f32; 8192], 0usize);
    loop {
        buf.fill(0.0);
        let more = song.render(&mut buf);
        f(&buf);
        frames += buf.len() / 2;
        if !more || frames as f64 >= sion::RATE as f64 * secs { return }
    }
}

/// Render MML (at most `secs` seconds, see `render_chunks`).
pub fn render(mml: &str, secs: f64) -> Vec<f32> {
    let mut out = vec![];
    render_chunks(mml, secs, |b| out.extend_from_slice(b));
    out
}

/// `render` without declicking: SiON's output exactly, to compare with a mix that doesn't ramp either (the
/// tracker or chip reference).
pub fn render_raw(mml: &str, secs: f64) -> Vec<f32> {
    let (mut song, mut out) = (sion::Song::new(mml), vec![]);
    song.set_declick(false);
    render_song(song, secs, |b| out.extend_from_slice(b));
    out
}

/// Loudest sample of the rendered MML (at most `secs` seconds).
fn peak(mml: &str, secs: f64) -> f32 {
    let mut peak = 0f32;
    render_chunks(mml, secs, |b| peak = b.iter().fold(peak, |p, s| p.max(s.abs())));
    peak
}

#[cfg(test)]
mod tests;
