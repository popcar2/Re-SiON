//! End-to-end checks of the non-MIDI readers on small files built here: the right notes come out, and the
//! written MML sounds identical to the plain one.

use sion_converter::{convert, Options};

/// Render MML and return its samples (at most `secs` seconds).
fn render(mml: &str, secs: usize) -> Vec<f32> {
    let (mut song, mut buf, mut out) = (sion::Song::new(mml), vec![0f32; 4096], vec![]);
    while out.len() < sion::RATE as usize * 2 * secs {
        buf.fill(0.0);
        let more = song.render(&mut buf);
        out.extend_from_slice(&buf);
        if !more { break }
    }
    out
}

fn check(data: &[u8]) -> sion_converter::Converted { check_with(data, &Options::default()) }

fn check_with(data: &[u8], opt: &Options) -> sion_converter::Converted {
    let out = convert(data, opt).unwrap();
    assert_eq!(render(&out.plain, 60), render(&out.mml, 60), "written MML must sound like the plain conversion");
    out
}

/// A VGM 1.71 file: header clock fields, then the commands.
fn vgm(clocks: &[(usize, u32)], body: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 0x100];
    h[..4].copy_from_slice(b"Vgm ");
    h[8..12].copy_from_slice(&0x171u32.to_le_bytes());
    h[0x34..0x38].copy_from_slice(&(0x100u32 - 0x34).to_le_bytes());
    for &(o, v) in clocks { h[o..o + 4].copy_from_slice(&v.to_le_bytes()) }
    h.extend_from_slice(body);
    h.push(0x66);
    let n = h.len() as u32 - 4;
    h[4..8].copy_from_slice(&n.to_le_bytes());
    h
}

const SCALE: [f64; 5] = [261.63, 329.63, 392.0, 440.0, 523.25];
const WAIT: u8 = 0x62; // one 60 Hz frame

fn frames(b: &mut Vec<u8>, n: usize) { b.extend(std::iter::repeat_n(WAIT, n)) }

/// The notes (with octaves) of the first track of the plain MML.
fn notes(plain: &str, track: usize) -> Vec<String> {
    let t = plain.split(";\n").filter(|t| !t.starts_with('#')).nth(track).unwrap();
    let (mut out, b) = (vec![], t.as_bytes());
    for i in 0..b.len() {
        if b[i] == b'o' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            let mut j = i + 2;
            while b.get(j).is_some_and(u8::is_ascii_digit) { j += 1 }
            let mut n = String::from_utf8(b[i + 1..j + 1].to_vec()).unwrap();
            if matches!(b.get(j + 1), Some(b'+')) { n.push('+') }
            out.push(n);
        }
    }
    out
}

#[test]
fn nes_apu() {
    let mut b = vec![0xb4, 0x15, 0x0f];
    for f in SCALE {
        let t = (1789773.0 / (16.0 * f)).round() as u32 - 1;
        b.extend([0xb4, 0, 0xbf, 0xb4, 2, t as u8, 0xb4, 3, (t >> 8) as u8 | 0xf8]);
        frames(&mut b, 12);
        b.extend([0xb4, 0, 0xb0]);
        frames(&mut b, 12);
    }
    let out = check(&vgm(&[(0x84, 1789772)], &b));
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "5a", "6c"]);
    assert!(out.plain.contains("%1@4"), "50% pulse");
}

#[test]
fn sn76489_and_noise() {
    let mut b = vec![];
    for f in SCALE {
        let n = (3579545.0 / (32.0 * f)).round() as u32;
        b.extend([0x50, 0x80 | (n & 15) as u8, 0x50, (n >> 4) as u8 & 0x3f, 0x50, 0x90]);
        frames(&mut b, 12);
        b.extend([0x50, 0x9f]);
        frames(&mut b, 12);
    }
    b.extend([0x50, 0xe4, 0x50, 0xf0]);
    frames(&mut b, 24);
    let out = check(&vgm(&[(0x0c, 3579545)], &b));
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "5a", "6c"]);
    assert!(out.plain.contains("%2@1"), "white noise");
}

#[test]
fn game_boy_wave_channel() {
    let mut b = vec![0xb3, 0x16, 0x80, 0xb3, 0x14, 0x77, 0xb3, 0x15, 0xff];
    for k in 0..16u8 { b.extend([0xb3, 0x20 + k, (k << 4) | (15 - k)]) }
    let x = (2048.0 - 65536.0 / 220.0f64).round() as u32;
    b.extend([0xb3, 0x0a, 0x80, 0xb3, 0x0c, 0x20, 0xb3, 0x0d, x as u8, 0xb3, 0x0e, 0x80 | (x >> 8) as u8]);
    frames(&mut b, 30);
    let out = check(&vgm(&[(0x80, 4194304)], &b));
    assert!(out.plain.contains("#WAV0{"), "the wave RAM becomes a wavetable");
    assert_eq!(notes(&out.plain, 0), ["4a"]);
}

#[test]
fn ym2151_voice() {
    let mut b = vec![0x54, 0x20, 0xc7];
    for s in 0..4u8 {
        for (reg, v) in [(0x40, 1), (0x60, if s == 0 { 0x10 } else { 0x7f }), (0x80, 0x1f), (0xa0, 0), (0xc0, 0), (0xe0, 0x0f)] { b.extend([0x54, reg + s * 8, v]) }
    }
    for semis in [-9i32, -5, -2, 0, 3] {
        let rel = 69 + semis - 61;
        let code = [0, 1, 2, 4, 5, 6, 8, 9, 10, 12, 13, 14][rel.rem_euclid(12) as usize];
        b.extend([0x54, 0x28, ((4 + rel.div_euclid(12)) << 4) as u8 | code, 0x54, 0x08, 0x78]);
        frames(&mut b, 12);
        b.extend([0x54, 0x08, 0]);
        frames(&mut b, 12);
    }
    let out = check(&vgm(&[(0x30, 3579545)], &b));
    assert!(out.plain.contains("#OPM@0{7,0,"), "an FM voice from the patch registers");
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "5a", "6c"]);
}

/// A 4-channel ProTracker module: sample 1 is a looped 32-frame square wave; one pattern plays
/// C-2 E-2 G-2 C-3, then silences it.
fn protracker() -> Vec<u8> {
    let mut d = vec![0u8; 1084];
    d[..4].copy_from_slice(b"test");
    let s = 20;
    d[s + 22..s + 24].copy_from_slice(&16u16.to_be_bytes()); // 32 bytes
    d[s + 25] = 64;
    d[s + 28..s + 30].copy_from_slice(&16u16.to_be_bytes()); // loop all of it
    d[950] = 1;
    d[1080..1084].copy_from_slice(b"M.K.");
    let mut pat = vec![0u8; 64 * 4 * 4];
    for (row, period) in [(0, 428u16), (8, 339), (16, 285), (24, 214)] {
        let c = &mut pat[row * 16..row * 16 + 4];
        c[0] = (period >> 8) as u8;
        c[1] = period as u8;
        c[2] = 0x10; // sample 1
    }
    pat[32 * 16 + 2..32 * 16 + 4].copy_from_slice(&[0x0c, 0x00]); // C00: silence
    d.extend(pat);
    d.extend((0..32).map(|i| if i < 16 { 100u8 } else { 156 }));
    d
}

