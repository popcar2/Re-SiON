//! PCM sample playback (`%7`, `%10`, `#PCMB`, `@ph`).

use sion::{Sample, Song, RATE};

fn render(mut song: Song, frames: usize) -> Vec<f32> {
    let mut out = vec![0f32; frames * 2];
    song.render(&mut out);
    out.iter().step_by(2).copied().collect()
}

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..c.len() + 1 { s.push(A[(n >> (18 - 6 * k)) as usize & 63] as char) }
    }
    s
}

/// Rising zero crossings per second.
fn hz(x: &[f32]) -> f32 { x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f32 * RATE / x.len() as f32 }

fn sine(n: usize) -> Vec<f32> { (0..n).map(|i| (i as f32 * core::f32::consts::TAU / n as f32).sin()).collect() }

#[test]
fn pitch_follows_the_note() {
    // A 100-frame cycle at 44000 frames per second at o5a: 440 Hz there, an octave up at o6a.
    for (mml, want) in [("%7@3 o5a1;", 440.0), ("%7@3 o6a1;", 880.0), ("%7@3 k32 o5a1;", 440.0 * 2f32.powf(0.5 / 12.0))] {
        let mut s = Song::new(mml);
        s.set_sample(3, Sample { data: sine(100), rate: 44000.0, key: 69, lp: Some(0), bidi: false });
        let got = hz(&render(s, 44100)[4410..]);
        assert!((got - want).abs() < 2.0, "{mml}: {got} Hz, want {want}");
    }
}

#[test]
fn sampler_picks_by_note() {
    let mut s = Song::new("%10 o5a1;");
    s.set_sample(69, Sample { data: sine(100), rate: 22050.0, key: 0, lp: Some(0), bidi: false });
    let got = hz(&render(s, 44100)[4410..]);
    assert!((got - 220.5).abs() < 2.0, "{got}");
}

/// Play `data` once at the output rate (one frame per sample, root note) and return what comes out, scaled to the
/// first frame's level.
fn played(head: &str, notes: &str, frames: usize) -> Vec<f32> {
    render(Song::new(&format!("{head};#VMODE{{%v0,0}};%7@0 s63 v256 {notes};")), frames)
}

#[test]
fn pcmb_decodes_8_and_16_bit_exactly() {
    let v8: Vec<i8> = vec![100, -50, 25, -128, 127, 3];
    let v16: Vec<i16> = vec![30000, -12345, 7, -32768, 32767, 1];
    let a = played(&format!("#PCMB0{{{}}}44100,60", b64(&v8.iter().map(|&v| v as u8).collect::<Vec<_>>())), "o5c1", 6);
    let b = played(&format!("#PCMB0{{{}}}44100,60,-1,1", b64(&v16.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>())), "o5c1", 6);
    let (ga, gb) = (a[0] / (100.0 / 128.0), b[0] / (30000.0 / 32768.0));
    for (i, v) in v8.iter().enumerate() { assert!((a[i] / ga - *v as f32 / 128.0).abs() < 1e-6, "8-bit frame {i}") }
    for (i, v) in v16.iter().enumerate() { assert!((b[i] / gb - *v as f32 / 32768.0).abs() < 1e-6, "16-bit frame {i}") }
}

