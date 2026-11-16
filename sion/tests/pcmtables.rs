//! `#PCMWAVE`, `#PCMVOICE` and `#SAMPLER`: instruments and drum kits built from `#PCMB` samples.

use sion::{Sample, Song};

/// Stereo frames, (left, right).
fn render(mml: &str) -> Vec<(f32, f32)> {
    let mut out = vec![0f32; 8192 * 2];
    Song::new(mml).render(&mut out);
    out.chunks(2).map(|f| (f[0], f[1])).collect()
}

fn mono(mml: &str) -> Vec<f32> { render(mml).iter().map(|f| f.0).collect() }

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..=c.len() { s.push(A[(n >> (18 - 6 * k)) as usize & 63] as char) }
    }
    s
}

/// An 8-bit sample: a rising ramp of `n` frames from `from`.
fn ramp(from: i8, n: usize) -> Vec<u8> { (0..n).map(|i| from.wrapping_add(i as i8) as u8).collect() }

fn energy(v: &[f32]) -> f32 { v.iter().map(|x| x * x).sum() }

#[test]
fn pcmwave_plays_a_pcmb_like_the_pcmb_itself() {
    let d = b64(&ramp(-100, 2000));
    let a = mono(&format!("#PCMB0{{{d}}}22050; %7@0 o5c1;"));
    let b = mono(&format!("#PCMB3{{{d}}}22050; #PCMWAVE0{{3}}; %7@0 o5c1;"));
    assert!(energy(&a) > 0.1);
    assert_eq!(a, b);
}

#[test]
fn key_ranges_pick_the_sample() {
    // Keys 0..59 play a rising ramp, 60..127 a falling one (the same notes either side of the split).
    let up = b64(&ramp(-100, 4000));
    let down: Vec<u8> = ramp(-100, 4000).iter().map(|b| (*b as i8).wrapping_neg() as u8).collect();
    let defs = format!("#PCMB0{{{up}}}44100,59; #PCMB1{{{}}}44100,60; #PCMWAVE5{{0,,0,59}}; #PCMWAVE5{{1,,60,127}};", b64(&down));
    let lo = mono(&format!("{defs} %7@5 o4b1;"));
    let hi = mono(&format!("{defs} %7@5 o5c1;"));
    assert_eq!(lo, mono(&format!("#PCMB0{{{up}}}44100,59; %7@0 o4b1;")));
    assert_eq!(hi, mono(&format!("#PCMB1{{{}}}44100,60; %7@1 o5c1;", b64(&down))));
    // A key outside every range is silent.
    let gap = mono(&format!("#PCMB0{{{up}}}; #PCMWAVE5{{0,,60,61}}; %7@5 o4b1;"));
    assert!(gap.iter().all(|x| *x == 0.0));
}

#[test]
fn sampling_note_sets_the_pitch() {
    // A sample recorded at note 72 (`#PCMWAVE`'s note) sounds there as `#PCMB`'s key 72 does.
    let d = b64(&ramp(-100, 3000));
    let a = mono(&format!("#PCMB0{{{d}}}22050,72; %7@0 o6c1;"));
    let b = mono(&format!("#PCMB0{{{d}}}22050; #PCMWAVE1{{0,72}}; %7@1 o6c1;"));
    assert_eq!(a, b);
    // A fraction of a note: o5c at sampling note 60.5 plays 1/2 semitone below the rate.
    let c = mono(&format!("#PCMB0{{{d}}}22050; #PCMWAVE1{{0,60.5}}; %7@1 o5c1;"));
    let e = mono(&format!("#PCMB0{{{d}}}22050; %7@0 o5k-32c1;"));
    let diff = c.iter().zip(&e).map(|(x, y)| (x - y).abs()).fold(0f32, f32::max);
    assert!(diff < 1e-3, "{diff}");
}