#[test]
fn mod_square_wave() {
    let opt = Options { wavetables: true, ..Options::default() };
    let out = convert(&protracker(), &opt).unwrap();
    assert_eq!(render(&out.plain, 60), render(&out.mml, 60), "written MML must sound like the plain conversion");
    // A 32-frame cycle played at 3546895 / 428 Hz is 259 Hz: just under middle C.
    assert!(out.plain.contains("#WAV0{"), "the looped sample becomes a wavetable");
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "6c"]);
}

#[test]
fn mod_sample_plays_as_pcm() {
    let out = check_with(&protracker(), &Options { exact: true, ..Options::default() });
    assert!(out.plain.contains("#PCMB0{") && out.plain.contains("%7@0"), "the sample is embedded and played with %7");
    let track = out.plain.split(";\n").find(|t| t.contains("%7")).unwrap();
    assert!(track.contains("o5c2o5e2o5g2o6c2&v0o6c92r"), "C-2 E-2 G-2 C-3, then off a tick after C00: {track}");
    let want = sion_converter::reference(&protracker()).unwrap();
    let db = sion_converter::snr(&want, &render(&out.mml, 60));
    assert!(db > 50.0, "conversion differs from the tracker mix: {db:.1} dB");
}

#[test]
fn mod_written_as_by_hand() {
    // By default a module is laid out by its patterns: a comment per track, a single-cycle loop as a wavetable,
    // and the notes as written (C-2 E-2 G-2 C-3, half notes, then silence from the C00).
    let out = check(&protracker());
    println!("{}\n{}", out.mml, out.plain);
    assert!(out.mml.contains("// Track 1, channel 1") && out.mml.contains("#WAV0{"), "{}", out.mml);
    assert!(out.mml.contains("o5c e g <c"), "{}", out.mml);
}

/// A format 0 MIDI file at 96 ticks per quarter: (delta ticks, event bytes).
fn smf(events: &[(u32, &[u8])]) -> Vec<u8> {
    let mut trk = vec![];
    for &(dt, ev) in events.iter().chain(&[(0, &[0xff, 0x2f, 0][..])]) {
        let mut v = vec![(dt & 0x7f) as u8];
        let mut d = dt >> 7;
        while d > 0 { v.insert(0, (d & 0x7f) as u8 | 0x80); d >>= 7 }
        trk.extend(v);
        trk.extend_from_slice(ev);
    }
    let mut f = b"MThd\0\0\0\x06\0\0\0\x01\0\x60MTrk".to_vec();
    f.extend((trk.len() as u32).to_be_bytes());
    f.extend(trk);
    f
}

#[test]
fn midi_written_by_bars() {
    // 3/4: a bar of three quarter notes, then a note held for two bars; a swept kick (pitch table) and a snare
    // (keyed off at once) each held across bar lines until the next hit.
    let q = 96;
    let out = check(&smf(&[
        (0, &[0xff, 0x58, 4, 3, 2, 24, 8]),
        (0, &[0x90, 60, 100]), (0, &[0x99, 36, 100]), (q, &[0x80, 60, 0]),
        (0, &[0x90, 62, 100]), (0, &[0x99, 38, 100]), (q, &[0x80, 62, 0]),
        (0, &[0x90, 64, 100]), (q, &[0x80, 64, 0]),
        (0, &[0x90, 65, 100]), (6 * q, &[0x80, 65, 0]),
        (0, &[0x99, 36, 100]), (0, &[0x99, 38, 100]), (q, &[0x89, 36, 0]),
    ]));
    println!("{}\n{}", out.mml, out.plain);
    // One line per 3/4 bar: the held note slurred across the bar lines.
    assert!(out.mml.contains("// Track 1, channel 1\n") && out.mml.contains("// Track 2, drums: kick"), "{}", out.mml);
    assert!(out.mml.contains("\no5c d e\no5f2.&\no5f2.;"), "{}", out.mml);
    // The drum hits aren't cut at the bar lines: their lines run on to the next hit.
    assert!(out.mml.contains("np0\no4a1^1^4\no4a;") && out.mml.contains("\nr o6c1^1\no6c;"), "{}", out.mml);
}

/// Signal-to-difference (dB) of SiON's left channel against `want` (mono, in VGM time), up to overall level.
/// The MML's timeline is offset from the VGM's (it starts at the converter's frame phase, after a lead-in that
/// lines up its rows): align the first sound, then try a few frames either way.
fn match_db(mml: &str, want: &[f64]) -> f64 {
    // (Against the chips' own output: without the Mega Drive's master shelf, which matches a player's.)
    let mml = &mml.replace("#EFFECT0{hb9000,1,-3};", "");
    let got: Vec<f32> = render(mml, 60).iter().step_by(2).copied().collect();
    let want: Vec<f32> = want.iter().map(|v| *v as f32).collect();
    let first = |x: &[f32]| x.iter().position(|v| v.abs() > 1e-6).unwrap_or(0) as i64;
    let lag = first(&got) - first(&want);
    (lag - 4..=lag + 4).map(|l| {
        let (w, g) = if l >= 0 { (&want[..], &got[(l as usize).min(got.len())..]) } else { (&want[((-l) as usize).min(want.len())..], &got[..]) };
        sion_converter::snr(w, g)
    }).fold(f64::NEG_INFINITY, f64::max)
}

/// Add a sample's sound to `out`: `frames` played from VGM time `at` at `rate` frames per second, linearly
/// interpolated (at 44100 this is the frames themselves, held: a DAC's output).
fn mix(out: &mut Vec<f64>, at: f64, rate: f64, frames: &[f64]) {
    let n = (frames.len() as f64 * 44100.0 / rate).ceil() as usize + 2;
    if out.len() < at as usize + n + 1 { out.resize(at as usize + n + 1, 0.0) }
    for i in (at.ceil() as usize)..(at as usize + n) {
        let p = (i as f64 - at) * rate / 44100.0;
        let (k, f) = (p as usize, p.fract());
        let v = |k: usize| frames.get(k).copied().unwrap_or(0.0);
        out[i] += v(k) + (v(k + 1) - v(k)) * f;
    }
}