#[test]
fn loops_one_shots_and_offsets() {
    let data: Vec<u8> = (1..=6).map(|v| v * 10).collect(); // frames 10, 20, .. 60
    let enc = b64(&data);
    let run = |pfx: &str, notes: &str, n: usize| {
        let o = played(&format!("#PCMB0{{{enc}}}{pfx}"), notes, n);
        let g = o.iter().copied().find(|v| *v != 0.0).unwrap();
        o.iter().map(|v| (v / g * 10.0).round() as i32).collect::<Vec<_>>()
    };
    let one = run("44100,60", "o5c1", 9);
    assert_eq!(one, [10, 20, 30, 40, 50, 60, 0, 0, 0], "one-shot stops after its last frame");
    let fwd = run("44100,60,2", "o5c1", 12);
    assert_eq!(fwd, [10, 20, 30, 40, 50, 60, 30, 40, 50, 60, 30, 40], "forward loop from frame 2");
    let bidi = run("44100,60,2,2", "o5c1", 14);
    assert_eq!(bidi, [10, 20, 30, 40, 50, 60, 50, 40, 30, 40, 50, 60, 50, 40], "ping-pong loop over frames 2..5");
    let ofs = played(&format!("#PCMB0{{{enc}}}44100,60,2"), "@ph3 o5c1", 6);
    let g = ofs[0] / 40.0;
    assert_eq!(ofs.iter().map(|v| (v / g).round() as i32).collect::<Vec<_>>(), [40, 50, 60, 30, 40, 50], "`@ph` starts the note at frame 3");
}

#[test]
fn half_speed_interpolates() {
    let o = played(&format!("#PCMB0{{{}}}22050,60", b64(&[0, 100, 0])), "o5c1", 5);
    let g = o[2] / 100.0;
    let got: Vec<i32> = o.iter().map(|v| (v / g).round() as i32).collect();
    assert_eq!(got, [0, 50, 100, 50, 0]);
}

#[test]
fn slur_keeps_the_sample_position() {
    // A pitch change on a slurred note must not restart the sample.
    let ramp: Vec<f32> = (0..44100).map(|i| i as f32 / 44100.0).collect();
    let mut s = Song::new("#VMODE{%v0,0};%7@0 q8 s63 v256 o5c8&d8;");
    s.set_sample(0, Sample { data: ramp, rate: 44100.0, key: 60, lp: None, bidi: false });
    let o = render(s, 22000);
    assert!(o[21000] > 0.01, "still sounding near the end");
    let jumps = o.windows(2).filter(|w| w[1] < w[0] - 1e-3).count();
    assert_eq!(jumps, 0, "the ramp never restarts");
}

#[test]
fn fractional_detune() {
    // k-0.5 is half of a 1/64-semitone step below the note: measure it on a long note as the frames played.
    let ramp: Vec<f32> = (0..200000).map(|i| i as f32 / 200000.0).collect();
    let pos = |mml: &str| {
        let mut s = Song::new(mml);
        s.set_sample(0, Sample { data: ramp.clone(), rate: 44100.0, key: 60, lp: None, bidi: false });
        let o = render(s, 88200);
        o[88199] as f64 // the ramp's value: frames played so far, at the same level in every run
    };
    let base = pos("#VMODE{%v0,0};%7@0 q8 s63 v256 k0 o5c1;");
    let half = pos("#VMODE{%v0,0};%7@0 q8 s63 v256 k-0.5 o5c1;");
    let one = pos("#VMODE{%v0,0};%7@0 q8 s63 v256 k-1 o5c1;");
    assert!(base != half && half != one);
    let (ra, rb) = (base / half, half / one);
    assert!((ra - rb).abs() < 1e-6 && ra > 1.0, "k-0.5 lies halfway (in log pitch) between k0 and k-1: {ra} {rb}");
}

#[test]
fn fractional_start_frame() {
    let enc = b64(&[0, 40, 80, 120]);
    let o = played(&format!("#PCMB0{{{enc}}}44100,60"), "@ph1.5 o5c1", 3);
    let g = o[0] / 60.0;
    let got: Vec<i32> = o.iter().map(|v| (v / g).round() as i32).collect();
    assert_eq!(got, [60, 100, 60], "starts halfway between frames 1 and 2 (the last step runs past the end into silence)");
}

#[test]
fn negative_start_frame_waits() {
    let o = played(&format!("#PCMB0{{{}}}44100,60", b64(&[100, 100, 100])), "@ph-2.5 o5c1", 6);
    let g = o[4] / 100.0;
    let got: Vec<i32> = o.iter().map(|v| (v / g).round() as i32).collect();
    assert_eq!(got, [0, 0, 50, 100, 100, 50], "two and a half frames of silence (fading in over the last one), the sample, and its fade past the end");
}