#[test]
fn start_end_and_loop_cut_the_sample() {
    let all = ramp(-100, 1000);
    let d = b64(&all);
    // Frames 200..600, looping from 300: the same as a `#PCMB` of just those frames, looping from 100.
    let a = mono(&format!("#PCMB0{{{d}}}22050; #PCMWAVE1{{0,,,,,200,600,300}}; %7@1 o5c1;"));
    let b = mono(&format!("#PCMB0{{{}}}22050,60,100; %7@0 o5c1;", b64(&all[200..600])));
    assert_eq!(a, b);
    assert!(energy(&a[4000..]) > 0.1, "the loop keeps playing");
    // A loop below -1: the last that many frames.
    let c = mono(&format!("#PCMB0{{{d}}}22050; #PCMWAVE1{{0,,,,,,,-50}}; %7@1 o5c1;"));
    let e = mono(&format!("#PCMB0{{{d}}}22050,60,950; %7@0 o5c1;"));
    assert_eq!(c, e);
    // Left out, the `#PCMB`'s own loop stays (moved with the start); -1 takes it away.
    let f = mono(&format!("#PCMB0{{{d}}}22050,60,500; #PCMWAVE1{{0,,,,,100}}; %7@1 o5c1;"));
    let g = mono(&format!("#PCMB0{{{}}}22050,60,400; %7@0 o5c1;", b64(&all[100..])));
    assert_eq!(f, g);
    let h = mono(&format!("#PCMB0{{{d}}}22050,60,500; #PCMWAVE1{{0,,,,,,,-1}}; %7@1 o5c1;"));
    assert!(energy(&h[4000..]) == 0.0);
}

#[test]
fn definitions_can_come_in_any_order() {
    let d = b64(&ramp(-100, 2000));
    let a = mono(&format!("#PCMWAVE0{{3,,,,,100}}; #PCMB3{{{d}}}22050; %7@0 o5c1;"));
    let b = mono(&format!("#PCMB3{{{d}}}22050; #PCMWAVE0{{3,,,,,100}}; %7@0 o5c1;"));
    assert!(energy(&a) > 0.1);
    assert_eq!(a, b);
}

#[test]
fn set_sample_replaces_the_cuts_too() {
    let mml = "#PCMB0{AAAA}; #PCMWAVE1{0,,,,,100}; %7@1 o5c1;";
    let data: Vec<f32> = (0..2000).map(|i| (i % 50) as f32 / 50.0 - 0.5).collect();
    let mut song = Song::new(mml);
    song.set_sample(0, Sample { data: data.clone(), rate: 22050.0, key: 60, lp: None, ..Default::default() });
    let mut a = vec![0f32; 8192 * 2];
    song.render(&mut a);
    let mut song = Song::new("%7@0 o5c1;");
    song.set_sample(0, Sample { data: data[100..].to_vec(), rate: 22050.0, key: 60, lp: None, ..Default::default() });
    let mut b = vec![0f32; 8192 * 2];
    song.render(&mut b);
    assert!(energy(&a) > 0.1);
    assert_eq!(a, b);
}

#[test]
fn pcmvoice_sets_envelope_key_scaling_and_commands() {
    let d = b64(&ramp(-100, 4000));
    // Release rate 20 instead of an instant cut: the note rings on after key-off.
    let plain = mono(&format!("#PCMB0{{{d}}}22050,60,3000; #PCMVOICE0{{}}; %7@0 o5c32r2;"));
    let rel = mono(&format!("#PCMB0{{{d}}}22050,60,3000; #PCMVOICE0{{,,,,,,63,0,0,20,0}}; %7@0 o5c32r2;"));
    let off = 4000; // after the 1/32 note's key-off (2756 frames at 120 bpm)
    assert!(energy(&rel[off..off + 1000]) > 10.0 * energy(&plain[off..off + 1000]).max(1e-9));
    // Volume key scaling: amount 64 over keys 0..127 centered on 64 halves key 0's level.
    let low = mono(&format!("#PCMB0{{{d}}}22050,0; #PCMVOICE0{{64,128,64}}; %7@0 o0c1;"));
    let full = mono(&format!("#PCMB0{{{d}}}22050,0; %7@0 o0c1;"));
    let r = (energy(&low) / energy(&full)).sqrt();
    assert!((r - 0.5).abs() < 0.01, "{r}");
    // The commands after it run when the voice is chosen (here a pan to the right).
    let p = render(&format!("#PCMB0{{{d}}}22050; #PCMVOICE0{{}}@p64; %7@0 o5c1;"));
    let (l, rt): (f32, f32) = p.iter().fold((0.0, 0.0), |a, f| (a.0 + f.0 * f.0, a.1 + f.1 * f.1));
    assert!(rt > 0.1 && l < rt * 1e-3, "{l} {rt}");
}