fn block(ty: u8, body: &[u8]) -> Vec<u8> { let mut b = vec![0x67, 0x66, ty]; b.extend((body.len() as u32).to_le_bytes()); b.extend(body); b }

fn wait(b: &mut Vec<u8>, n: u16) { b.extend([0x61, n as u8, (n >> 8) as u8]) }

const EXACT: Options = Options { grid: None, gain: None, wavetables: false, exact: false, exact_pcm: true, track: None };

/// Convert both ways: (default, `--exact-pcm`).
fn both(data: &[u8]) -> (sion_converter::Converted, sion_converter::Converted) { (check(data), check_with(data, &EXACT)) }

#[test]
fn ym2612_dac_samples() {
    // A 300-byte drum in the data bank, played twice from it (0x8n: write, then wait 2), then 100 of its bytes
    // written directly every 3 frames.
    let drum: Vec<u8> = (0..300).map(|i| if i == 299 { 128 } else { (128.0 + 100.0 * (i as f64 * 0.3).sin() * (-(i as f64) / 120.0).exp()) as u8 }).collect();
    let f = |v: u8| (v as f64 - 128.0) / 128.0;
    let mut b = block(0, &drum);
    b.extend([0x52, 0x2b, 0x80]);
    let (mut held, mut played, mut t) = (vec![], vec![], 0u64);
    for _ in 0..2 {
        wait(&mut b, 5000);
        t += 5000;
        b.extend([0xe0, 0, 0, 0, 0]);
        b.extend(std::iter::repeat_n(0x82, drum.len()));
        // The DAC holds each byte for its 2 frames; stored once, the drum plays its bytes at 22050 Hz.
        mix(&mut held, t as f64, 44100.0, &drum.iter().flat_map(|&v| [f(v); 2]).collect::<Vec<_>>());
        mix(&mut played, t as f64, 22050.0, &drum.iter().map(|&v| f(v)).collect::<Vec<_>>());
        t += 600;
    }
    wait(&mut b, 7000);
    t += 7000;
    for &v in &drum[..100] { b.extend([0x52, 0x2a, v]); wait(&mut b, 3) }
    b.extend([0x52, 0x2a, 128]);
    mix(&mut held, t as f64, 44100.0, &drum[..100].iter().flat_map(|&v| [f(v); 3]).collect::<Vec<_>>());
    mix(&mut played, t as f64, 14700.0, &drum[..100].iter().map(|&v| f(v)).collect::<Vec<_>>());
    wait(&mut b, 4000);
    let (out, exact) = both(&vgm(&[(0x2c, 7670453)], &b));
    assert_eq!(out.plain.matches("#PCMB").count(), 2, "the bank's drum (stored once) and the directly written bytes");
    assert!(out.plain.contains("}22050,60,"), "the drum at the rate the driver plays it");
    assert_eq!(out.plain.matches("o5c").count(), 3, "three sample notes");
    let (db, xdb) = (match_db(&out.mml, &played), match_db(&exact.mml, &held));
    println!("{db:.1} dB, exact {xdb:.1} dB");
    assert!(db > 50.0, "each byte once, at the driver's rate: {db:.1} dB");
    assert!(xdb > 60.0, "--exact-pcm: the DAC output as written: {xdb:.1} dB");
}

#[test]
fn ym2612_dac_stream() {
    // The same drum through the DAC stream commands at 22050 Hz (a byte every 2 frames), twice: by start offset
    // (0x93) and by block (0x95); the second stops early (0x94).
    let drum: Vec<u8> = (0..300).map(|i| if i == 299 { 128 } else { (128.0 + 90.0 * (i as f64 * 0.25).sin()) as u8 }).collect();
    let f = |v: u8| (v as f64 - 128.0) / 128.0;
    let mut b = block(0, &drum);
    b.extend([0x52, 0x2b, 0x80]);
    b.extend([0x90, 0, 0x02, 0, 0x2a, 0x91, 0, 0x00, 1, 0, 0x92, 0]);
    b.extend(22050u32.to_le_bytes());
    let (mut held, mut played) = (vec![], vec![]);
    let frames: Vec<f64> = drum.iter().flat_map(|&v| [f(v); 2]).collect();
    let bytes: Vec<f64> = drum.iter().map(|&v| f(v)).collect();
    wait(&mut b, 3000);
    b.extend([0x93, 0, 0, 0, 0, 0, 0x01]);
    b.extend((drum.len() as u32).to_le_bytes());
    mix(&mut held, 3000.0, 44100.0, &frames);
    mix(&mut played, 3000.0, 22050.0, &bytes);
    wait(&mut b, 6000);
    b.extend([0x95, 0, 0, 0, 0]);
    wait(&mut b, 200);
    b.extend([0x94, 0]);
    // (Stopped mid-sample, the DAC holds its last value: exactly, it is kept for one frame, then the DC offset
    // that follows is left out; stored once, the note ends a byte after the last one written.)
    mix(&mut held, 9000.0, 44100.0, &frames[..199]);
    mix(&mut played, 9000.0, 22050.0, &bytes[..100]);
    wait(&mut b, 3000);
    let (out, exact) = both(&vgm(&[(0x2c, 7670453)], &b));
    assert_eq!(out.plain.matches("o5c").count(), 2, "two streamed hits");
    assert!(out.plain.split("@ph").skip(1).any(|p| p.split('o').next().unwrap().contains(',')), "the second plays just its frames (`@ph start,length`)");
    assert_eq!(out.plain.matches("#PCMB").count(), 1, "one sample: the bank");
    let (db, xdb) = (match_db(&out.mml, &played), match_db(&exact.mml, &held));
    println!("{db:.1} dB, exact {xdb:.1} dB");
    assert!(db > 45.0, "the streamed bytes, once each (the cut one stopping on its frame): {db:.1} dB");
    assert!(xdb > 50.0, "--exact-pcm: the streamed DAC output as written: {xdb:.1} dB");
}

/// Set `level` (the DMC's output, which holds between samples) to `frames` played from VGM time `at` at `rate`
/// frames per second, linearly interpolated, holding the level up to there.
fn play_level(level: &mut Vec<f64>, at: f64, rate: f64, frames: &[f64]) {
    let n = (frames.len() as f64 * 44100.0 / rate).ceil() as usize + 2;
    let last = level.last().copied().unwrap_or(0.0);
    level.resize(at as usize + n + 1, last);
    for i in (at.ceil() as usize)..=(at as usize + n) {
        let p = (i as f64 - at) * rate / 44100.0;
        let (k, f) = (p as usize, p.fract());
        let v = |k: usize| frames.get(k).or(frames.last()).copied().unwrap_or(0.0);
        level[i] = v(k) + (v(k + 1) - v(k)) * f;
    }
}

