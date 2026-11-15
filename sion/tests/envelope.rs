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

/// The whole render (interleaved stereo), without declicking.
fn render(mml: &str, frames: usize) -> Vec<f32> {
    let mut song = Song::new(mml);
    song.set_declick(false);
    let mut out = vec![0f32; frames * 2];
    song.render(&mut out);
    out
}

#[test]
fn table_steps_land_where_the_ticks_do() {
    // As in SiON, a table's first value plays from the key-on and each next one an envelope frame (#FPS) later:
    // at t150 a 1/96 note is 735 samples, one 60 fps frame, so the table steps with the `x` changes slurred in
    // the first song, sample for sample. (Pitch too: `np` against `k`.)
    let voice = "#OPN@0{4,6,31,0,0,15,0,22,0,1,7,0,18,10,0,15,1,0,0,4,7,0,31,0,0,15,0,23,0,2,3,0,18,10,0,15,1,0,0,2,3,0};";
    let slurs = render(&format!("#VMODE{{%v1,1 %x1}};{voice} t150 q8 %6@0 l96 v120 o5c&x118c&x108k10c&k20c4 r8;"), RATE as usize);
    let table = render(&format!("#VMODE{{%v1,1 %x1}};{voice} #TABLE0{{128,118,108}};#TABLE1{{0,0,10,20}}; t150 q8 %6@0 l96 na0 np1 v120 o5c4^96^96^96 r8;"), RATE as usize);
    assert!(slurs == table, "first difference at sample {:?}", slurs.iter().zip(&table).position(|(a, b)| a != b).map(|i| i / 2));
}
