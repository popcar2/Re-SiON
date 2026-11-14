//! Envelope rates scaled by the key (`kc >> 5`, with key scale rate 0).

use sion::{Song, RATE};

/// Level (dB) of each quarter second.
fn levels(mml: &str, secs: usize) -> Vec<f32> {
    let mut out = vec![0f32; secs * RATE as usize * 2];
    Song::new(mml).render(&mut out);
    let q = RATE as usize / 4;
    out.chunks(q * 2).map(|c| 10.0 * (c.iter().step_by(2).map(|x| x * x).sum::<f32>() / q as f32 + 1e-12).log10()).collect()
}

#[test]
fn a_tracks_first_note_has_its_keys_rates() {
    // Sustain rate 13 at o7e falls faster than at o2e (kc >> 5 = 3 against 0), the first note on the track as much
    // as the next: as in SiON, the key is set before the key-on, so the stage the key-on enters is scaled too.
    let fall = |l: &[f32], at: usize| (l[at + 1] - l[at + 9]) / 2.0; // dB/s over two seconds
    let hi = levels("t120 %0@0,63,0,13,50,0 o7e1^1^1 r1 o7e1^1^1;", 16);
    let lo = levels("t120 %0@0,63,0,13,50,0 o2e1^1^1;", 4);
    assert!((fall(&hi, 0) - fall(&hi, 32)).abs() < 0.05, "first {} and second {} note", fall(&hi, 0), fall(&hi, 32));
    // (Rate 16 against 13: 4 × 2^4 against 5 × 2^3 steps.)
    let r = fall(&hi, 0) / fall(&lo, 0);
    assert!((r - 1.6).abs() < 0.02, "o7e falls {r} times as fast as o2e");
}
