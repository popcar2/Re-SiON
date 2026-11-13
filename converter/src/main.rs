//! sion-convert: MIDI, tracker modules (MOD/S3M/XM/IT), VGM/VGZ, NSF/NSFE and SPC → SiON MML, written as by hand.
//!
//! Usage: sion-convert <input> [-o output.mml] [--grid N] [--gain G] [--wavetables] [--exact] [--exact-pcm] [--track N] [--plain] [--verify] [--wav FILE]

use std::process::exit;

const HELP: &str = "usage: sion-convert <input> [options]
       sion-convert <song.mml> --wav FILE   (render an MML file)

  input: Standard MIDI (.mid), tracker module (.mod .s3m .xm .it), chip log (.vgm .vgz) or NES music
         (.nsf .nsfe) or SNES music (.spc);
         the format is recognized from the file's contents

  -o, --output FILE  where to write the MML (default: input name with .mml)
  --grid N           snap timing to 1/N notes (smaller output; default keeps exact timing)
  --gain G           volume scale (default: automatic, avoids clipping chords)
  --wavetables       tracker modules: approximate samples with wavetables and noise instead of embedding them
                     (much smaller output, but only chiptune samples come out close)
  --exact            tracker modules: convert tick by tick, exactly (every volume, pan and pitch step, sample
                     positions), minified; by default the song is written as by hand: per row, laid out by
                     its patterns, repeats as loops and macros
  --track N          NSF/NSFE: which song of the file to convert (from 1; default: the file's first song)
  --exact-pcm        VGM/NSF: capture samples exactly as the chips output them (timing jitter and all) instead of
                     storing each sound once (many times larger)
  --plain            write the plain MML (one command per event) instead
  --verify           render both versions with the sion crate and check the audio is identical; for a
                     tracker module with --exact, also compare with the module mixed straight from its samples
  --wav FILE         also render the MML to a WAV file (to listen to the conversion)
";

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut input, mut output, mut opt, mut plain, mut verify, mut wav) = (None, None, sion_converter::Options::default(), false, false, None);
    let fail = |msg: &str| -> ! { eprintln!("{msg}"); exit(1) };
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().unwrap_or_else(|| fail(&format!("{name} needs a value")));
        match a.as_str() {
            "-h" | "--help" => { print!("{HELP}"); return }
            "-o" | "--output" => output = Some(val("--output")),
            "--grid" => opt.grid = Some(val("--grid").parse().unwrap_or_else(|_| fail("--grid takes a number, e.g. 64"))),
            "--gain" => opt.gain = Some(val("--gain").parse().unwrap_or_else(|_| fail("--gain takes a number, e.g. 1.5"))),
            "--wavetables" => opt.wavetables = true,
            "--exact-pcm" => opt.exact_pcm = true,
            "--exact" => opt.exact = true,
            "--track" => opt.track = Some(val("--track").parse().unwrap_or_else(|_| fail("--track takes a number, e.g. 2"))),
            "--plain" => plain = true,
            "--verify" => verify = true,
            "--wav" => wav = Some(val("--wav")),
            _ if input.is_none() && !a.starts_with('-') => input = Some(a),
            _ => fail(&format!("unknown argument {a}\n\n{HELP}")),
        }
    }
    let Some(input) = input else { fail(HELP) };
    let data = std::fs::read(&input).unwrap_or_else(|e| fail(&format!("cannot read {input}: {e}")));
    if input.ends_with(".mml") || input.ends_with(".sionmml") {
        // MML input: just render it (for listening to an existing song).
        let Some(w) = wav else { fail("an .mml input needs --wav FILE") };
        // (A song with a `$` loop point never ends: render its first 5 minutes.)
        let mml = String::from_utf8_lossy(&data);
        return write_wav(&w, &mml, if mml.contains('$') { 60.0 * 5.0 } else { 60.0 * 30.0 }).unwrap_or_else(|e| fail(&format!("cannot write {w}: {e}")));
    }
    let out = sion_converter::convert(&data, &opt).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
    for w in &out.warnings { eprintln!("{input}: note: {w}") }
    let path = output.unwrap_or_else(|| std::path::Path::new(&input).with_extension("mml").to_string_lossy().into_owned());
    std::fs::write(&path, if plain { &out.plain } else { &out.mml }).unwrap_or_else(|e| fail(&format!("cannot write {path}: {e}")));
    println!("{path}: {} tracks, {} notes, {} bytes (plain {} bytes, {:.1}%){}",
        out.tracks, out.notes, out.mml.len(), out.plain.len(), 100.0 * out.mml.len() as f64 / out.plain.len().max(1) as f64,
        out.loop_secs.map_or(String::new(), |l| format!(", loops the last {l:.1}s")));
    // A looping song: the loop played twice (and checked across the jump back), as a VGM player does.
    let secs = out.loop_secs.map_or(60.0 * 30.0, |l| out.secs + l);
    if let Some(w) = wav { write_wav(&w, &out.mml, secs).unwrap_or_else(|e| fail(&format!("cannot write {w}: {e}"))) }
    if verify && !check(&out.plain, &out.mml, secs) { exit(2) }
    if verify && opt.exact && !opt.wavetables && sion_converter::detect(&data) == Some(sion_converter::Format::Module) {
        let want = sion_converter::reference(&data).unwrap_or_else(|e| fail(&format!("{input}: {e}")));
        let got = render(&out.mml, want.len() / 2);
        let db = sion_converter::snr(&want, &got);
        println!("verify: {db:.1} dB signal-to-difference against the module mixed from its samples{}",
            if db >= 40.0 { "" } else { " (low: see the notes above for what the conversion left out)" });
    }
}

