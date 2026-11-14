//! Jumps from key-ons and volume and pan changes are smoothed over; attacks from silence stay sharp.

use sion::{Song, RATE};

fn left(mml: &str, secs: f32) -> Vec<f32> {
    let mut out = vec![0f32; (secs * RATE) as usize * 2];
    Song::new(mml).render(&mut out);
    out.iter().step_by(2).copied().collect()
}

/// Largest change from one sample to the next.
fn step(x: &[f32]) -> f32 { x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max) }

fn peak(x: &[f32]) -> f32 { x.iter().map(|v| v.abs()).fold(0.0, f32::max) }

// `%5@0` is a sine: a low one moves only a little from sample to sample, so any jump stands out.

#[test]
fn a_retrigger_doesnt_jump() {
    // Each key-on restarts the wave from its first frame, wherever the note before it was.
    let x = left("t120 %5@0 o3 l16 v16 [c e g e]4", 1.0);
    let x = &x[2000..];
    assert!(step(x) < 0.05 * peak(x), "a jump of {} at a level of {}", step(x), peak(x));
}

#[test]
fn a_pan_flip_and_a_volume_drop_dont_jump() {
    let x = left("t120 %5@0 o3 l16 v16 [@p-64 c & @p64 c & @p-64 v4 c & v16 c &]4 c", 1.0);
    let x = &x[2000..];
    // (Unsmoothed, the left channel drops by 3/4 of its level at once.)
    assert!(step(x) < 0.08 * peak(x), "a jump of {} at a level of {}", step(x), peak(x));
}

#[test]
fn an_attack_from_silence_stays_sharp() {
    // Started at the top of its cycle (`@ph64`), the sine starts at full level.
    let x = left("%5@0 @ph64 v16 o5a4", 0.2);
    let first = x.iter().position(|v| v.abs() > 1e-4).unwrap();
    assert!(x[first].abs() > 0.9 * peak(&x), "first sample {} of peak {}", x[first], peak(&x));
}

#[test]
fn it_can_be_turned_off() {
    // Off, the retriggers jump as SiON's do.
    let mml = "t120 %5@0 o3 l16 v16 [c e g e]4";
    let mut song = Song::new(mml);
    song.set_declick(false);
    let mut out = vec![0f32; RATE as usize * 2];
    song.render(&mut out);
    let x: Vec<f32> = out.iter().step_by(2).copied().collect();
    let x = &x[2000..];
    assert!(step(x) > 0.1 * peak(x), "a jump of {} at a level of {}", step(x), peak(x));
    assert!(step(x) > 2.0 * step(&left(mml, 1.0)[2000..]));
}