/// The DC blocker a DPCM channel's level goes through (first order, 60 Hz).
fn dc_block(x: &[f64]) -> Vec<f64> {
    let a = 1.0 / (1.0 + std::f64::consts::TAU * 60.0 / 44100.0);
    let (mut y, mut prev) = (0.0, 0.0);
    x.iter().map(|&v| { y = a * (y + v - prev); prev = v; y }).collect()
}

#[test]
fn nes_noise_mode_change_mid_note() {
    // Long-mode noise hits every 6 frames from the fifth row; the last switches to short mode two frames in, without a new key-on:
    // one slurred note that swaps to `@10` there. A DPCM sample in the song makes the score count in SiON ticks
    // (`sub` per frame), where the swap must land too, not at the note's start.
    let mut b = block(0xc2, &[0x00, 0xc0, 0x55, 0xaa]);
    b.extend([0xb4, 0x10, 0x0f, 0xb4, 0x12, 0x00, 0xb4, 0x13, 0x00, 0xb4, 0x0c, 0x3f, 0xb4, 0x0e, 0x05]);
    for i in 0..8 {
        // (Pulse notes every row, so the score finds the rows.)
        b.extend([0xb4, 0x15, if i == 0 { 0x11 } else if i < 4 { 0x01 } else { 0x09 }, 0xb4, 0x00, 0xbf, 0xb4, 0x02, 0x80 + i * 8, 0xb4, 0x03, 0x01]);
        if i >= 4 { b.extend([0xb4, 0x0f, 0x08]) }
        if i == 7 { wait(&mut b, 735 * 2); b.extend([0xb4, 0x0e, 0x85]); wait(&mut b, 735 * 4) } else { wait(&mut b, 735 * 6) }
    }
    b.extend([0xb4, 0x15, 0x00]);
    wait(&mut b, 735 * 6);
    let out = check(&vgm(&[(0x84, 1789773)], &b));
    assert!(out.plain.contains("%7"), "the DPCM sample: {}", out.plain);
    let noise = out.plain.lines().find(|l| l.contains("%1@9")).unwrap_or_else(|| panic!("long noise: {}", out.plain));
    let (long, short) = (noise.rfind("%1@9").unwrap(), noise.find("@10").unwrap_or_else(|| panic!("short noise: {noise}")));
    assert!(short > long && noise[long..short].contains('&'), "short mode only after the slur: {noise}");
}

#[test]
fn nes_dpcm_and_raw_pcm() {
    // A 17-byte DPCM sample at $C000 played twice at the top rate, then levels written to $4011 every 3 frames.
    // The DMC's level (64, from $4011) holds between the samples; the channel playing them takes its DC out.
    let bits: Vec<u8> = (0..17u8).map(|i| [0x0f, 0xf0, 0x33, 0xcc, 0xaa, 0x55, 0x3c][i as usize % 7]).collect();
    let mut body = vec![0x00, 0xc0];
    body.extend(&bits);
    let mut b = block(0xc2, &body);
    b.extend([0xb4, 0x15, 0x00, 0xb4, 0x11, 64, 0xb4, 0x10, 0x0f, 0xb4, 0x12, 0x00, 0xb4, 0x13, 0x01]);
    let rate = 1789773.0 / 54.0;
    let (mut held, mut played, mut dmc, mut t) = (vec![], vec![], vec![], 0u64);
    for _ in 0..2 {
        wait(&mut b, 3000);
        t += 3000;
        b.extend([0xb4, 0x15, 0x10]);
        let (mut l, mut frames) = (64i32, vec![64.0]);
        for byte in &bits { for k in 0..8 { if byte >> k & 1 != 0 { if l <= 125 { l += 2 } } else if l >= 2 { l -= 2 } frames.push(l as f64) } }
        play_level(&mut dmc, t as f64, rate, &frames);
        assert_eq!(l, 64);
        wait(&mut b, 2000);
        t += 2000;
        b.extend([0xb4, 0x15, 0x00]);
    }
    wait(&mut b, 3000);
    t += 3000;
    let levels: Vec<f64> = (0..200).map(|k| ((k as f64 * 0.2).sin() * 40.0).trunc()).collect();
    for &v in &levels { b.extend([0xb4, 0x11, (64 + v as i32) as u8]); wait(&mut b, 3) }
    b.extend([0xb4, 0x11, 64]);
    mix(&mut held, t as f64, 44100.0, &levels.iter().flat_map(|&v| [v; 3]).collect::<Vec<_>>());
    mix(&mut played, t as f64, 14700.0, &levels);
    wait(&mut b, 4000);
    dmc.resize(held.len().max(played.len()) + 4000, 64.0);
    for (i, v) in dc_block(&dmc).into_iter().enumerate() {
        for out in [&mut held, &mut played] { if out.len() <= i { out.resize(i + 1, 0.0) } out[i] += v }
    }
    let (out, exact) = both(&vgm(&[(0x84, 1789773)], &b));
    assert!(out.plain.matches("#PCMB").count() >= 2, "the DPCM sample and the written levels");
    let (db, xdb) = (match_db(&out.mml, &played), match_db(&exact.mml, &held));
    println!("{db:.1} dB, exact {xdb:.1} dB");
    assert!(db > 50.0, "DPCM and $4011 levels, once each: {db:.1} dB");
    assert!(xdb > 50.0, "--exact-pcm: DPCM and $4011 levels as output: {xdb:.1} dB");
}

#[test]
fn ym2610_adpcm() {
    // ADPCM-A: one 256-byte unit of nibbles 0 (each adds the smallest step: a ramp); ADPCM-B: the same bytes at
    // half rate (delta-N 0x8000), so an octave below its root.
    let rom = vec![0u8; 256];
    let mut rb = (rom.len() as u32).to_le_bytes().to_vec();
    rb.extend(0u32.to_le_bytes());
    rb.extend(&rom);
    let mut b = block(0x82, &rb);
    b.extend(block(0x83, &rb));
    for (reg, v) in [(0x01, 0x3f), (0x08, 0xdf), (0x10, 0), (0x18, 0), (0x20, 0), (0x28, 0)] { b.extend([0x59, reg, v]) }
    wait(&mut b, 2000);
    b.extend([0x59, 0x00, 0x01]);
    wait(&mut b, 6000);
    for (reg, v) in [(0x11, 0xc0), (0x12, 0), (0x13, 0), (0x14, 0), (0x15, 0), (0x19, 0x00), (0x1a, 0x80), (0x1b, 0xff), (0x10, 0x80)] { b.extend([0x58, reg, v]) }
    wait(&mut b, 8000);
    let (out, exact) = both(&vgm(&[(0x4c, 8000000)], &b));
    assert_eq!(out.plain.matches("#PCMB").count(), 2, "an ADPCM-A and an ADPCM-B sample");
    // A: the accumulator steps by 2 per nibble (step 16, (2*0+1)*16/8); B: by 15 ((2*0+1)*127/8), the step
    // staying at its minimum.
    let a: Vec<f64> = (1..=512).map(|k| (2 * k) as f64 / 2048.0 * 15.0 / 4.0).collect();
    let bb: Vec<f64> = (1..=512).map(|k| (15 * k) as f64 / 32768.0 * 255.0 / 64.0).collect();
    let mut want = vec![];
    mix(&mut want, 2000.0, 8000000.0 / 432.0, &a);
    mix(&mut want, 8000.0, 8000000.0 / 144.0 / 2.0, &bb);
    let (db, xdb) = (match_db(&out.mml, &want), match_db(&exact.mml, &want));
    println!("{db:.1} dB, exact {xdb:.1} dB");
    assert!(db > 40.0, "ADPCM in 8 bits, as the chip plays it: {db:.1} dB");
    assert!(xdb > 60.0, "--exact-pcm: ADPCM in 16 bits: {xdb:.1} dB");
    assert!(out.plain.contains("o4c"), "ADPCM-B at half rate plays an octave down");
}

