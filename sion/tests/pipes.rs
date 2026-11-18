//! Pipes between tracks (`@o`, `@i`): depth, and how tracks see each other's writes, as measured in GDSiON.

use sion::{Song, RATE};

fn render(mml: &str, frames: usize) -> Vec<f32> {
    let mut out = vec![0f32; frames * 2];
    Song::new(mml).render(&mut out);
    out.iter().step_by(2).copied().collect()
}

/// Magnitude of `x` at `hz` (a single-bin DFT over a Hann window).
fn at(x: &[f32], hz: f64) -> f64 {
    let n = x.len() as f64;
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n).cos();
        let p = 2.0 * std::f64::consts::PI * hz * i as f64 / RATE as f64;
        (re, im) = (re + v as f64 * w * p.cos(), im - v as f64 * w * p.sin());
    }
    (re * re + im * im).sqrt()
}

/// Upper sideband against the carrier: a 440 Hz sine phase-modulated by a 220 Hz one.
fn sideband(x: &[f32]) -> f64 { at(x, 660.0) / at(x, 440.0) }

/// Bessel function of the first kind, by its series.
fn j(n: i32, b: f64) -> f64 {
    let f = |k: i32| (1..=k).map(f64::from).product::<f64>();
    (0..30).map(|k| (-1f64).powi(k) / (f(k) * f(k + n)) * (b / 2.0).powi(2 * k + n)).sum()
}

const BLOCK: usize = 2048;

#[test]
fn depth_is_a_share_of_a_cycle() {
    // `@i1` and a modulator at a quarter of full volume: 2^(1-3) × 1/4 of a cycle, times 127/128 (GDSiON's depth),
    // so J1/J0 of beta = 2pi/16 × 127/128.
    let x = render("t120 %5@0 @o1,0 v4 o4a1^1; %5@0 @i1,0 v16 o5a1^1;", RATE as usize * 2);
    let b = 2.0 * std::f64::consts::PI / 16.0 * 127.0 / 128.0;
    let (got, want) = (sideband(&x[BLOCK..BLOCK + 32768]), j(1, b) / j(0, b));
    assert!((got / want - 1.0).abs() < 0.01, "J1/J0 {got} against {want}");
    // (The modulator writing to the pipe is not heard itself.)
    assert!(at(&x[BLOCK..BLOCK + 32768], 220.0) / at(&x[BLOCK..BLOCK + 32768], 660.0) < 1.1);
}

#[test]
fn a_later_track_is_read_a_block_late() {
    // SiON renders a block of frames track by track: a carrier before its modulator reads the block before
    // (silence at first), then modulates as deeply.
    let before = render("t120 %5@0 @o1,0 v4 o4a1^1; %5@0 @i1,0 v16 o5a1^1;", RATE as usize * 2);
    let after = render("t120 %5@0 @i1,0 v16 o5a1^1; %5@0 @o1,0 v4 o4a1^1;", RATE as usize * 2);
    let alone = render("t120 %5@0 v16 o5a1^1;", BLOCK);
    assert_eq!(after[..BLOCK], alone[..], "nothing to read in the first block");
    let (b, a) = (sideband(&before[BLOCK * 2..BLOCK * 2 + 32768]), sideband(&after[BLOCK * 2..BLOCK * 2 + 32768]));
    assert!((a / b - 1.0).abs() < 0.01, "{a} against {b}");
}

#[test]
fn adding_to_a_pipe_no_one_clears_builds_on_it() {
    // `@o2` with nothing overwriting the pipe adds onto what it held a block ago: the same as `@o1` for the first
    // block, then the depth wanders as the blocks' values pile up (220 Hz is a fifth of a cycle out each block).
    let add = render("t120 %5@0 @o2,0 v4 o4a1^1; %5@0 @i1,0 v16 o5a1^1;", RATE as usize * 3);
    let set = render("t120 %5@0 @o1,0 v4 o4a1^1; %5@0 @i1,0 v16 o5a1^1;", RATE as usize * 3);
    assert_eq!(add[..BLOCK], set[..BLOCK]);
    let depth: Vec<f64> = (0..12).map(|k| sideband(&add[k * 5000 + 2 * BLOCK..k * 5000 + 2 * BLOCK + 4096])).collect();
    let (lo, hi) = (depth.iter().copied().fold(f64::MAX, f64::min), depth.iter().copied().fold(0.0, f64::max));
    assert!(hi / lo > 2.0, "the depth varies: {depth:?}");
}

#[test]
fn pipes_first_used_mid_song_are_as_if_used_from_the_start() {
    // A song holds one frame of pipes until a pipe command runs. Here the first ones come partway in, a later
    // track's write (added to, `@o2`) read a block late by an earlier one: as with the pipes in use from the start
    // (`@o0,0`: output as usual).
    let mml = "t120 %5@0 v16 r1 @i2,0 o5a1^1; %5@0 v8 r1 @o2,0 o4a1^1;";
    let (late, early) = (render(mml, RATE as usize * 4), render(&format!("@o0,0 {mml}"), RATE as usize * 4));
    let alone = render("t120 %5@0 v16 r1 o5a1^1;", RATE as usize * 4);
    assert!(late != alone, "the pipe modulates");
    assert_eq!(late, early);
}