/// Render MML: at least `frames` frames (silence past the song's end), at most 30 minutes.
fn render(mml: &str, frames: usize) -> Vec<f32> {
    let (mut song, mut buf, mut out, mut more) = (sion::Song::new(mml), vec![0f32; 8192], vec![], true);
    while (more || out.len() < frames * 2) && out.len() < sion::RATE as usize * 2 * 60 * 30 {
        buf.fill(0.0);
        if more { more = song.render(&mut buf) }
        out.extend_from_slice(&buf);
    }
    out
}

/// Render MML to a 16-bit stereo WAV file (at most `secs` seconds).
fn write_wav(path: &str, mml: &str, secs: f64) -> std::io::Result<()> {
    let (mut song, mut buf, mut pcm) = (sion::Song::new(mml), vec![0f32; 8192], Vec::<u8>::new());
    let rate = sion::RATE as u32;
    loop {
        buf.fill(0.0);
        let more = song.render(&mut buf);
        for s in &buf { pcm.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()) }
        if !more || pcm.len() as f64 >= rate as f64 * 4.0 * secs { break }
    }
    let secs = pcm.len() as f64 / 4.0 / rate as f64;
    let mut f = Vec::with_capacity(44 + pcm.len());
    for (tag, v) in [(&b"RIFF"[..], 36 + pcm.len() as u32)] { f.extend(tag); f.extend(v.to_le_bytes()) }
    f.extend(b"WAVEfmt ");
    for v in [16u32, 1 | 2 << 16, rate, rate * 4, 4 | 16 << 16] { f.extend(v.to_le_bytes()) }
    f.extend(b"data");
    f.extend((pcm.len() as u32).to_le_bytes());
    f.extend(pcm);
    std::fs::write(path, f)?;
    println!("{path}: {secs:.1}s of audio");
    Ok(())
}

/// Render both MML texts side by side (at most `secs` seconds) and compare every sample.
fn check(a: &str, b: &str, secs: f64) -> bool {
    let (mut x, mut y) = (sion::Song::new(a), sion::Song::new(b));
    let (mut bx, mut by) = (vec![0f32; 8192], vec![0f32; 8192]);
    let (mut ax, mut ay, mut frames, mut diff, mut first, mut peak, mut clip) = (true, true, 0u64, 0f32, None, 0f32, 0u64);
    let limit = (sion::RATE as f64 * secs) as u64;
    while (ax || ay) && frames < limit {
        bx.fill(0.0);
        by.fill(0.0);
        if ax { ax = x.render(&mut bx) }
        if ay { ay = y.render(&mut by) }
        for (i, (p, q)) in bx.iter().zip(&by).enumerate() {
            let d = (p - q).abs();
            if d > 0.0 && first.is_none() { first = Some(frames + i as u64 / 2) }
            diff = diff.max(d);
            peak = peak.max(q.abs());
            clip += (q.abs() >= 1.0) as u64;
        }
        frames += bx.len() as u64 / 2;
    }
    let secs = frames as f64 / sion::RATE as f64;
    println!("verify: {:.1}s rendered, peak {:.3}{}", secs, peak, if clip > 0 { format!(", {clip} clipped samples") } else { String::new() });
    match first {
        None => { println!("verify: audio is identical to the plain conversion"); true }
        Some(f) => { println!("verify: MISMATCH from {:.3}s (max difference {diff})", f as f64 / sion::RATE as f64); false }
    }
}