#[test]
fn mega_drive_mix() {
    // The same YM2612 patch (algorithm 7, S1 alone at full level, feedback 3) on channel 1 (centre) and channel 2
    // (left only), and a PSG square at full volume.
    let mut b = vec![0x52, 0x22, 0, 0x52, 0x27, 0, 0x52, 0x2b, 0];
    for ch in 0..2u8 {
        for slot in [0u8, 4, 8, 12] {
            let r = slot + ch;
            b.extend([0x52, 0x30 + r, 1, 0x52, 0x40 + r, if slot == 0 { 0 } else { 127 }, 0x52, 0x50 + r, 31, 0x52, 0x60 + r, 0, 0x52, 0x70 + r, 0, 0x52, 0x80 + r, 15]);
        }
        b.extend([0x52, 0xb0 + ch, 7 | 3 << 3, 0x52, 0xb4 + ch, if ch == 0 { 0xc0 } else { 0x80 }, 0x52, 0xa4 + ch, 0x22, 0x52, 0xa0 + ch, 0x69, 0x52, 0x28, 0xf0 | ch]);
    }
    b.extend([0x50, 0x8e, 0x50, 0x07, 0x50, 0x90]);
    frames(&mut b, 30);
    let out = check(&vgm(&[(0x0c, 3579545), (0x2c, 7670453)], &b));
    // The chips feed back the sum of S1's last two outputs: SiON's level one higher, its loop the chips' way.
    assert!(out.plain.contains("#FBMODE{chip};"), "chip-style feedback");
    assert!(out.plain.contains("#OPN@0{7,4,"), "feedback 3 is SiON's 4");
    let tracks: Vec<&str> = out.plain.split(";\n").filter(|t| !t.starts_with('#')).collect();
    let track = |f: &dyn Fn(&str) -> bool| *tracks.iter().find(|t| f(t)).unwrap();
    // In 0.75 dB steps: the centred FM note at full volume (v128), the one on one side 3 dB down (the chip
    // switches each side on or off; SiON pans at constant power), and the PSG 12 dB under the FM.
    assert!(!track(&|t| t.contains("%6") && !t.contains("@p")).contains('v'), "centred FM at v128");
    assert!(track(&|t| t.contains("@p-64")).contains("v124"), "left-only FM at v124");
    assert!(track(&|t| t.contains("%1@4")).contains("v112"), "PSG at v112");
}

#[test]
fn fast_noise_level() {
    // SN76489 white noise shifting with tone 3 at its fastest (112 kHz) keeps about 37% of its power below
    // 22 kHz, at the slowest rate (clock / 2048) nearly all.
    let mut b = vec![0x50, 0xc1, 0x50, 0x00, 0x50, 0xe7, 0x50, 0xf0];
    frames(&mut b, 30);
    b.extend([0x50, 0xe6, 0x50, 0xf0]);
    frames(&mut b, 30);
    let out = check(&vgm(&[(0x0c, 3579545)], &b));
    let t = out.plain.split(";\n").find(|t| t.contains("%2@1")).unwrap();
    // (Volumes in 0.75 dB steps: the fast noise 4.3 dB down, at v122; the slow one at full, v128.)
    let (fast, slow) = (t.find("v122").expect("fast noise at v122"), t.find("v128").expect("slow noise at v128"));
    assert!(fast < slow, "the fast noise first: {t}");
}

#[test]
fn vgm_loop_point() {
    // SN76489: channel 0 plays C in the intro, then G and A in the loop; channel 1 holds E through the loop point
    // and plays C louder at the end, so the loop has to set its volume back when it goes round.
    let tone = |b: &mut Vec<u8>, c: u8, f: f64, att: u8| {
        let n = (3579545.0 / (32.0 * f)).round() as u32;
        b.extend([0x50, 0x80 | c << 5 | (n & 15) as u8, 0x50, (n >> 4) as u8 & 0x3f, 0x50, 0x90 | c << 5 | att]);
    };
    let mut b = vec![];
    tone(&mut b, 0, SCALE[0], 0);
    frames(&mut b, 6);
    // (Channel 0 off before the loop: a pitch change without a key-on would carry its note on, slurred.)
    b.extend([0x50, 0x9f]);
    tone(&mut b, 1, SCALE[1], 6);
    frames(&mut b, 6);
    let lp = 0x100 + b.len();
    tone(&mut b, 0, SCALE[2], 2);
    frames(&mut b, 6);
    b.extend([0x50, 0xbf]);
    frames(&mut b, 6);
    tone(&mut b, 0, SCALE[3], 4);
    tone(&mut b, 1, SCALE[4], 0);
    frames(&mut b, 12);
    let mut data = vgm(&[(0x0c, 3579545), (0x18, 36 * 735), (0x1c, (lp - 0x1c) as u32), (0x20, 24 * 735)], &b);
    let out = convert(&data, &Options::default()).unwrap();
    assert!((out.loop_secs.unwrap() - 0.4).abs() < 1e-6 && (out.secs - 0.6).abs() < 1e-6, "{} {:?}", out.secs, out.loop_secs);
    let (plain, mml) = (render(&out.plain, 2), render(&out.mml, 2));
    assert_eq!(plain, mml, "written MML must sound like the plain conversion, across the loop too");
    assert!(plain.len() >= sion::RATE as usize * 2 * 2, "a looping song plays on");
    // The second time round sounds like the first, volumes included. (Except for the held E: the first time it
    // carries on slurred from before the loop point, the second time it starts afresh, so its waveform's phase
    // differs until it ends at 0.3 s.) In samples: the loop starts 0.2 s in and lasts 0.4 s.
    let (lp, len, held) = (sion::RATE as usize / 5 * 2, sion::RATE as usize * 2 / 5 * 2, sion::RATE as usize / 8 * 2);
    assert!(mml[lp + held..lp + len] == mml[lp + len + held..lp + 2 * len], "second pass differs");
    // Without the loop point it plays once.
    data[0x1c..0x24].fill(0);
    assert!(convert(&data, &Options::default()).unwrap().loop_secs.is_none());
}