#[test]
fn pcmvoice_pan_width_spreads_keys() {
    let d = b64(&ramp(-100, 4000));
    // Width 128 across keys 0..127: key 0 hard left, key 127 hard right.
    let lr = |mml: &str| render(mml).iter().fold((0f32, 0f32), |a, f| (a.0 + f.0 * f.0, a.1 + f.1 * f.1));
    let (l, r) = lr(&format!("#PCMB0{{{d}}}22050,0; #PCMVOICE0{{,,,64,128,128}}; %7@0 o0c1;"));
    assert!(l > 0.1 && r < l * 1e-3, "{l} {r}");
    let (l, r) = lr(&format!("#PCMB0{{{d}}}22050,127; #PCMVOICE0{{,,,64,128,128}}; %7@0 o10g1;"));
    assert!(r > 0.1 && l < r * 1e-3, "{l} {r}");
}

#[test]
fn sampler_keys_make_a_drum_kit() {
    let kick = b64(&ramp(-100, 1500));
    let snare: Vec<u8> = ramp(-100, 1500).iter().map(|b| (*b as i8).wrapping_neg() as u8).collect();
    let defs = format!("#PCMB0{{{kick}}}22050; #PCMB1{{{}}}22050; #SAMPLER36{{0}}; #SAMPLER38{{1}};", b64(&snare));
    // Key 36 (o3c) plays the kick at its own rate, whatever the note, as `%10` does: the same as `%7` at its key.
    assert_eq!(mono(&format!("{defs} %10 o3c1;")), mono(&format!("#PCMB0{{{kick}}}22050; %7@0 o5c1;")));
    assert_eq!(mono(&format!("{defs} %10 o3d1;")), mono(&format!("#PCMB1{{{}}}22050; %7@1 o5c1;", b64(&snare))));
    // A key without a `#SAMPLER` in a bank that has some is silent (not `#PCMB` n).
    assert!(mono(&format!("{defs} %10 o0c1;")).iter().all(|x| *x == 0.0));
    // Without any `#SAMPLER`, `%10` still plays `#PCMB` n on key n.
    assert_eq!(mono(&format!("#PCMB0{{{kick}}}22050; %10 o0c1;")), mono(&format!("#PCMB0{{{kick}}}22050; %7@0 o5c1;")));
}

#[test]
fn sampler_banks_pan_and_key_off() {
    let d = b64(&ramp(-100, 3000));
    // Bank 1 (`#SAMPLER` 128..255) is `%10@1`.
    let a = mono(&format!("#PCMB0{{{d}}}22050; #SAMPLER164{{0}}; %10@1 o3c1;"));
    assert!(energy(&a) > 0.1);
    assert!(mono(&format!("#PCMB0{{{d}}}22050; #SAMPLER164{{0}}; %10 o3c1;")).iter().all(|x| *x == 0.0));
    // Pan offset -64: hard left.
    let p = render(&format!("#PCMB0{{{d}}}22050; #SAMPLER36{{0,0,-64}}; %10 o3c1;"));
    let (l, r) = p.iter().fold((0f32, 0f32), |a, f| (a.0 + f.0 * f.0, a.1 + f.1 * f.1));
    assert!(l > 0.1 && r < l * 1e-3, "{l} {r}");
    // Ignoring key-off: a short note plays the whole sample.
    let short = mono(&format!("#PCMB0{{{d}}}22050; #SAMPLER36{{0}}; %10 s63 o3c64r1;"));
    let held = mono(&format!("#PCMB0{{{d}}}22050; #SAMPLER36{{0,1}}; %10 s63 o3c64r1;"));
    assert!(energy(&held[3000..5000]) > 0.1 && energy(&short[3000..5000]) < 1e-6);
}
