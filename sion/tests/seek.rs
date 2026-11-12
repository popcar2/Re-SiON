//! Seeking: `Song::skip`, `Song::position`, `Song::length`.

use sion::{Song, RATE};

const MML: &str = "t150 @1 o5 l8 cdefgab<c; @3 o3 l4 c e g e c e g e; %6 l16 c r c r c c r c;";

#[test]
fn skip_lands_where_render_does() {
    // Skipping leaps the synthesis state along rather than rendering it, so it lands close to, not exactly,
    // where rendering does: oscillator phases differ by rounding (enough to shuffle noise), so compare the
    // loudness over each 10 ms.
    let (mut a, mut b) = (Song::new(MML), Song::new(MML));
    let mut buf = vec![0f32; 2 * 60_000];
    a.render(&mut buf);
    b.skip(60_000);
    assert_eq!((a.position(), b.position()), (60_000, 60_000));
    let (mut x, mut y) = (vec![0f32; 2 * 22050], vec![0f32; 2 * 22050]);
    a.render(&mut x);
    b.render(&mut y);
    let env = |v: &[f32]| v.chunks(882).map(|c| c.iter().map(|s| s * s).sum::<f32>().sqrt()).collect::<Vec<_>>();
    let (x, y) = (env(&x), env(&y));
    let sig = x.iter().map(|v| v * v).sum::<f32>().sqrt();
    let err = x.iter().zip(&y).map(|(p, q)| (p - q) * (p - q)).sum::<f32>().sqrt();
    assert!(sig > 1.0, "should land while notes play: {sig}");
    assert!(err < sig * 0.02, "error {err} of {sig}");
}

#[test]
fn length_is_one_pass() {
    // 8 eighth notes at t150: 1.6 seconds, with or without a `$` loop.
    for mml in ["t150 l8 cdefgab<c;", "t150 l8 c$defgab<c;"] {
        let n = Song::length(mml);
        assert!((n as i64 - 16 * RATE as i64 / 10).abs() < 64, "{mml}: {n}");
    }
}

#[test]
fn skip_past_the_end_finishes() {
    let mut s = Song::new("l8 cdef;");
    assert!(!s.skip(10 * RATE as u64));
    assert!(Song::new("l8 c$def;").skip(10 * RATE as u64));
}
