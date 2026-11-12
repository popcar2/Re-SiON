//! End-to-end checks of the non-MIDI readers on small files built here: the right notes come out, and the
//! minified MML sounds identical to the plain one.

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
    assert_eq!(render(&out.plain, 60), render(&out.mml, 60), "minified MML must sound like the plain conversion");
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
    assert_eq!(render(&out.plain, 60), render(&out.mml, 60), "minified MML must sound like the plain conversion");
    // A 32-frame cycle played at 3546895 / 428 Hz is 259 Hz: just under middle C.
    assert!(out.plain.contains("#WAV0{"), "the looped sample becomes a wavetable");
    assert_eq!(notes(&out.plain, 0), ["5c", "5e", "5g", "6c"]);
}

#[test]
fn mod_sample_plays_as_pcm() {
    let out = check(&protracker());
    assert!(out.plain.contains("#PCMB0{") && out.plain.contains("%7@0"), "the sample is embedded and played with %7");
    let track = out.plain.split(";\n").find(|t| t.contains("%7")).unwrap();
    assert!(track.contains("o5c2o5e2o5g2o6c2&v0o6c92r"), "C-2 E-2 G-2 C-3, then off a tick after C00: {track}");
    let want = sion_converter::reference(&protracker()).unwrap();
    let db = sion_converter::snr(&want, &render(&out.mml, 60));
    assert!(db > 50.0, "conversion differs from the tracker mix: {db:.1} dB");
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

const EXACT: Options = Options { grid: None, gain: None, wavetables: false, exact_pcm: true };

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

#[test]
fn nes_dpcm_and_raw_pcm() {
    // A 17-byte DPCM sample at $C000 played twice at the top rate, then levels written to $4011 every 3 frames.
    // (Each byte has as many ups as downs, so the level ends where it started: the NES would hold any other
    // level, a DC offset the conversion leaves out.)
    let bits: Vec<u8> = (0..17u8).map(|i| [0x0f, 0xf0, 0x33, 0xcc, 0xaa, 0x55, 0x3c][i as usize % 7]).collect();
    let mut body = vec![0x00, 0xc0];
    body.extend(&bits);
    let mut b = block(0xc2, &body);
    b.extend([0xb4, 0x15, 0x00, 0xb4, 0x11, 64, 0xb4, 0x10, 0x0f, 0xb4, 0x12, 0x00, 0xb4, 0x13, 0x01]);
    let rate = 1789773.0 / 54.0;
    let (mut held, mut played, mut t) = (vec![], vec![], 0u64);
    for _ in 0..2 {
        wait(&mut b, 3000);
        t += 3000;
        b.extend([0xb4, 0x15, 0x10]);
        let (mut l, mut frames) = (64i32, vec![0.0]);
        for byte in &bits { for k in 0..8 { if byte >> k & 1 != 0 { if l <= 125 { l += 2 } } else if l >= 2 { l -= 2 } frames.push((l - 64) as f64) } }
        mix(&mut held, t as f64, rate, &frames);
        mix(&mut played, t as f64, rate, &frames);
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
    assert_eq!(plain, mml, "minified MML must sound like the plain conversion, across the loop too");
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