#[test]
fn play_length_stops_the_sample() {
    let o = played(&format!("#PCMB0{{{}}}44100,60", b64(&[100; 8])), "@ph-1,4 o5c1", 8);
    let g = o[2] / 100.0;
    let got: Vec<i32> = o.iter().map(|v| (v / g).round() as i32).collect();
    assert_eq!(got, [0, 100, 100, 100, 0, 0, 0, 0], "a frame of silence, then frames 0..2: four from the start position");
}

/// The NES output stage's DC blocker as the DPCM channel applies it (first order, 60 Hz).
fn dc_block(x: &[f32]) -> Vec<f32> {
    let a = 1.0 / (1.0 + core::f32::consts::TAU * 60.0 / RATE);
    let (mut y, mut prev) = (0f32, 0f32);
    x.iter().map(|&v| { y = a * (y + v - prev); prev = v; y }).collect()
}

/// `out` against `want` scaled by the gain that fits best: the largest difference, relative to `out`'s peak.
fn misfit(out: &[f32], want: &[f32]) -> f32 {
    let g = out.iter().zip(want).map(|(a, b)| a * b).sum::<f32>() / want.iter().map(|b| b * b).sum::<f32>();
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    out.iter().zip(want).fold(0f32, |m, (a, b)| m.max((a - g * b).abs())) / peak
}

/// DMC levels from `level` for DPCM `bytes`, one per bit, least significant first (as the console: steps of 2,
/// staying within 0..127), then held for the rest of `n` frames.
fn dmc_levels(bytes: &[u8], level: i32, n: usize) -> Vec<f32> {
    let mut l = level;
    let mut v = vec![l];
    for b in bytes { for k in 0..8 { l = if b >> k & 1 != 0 { (l + 2).min(127) } else { (l - 2).max(0) }; v.push(l) } }
    v.resize(n, l);
    v.into_iter().map(|l| l as f32 / 128.0).collect()
}

#[test]
fn dpcm_plays_as_the_dmc() {
    // From level 0, 0xF0 steps down four times (staying at 0) then up; 0xFF goes on up. The level then holds, and
    // what comes out is it through the DC blocker.
    let bytes = [0xf0, 0xff, 0x0f];
    let out = played(&format!("#PCMB0{{{}}}44100,60,-1,4,0", b64(&bytes)), "o5c1", 600);
    let want = dc_block(&dmc_levels(&bytes, 0, 600));
    let err = (0..3).map(|lag| misfit(&out[lag..], &want[..600 - lag])).fold(f32::MAX, f32::min);
    assert!(err < 1e-3, "{err}");
    // The same bytes from an earlier `#PCMB` (`{=m}`) play the same.
    let by_ref = played(&format!("#PCMB1{{{}}}44100,60,-1,4,9;#PCMB0{{=1}}44100,60,-1,4,0", b64(&bytes)), "o5c1", 600);
    assert_eq!(out, by_ref);
}

#[test]
fn dpcm_samples_join_at_the_level() {
    // Up 16 levels, then (a sample decoded from where that one ends) down 16, starting partway into a frame: no
    // step where they meet, nor where the second stops early (`@ph` length) and its level holds.
    let (up, down) = ([0xffu8, 0xff], [0x00u8, 0x00]);
    let head = format!("#PCMB0{{{}}}44100,60,-1,4,0;#PCMB1{{{}}}44100,60,-1,4,32", b64(&up), b64(&down));
    let out = played(&head, "l64 o5c @1 @ph-0.4,10 c", 3000);
    let step = out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    let first = out.windows(2).take(20).map(|w| (w[1] - w[0]).abs()).fold(0f32, f32::max);
    assert!(step <= first * 1.01, "largest step {step}, a DPCM step {first}");
    // The second sample stopped after 10 frames: the level holds there (the DC blocker takes it back to 0).
    assert!(out[2999].abs() < out[1400].abs().max(1e-4), "{} {}", out[1400], out[2999]);
}