// ---------------------------------------------------------------------------------------------------------
// NSF / NSFE: small sound drivers, hand-assembled, that the converter runs on its 6502.

/// An NSF header for a program loaded at $8000 with INIT at $8000 and PLAY at $8010 (60 Hz NTSC).
fn nsf(songs: u8, banks: [u8; 8], chips: u8, prog: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 0x80];
    h[..5].copy_from_slice(b"NESM\x1a");
    (h[5], h[6], h[7]) = (1, songs, 1);
    for (o, v) in [(8, 0x8000u16), (0x0a, 0x8000), (0x0c, 0x8010), (0x6e, 16639)] { h[o..o + 2].copy_from_slice(&v.to_le_bytes()) }
    h[0x0e..0x13].copy_from_slice(b"Scale");
    h[0x70..0x78].copy_from_slice(&banks);
    h[0x7b] = chips;
    h.extend_from_slice(prog);
    h
}

/// Place `code` at `addr` in a program image starting at $8000.
fn put(prog: &mut Vec<u8>, addr: usize, code: &[u8]) {
    let at = addr - 0x8000;
    if prog.len() < at + code.len() { prog.resize(at + code.len(), 0) }
    prog[at..at + code.len()].copy_from_slice(code);
}

/// Pulse 1 periods of the C major test scale (low bytes, high bytes with the longest length).
fn scale_periods() -> ([u8; 5], [u8; 5]) {
    let t: Vec<u32> = SCALE.iter().map(|f| (1789773.0 / (16.0 * f)).round() as u32 - 1).collect();
    (std::array::from_fn(|k| t[k] as u8), std::array::from_fn(|k| (t[k] >> 8) as u8 | 0xf8))
}

/// A driver that plays the scale on pulse 1, a note every 12 frames, and starts over after the fifth: its
/// state (frame counter $00, note $01) comes round after 60 frames, so the song loops.
/// Bankswitched: the code is in bank 0 at $8000, the note tables in bank 2 at $9000 (bank 1 is filler), and a
/// 17-byte DPCM sample, played by INIT, in bank 3 at $C000.
fn looping_scale() -> Vec<u8> {
    let mut p = vec![];
    put(&mut p, 0x8000, &[
        0xa9, 0x0f, 0x8d, 0x15, 0x40, // LDA #$0F; STA $4015
        0xa9, 0x00, 0x85, 0x00, 0x85, 0x01, // LDA #0; STA $00; STA $01
        0x4c, 0x00, 0x81, // JMP $8100 (DPCM)
    ]);
    put(&mut p, 0x8010, &[
        0xa5, 0x00, 0xd0, 0x1c, // LDA $00; BNE skip
        0xa6, 0x01, 0xa9, 0xbf, 0x8d, 0x00, 0x40, // LDX $01; LDA #$BF; STA $4000
        0xbd, 0x40, 0x90, 0x8d, 0x02, 0x40, // LDA $9040,X; STA $4002
        0xbd, 0x48, 0x90, 0x8d, 0x03, 0x40, // LDA $9048,X; STA $4003
        0xe8, 0xe0, 0x05, 0xd0, 0x02, 0xa2, 0x00, // INX; CPX #5; BNE ok; LDX #0
        0x86, 0x01, // ok: STX $01
        0xe6, 0x00, 0xa5, 0x00, 0xc9, 0x0c, 0xd0, 0x04, 0xa9, 0x00, 0x85, 0x00, // skip: INC $00; LDA $00; CMP #12; BNE done; LDA #0; STA $00
        0x60, // done: RTS
    ]);
    // DPCM: $C000, 17 bytes, the top rate, from level 64.
    put(&mut p, 0x8100, &[0xa9, 64, 0x8d, 0x11, 0x40, 0xa9, 0x00, 0x8d, 0x12, 0x40, 0xa9, 0x01, 0x8d, 0x13, 0x40, 0xa9, 0x0f, 0x8d, 0x10, 0x40, 0xa9, 0x1f, 0x8d, 0x15, 0x40, 0x60]);
    p.resize(0x1000, 0);
    p.extend(std::iter::repeat_n(0xff, 0x1000));
    let (lo, hi) = scale_periods();
    let mut tables = vec![0u8; 0x1000];
    tables[0x40..0x45].copy_from_slice(&lo);
    tables[0x48..0x4d].copy_from_slice(&hi);
    p.extend(tables);
    let mut dpcm: Vec<u8> = (0..17u8).map(|i| [0x0f, 0xf0, 0x33, 0xcc, 0xaa, 0x55, 0x3c][i as usize % 7]).collect();
    dpcm.resize(0x1000, 0);
    p.extend(dpcm);
    nsf(1, [0, 2, 0, 0, 3, 0, 0, 0], 0, &p)
}

#[test]
fn nsf_loops_where_its_state_repeats() {
    let out = check(&looping_scale());
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "5a", "6c"]);
    assert!(out.plain.contains("%1@4"), "50% pulse");
    assert!(out.plain.contains("#PCMB") && out.plain.contains("%7"), "the DPCM sample, from its bank");
    let lp = out.loop_secs.expect("a loop");
    assert!((lp - 60.0 * 16639e-6).abs() < 0.01, "the loop is the 60 frames: {lp}");
    let (plain, mml) = (render(&out.plain, 3), render(&out.mml, 3));
    assert_eq!(plain, mml, "written MML must sound like the plain conversion, across the loop too");
}

#[test]
fn nsf_song_that_ends() {
    // Three notes 12 frames apart, then the driver silences the APU every frame but keeps counting frames in
    // $02-$03 (so its state never repeats): the song ends after its last note instead of looping silence.
    let mut p = vec![];
    put(&mut p, 0x8000, &[0xa9, 0x0f, 0x8d, 0x15, 0x40, 0x60]); // LDA #$0F; STA $4015; RTS
    put(&mut p, 0x8010, &[
        0xe6, 0x02, 0xd0, 0x02, 0xe6, 0x03, // INC $02; BNE +2; INC $03
        0xa5, 0x04, 0xd0, 0x1e, // LDA $04 (timer); BNE dec
        0xa5, 0x00, 0xc9, 0x03, 0xb0, 0x1b, // LDA $00 (notes played); CMP #3; BCS end
        0xa9, 0xbf, 0x8d, 0x00, 0x40, // LDA #$BF; STA $4000
        0xa6, 0x00, 0xbd, 0x50, 0x80, 0x8d, 0x02, 0x40, // LDX $00; LDA $8050,X; STA $4002
        0xa9, 0xf8, 0x8d, 0x03, 0x40, // LDA #$F8; STA $4003
        0xe6, 0x00, 0xa9, 0x0c, 0x85, 0x04, // INC $00; LDA #12; STA $04
        0xc6, 0x04, 0x60, // dec: DEC $04; RTS
        0xa9, 0x00, 0x8d, 0x15, 0x40, 0x60, // end: LDA #0; STA $4015; RTS
    ]);
    let t: Vec<u8> = [523.25, 659.25, 783.99].iter().map(|f| ((1789773.0 / (16.0 * f)) as f64).round() as u8 - 1).collect();
    put(&mut p, 0x8050, &t);
    let out = check(&nsf(1, [0; 8], 0, &p));
    assert!(out.loop_secs.is_none(), "no loop of silence");
    assert_eq!(notes(&out.plain, 0), ["6c", "6e", "6g"]);
    assert!(out.secs < 1.0, "ends after its notes: {}", out.secs);
}

#[test]
fn nsf_vrc6() {
    // VRC6: pulse 1 at A (duty 4/16: SiON's 25% pulse), pulse 2 at C (3/16: a wavetable), the saw at A an
    // octave down; for 60 frames, then all off.
    let per = |hz: f64, div: f64| (1789773.0 / (div * hz)).round() as u32 - 1;
    let (a, c, s) = (per(440.0, 16.0), per(523.25, 16.0), per(220.0, 14.0));
    let mut p = vec![];
    put(&mut p, 0x8000, &[
        0xa9, 0x00, 0x8d, 0x03, 0x90, // $9003 = 0
        0xa9, 0x3f, 0x8d, 0x00, 0x90, 0xa9, a as u8, 0x8d, 0x01, 0x90, 0xa9, 0x80 | (a >> 8) as u8, 0x8d, 0x02, 0x90,
        0xa9, 0x2f, 0x8d, 0x00, 0xa0, 0xa9, c as u8, 0x8d, 0x01, 0xa0, 0xa9, 0x80 | (c >> 8) as u8, 0x8d, 0x02, 0xa0,
        0xa9, 0x2a, 0x8d, 0x00, 0xb0, 0xa9, s as u8, 0x8d, 0x01, 0xb0, 0xa9, 0x80 | (s >> 8) as u8, 0x8d, 0x02, 0xb0,
        0x60,
    ]);
    // PLAY (at $8080): count frames in $00; on the 60th switch all three off.
    put(&mut p, 0x8080, &[
        0xa5, 0x00, 0xc9, 0x3c, 0xf0, 0x13, // LDA $00; CMP #60; BEQ done
        0xe6, 0x00, 0xa5, 0x00, 0xc9, 0x3c, 0xd0, 0x0b, // INC $00; LDA $00; CMP #60; BNE done
        0xa9, 0x00, 0x8d, 0x02, 0x90, 0x8d, 0x02, 0xa0, 0x8d, 0x02, 0xb0, // LDA #0; STA $9002; STA $A002; STA $B002
        0x60, // done: RTS
    ]);
    let mut f = nsf(1, [0; 8], 1, &p);
    f[0x0c..0x0e].copy_from_slice(&0x8080u16.to_le_bytes());
    let out = check(&f);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    // Pulses other than 50% are wavetables balanced around zero (no DC offset to follow the volume steps).
    let wave = |hi: i32, h: usize, lo: i32| format!("{{{}}}", (0..16).map(|j| if j < h { hi } else { lo }.to_string()).collect::<Vec<_>>().join(","));
    assert!(out.plain.contains(&wave(126, 4, -42)), "4/16 duty");
    assert!(out.plain.contains(&wave(117, 3, -27)), "3/16 duty");
    assert!(!out.plain.contains("%1@"), "no SiON APU pulse (it swings from -full to +full)");
    let all: Vec<String> = (0..out.tracks).flat_map(|t| notes(&out.plain, t)).collect();
    for n in ["5a", "6c", "4a"] { assert!(all.contains(&n.to_string()), "{n} in {all:?}") }
    assert!(out.loop_secs.is_none() && out.secs < 1.5, "ends when switched off: {} {:?}", out.secs, out.loop_secs);
}

#[test]
fn nsf_vrc6_saw_rate_envelope() {
    // The saw at A, its rate (the level, and so the waveform table) changed every frame, as a volume envelope
    // does: one held note whose waveform changes (`@n` in a slur), not a new key-on (a phase reset) every frame.
    let s = (1789773.0 / (14.0 * 220.0f64)).round() as u32 - 1;
    let mut p = vec![];
    put(&mut p, 0x8000, &[
        0xa9, 0x00, 0x8d, 0x03, 0x90, // $9003 = 0
        0xa9, 0x2a, 0x8d, 0x00, 0xb0, 0xa9, s as u8, 0x8d, 0x01, 0xb0, 0xa9, 0x80 | (s >> 8) as u8, 0x8d, 0x02, 0xb0,
        0x60,
    ]);
    // PLAY: count frames in $00; rate = 16 + (frame & 15); on the 60th frame the saw goes off.
    put(&mut p, 0x8080, &[
        0xa5, 0x00, 0xc9, 0x3c, 0xf0, 0x16, // LDA $00; CMP #60; BEQ done
        0xe6, 0x00, 0xa5, 0x00, 0x29, 0x0f, 0x69, 0x10, 0x8d, 0x00, 0xb0, // INC $00; LDA $00; AND #15; ADC #16; STA $B000
        0xa5, 0x00, 0xc9, 0x3c, 0xd0, 0x05, // LDA $00; CMP #60; BNE done
        0xa9, 0x00, 0x8d, 0x02, 0xb0, // LDA #0; STA $B002
        0x60, // done: RTS
    ]);
    let mut f = nsf(1, [0; 8], 1, &p);
    f[0x0c..0x0e].copy_from_slice(&0x8080u16.to_le_bytes());
    let out = check(&f);
    let t = out.plain.split(";\n").find(|t| !t.starts_with('#')).unwrap();
    assert!(notes(&out.plain, 0).iter().all(|n| n == "4a"));
    // (Every piece but the last slurred into the next, and the module set once.)
    assert_eq!(t.matches("o4a").count(), t.matches('&').count() + 1, "one note: {t}");
    assert_eq!(t.matches('%').count(), 1, "waveform changes as `@n` in the slur: {t}");
}

#[test]
fn nsfe_chunks_and_tracks() {
    // The looping scale as an NSFE with two tracks and their names.
    let n = looping_scale();
    let chunk = |id: &[u8], body: &[u8]| { let mut c = (body.len() as u32).to_le_bytes().to_vec(); c.extend(id); c.extend(body); c };
    let mut info = vec![];
    for v in [0x8000u16, 0x8000, 0x8010] { info.extend(v.to_le_bytes()) }
    info.extend([0, 0, 2, 0]);
    let mut f = b"NSFE".to_vec();
    f.extend(chunk(b"INFO", &info));
    f.extend(chunk(b"BANK", &n[0x70..0x78]));
    f.extend(chunk(b"tlbl", b"First\0Second\0"));
    f.extend(chunk(b"DATA", &n[0x80..]));
    f.extend(chunk(b"NEND", &[]));
    let out = check(&f);
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "5a", "6c"]);
    assert!(out.warnings.iter().any(|w| w.contains("track 1 of 2 (First)")), "{:?}", out.warnings);
    let second = convert(&f, &Options { track: Some(2), ..Options::default() }).unwrap();
    assert!(second.warnings.iter().any(|w| w.contains("track 2 of 2 (Second)")));
    assert!(convert(&f, &Options { track: Some(3), ..Options::default() }).is_err());
}

// ---------------------------------------------------------------------------------------------------------
// SPC: a small sound driver, hand-assembled, that the converter runs on its SPC700.

/// An SPC snapshot: `prog` at $0200 (where the CPU starts), with `data` placed at the given addresses.
fn spc(prog: &[u8], data: &[(usize, &[u8])]) -> Vec<u8> {
    let mut f = vec![0u8; 0x10200];
    f[..33].copy_from_slice(b"SNES-SPC700 Sound File Data v0.30");
    (f[0x21], f[0x22], f[0x23], f[0x24]) = (26, 26, 27, 30);
    (f[0x25], f[0x26], f[0x2a], f[0x2b]) = (0x00, 0x02, 0x02, 0xef);
    let ram = &mut f[0x100..0x10100];
    ram[0x200..0x200 + prog.len()].copy_from_slice(prog);
    for &(at, d) in data { ram[at..at + d.len()].copy_from_slice(d) }
    f
}

/// A driver that keys on the C major scale on voice 0, a note every 25 ticks of timer 0 (100 Hz), and starts
/// over after the fifth: its state comes round every 1.25 s. The sample is a looping 1 kHz square (32 frames at
/// 32 kHz, two BRR blocks).
fn spc_scale() -> Vec<u8> {
    let dsp = |r: u8, v: u8| [0x8f, r, 0xf2, 0x8f, v, 0xf3]; // MOV $F2,#r; MOV $F3,#v
    let mut p = vec![];
    for (r, v) in [(0x5d, 0x03), (0x0c, 127), (0x1c, 127), (0x6c, 0x20), (0x00, 100), (0x01, 100), (0x04, 0), (0x05, 0x8f), (0x06, 0xe0)] { p.extend(dsp(r, v)) }
    p.extend([0x8f, 0x50, 0xfa, 0x8f, 0x01, 0xf1, 0x8f, 0x00, 0x00, 0x8f, 0x00, 0x01]); // timer 0 at 100 Hz; $00 = $01 = 0
    let lp = 0x200 + p.len() as u16;
    p.extend([0xeb, 0xfd, 0xf0, 0xfc]); // loop: MOV Y,$FD; BEQ loop
    p.extend([0xab, 0x01, 0xe4, 0x01, 0x68, 25]); // INC $01; MOV A,$01; CMP A,#25
    let bne = p.len();
    p.extend([0xd0, 0x00]); // BNE loop
    p.extend([0x8f, 0x00, 0x01, 0xf8, 0x00]); // MOV $01,#0; MOV X,$00
    p.extend([0xf5, 0x00, 0x04, 0x8f, 0x02, 0xf2, 0xc4, 0xf3]); // MOV A,$0400+X; MOV $F2,#2; MOV $F3,A
    p.extend([0xf5, 0x08, 0x04, 0x8f, 0x03, 0xf2, 0xc4, 0xf3]); // MOV A,$0408+X; MOV $F2,#3; MOV $F3,A
    p.extend(dsp(0x4c, 0x01)); // KON voice 0
    p.extend([0x3d, 0xc8, 0x05, 0xd0, 0x02, 0xcd, 0x00, 0xd8, 0x00]); // INC X; CMP X,#5; BNE +2; MOV X,#0; MOV $00,X
    let bra = p.len();
    p.extend([0x2f, 0x00]); // BRA loop
    let rel = |from: usize| (lp as i32 - (0x200 + from as i32 + 2)) as i8 as u8;
    (p[bne + 1], p[bra + 1]) = (rel(bne), rel(bra));
    let pitch: Vec<u16> = SCALE.iter().map(|f| (4096.0 * f / 1000.0f64).round() as u16).collect();
    let (lo, hi): (Vec<u8>, Vec<u8>) = pitch.iter().map(|&p| (p as u8, (p >> 8) as u8)).unzip();
    let mut brr = vec![0xa0];
    brr.extend([0x77; 8]);
    brr.push(0xa3);
    brr.extend([0x99; 8]);
    spc(&p, &[(0x300, &[0x00, 0x05, 0x00, 0x05]), (0x400, &lo), (0x408, &hi), (0x500, &brr)])
}

#[test]
fn spc_loops_where_its_state_repeats() {
    let out = check(&spc_scale());
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "5a", "6c"]);
    assert!(out.plain.contains("#PCMB") && out.plain.contains("%7@0"), "the BRR sample, embedded");
    let lp = out.loop_secs.expect("a loop");
    assert!((lp - 1.25).abs() < 0.02, "the loop is the five notes: {lp}");
    assert_eq!(render(&out.plain, 4), render(&out.mml, 4), "written MML must sound like the plain conversion, across the loop too");
}

#[test]
fn spc_sample_plays_at_its_pitch() {
    // The first note (C4, 261.63 Hz) as SiON plays it: its period, from zero crossings.
    let out = check(&spc_scale());
    let audio = render(&out.plain, 1);
    let left: Vec<f32> = audio.iter().step_by(2).copied().collect();
    let seg = &left[1000..10000];
    // (Rising crossings with hysteresis: the softened square ripples near zero.)
    let thr = seg.iter().fold(0f32, |m, v| m.max(v.abs())) * 0.5;
    let (mut ups, mut high) = (0, seg[0] > 0.0);
    for &v in seg { if !high && v > thr { ups += 1; high = true } else if high && v < -thr { high = false } }
    let hz = ups as f64 * sion::RATE as f64 / seg.len() as f64;
    assert!((hz - 261.63).abs() < 3.0, "C4: {hz} Hz");
}
