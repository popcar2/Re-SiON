//! Seeking: `Song::skip`, `Song::position`, `Song::length`.

use sion::{Song, RATE};

const MML: &str = "t150 @1 o5 l8 cdefgab<c; @3 o3 l4 c e g e c e g e; %6 l16 c r c r c c r c;";

#[test]
fn skip_lands_where_render_does() {
    // Skipping leaps the synthesis state along rather than rendering it, so it lands close to, not exactly,
    // where rendering does: oscillator phases differ by rounding (enough to shuffle noise), so compare the
    // loudness over each 10 ms (past the first, where the skip fades in).
    let (mut a, mut b) = (Song::new(MML), Song::new(MML));
    let mut buf = vec![0f32; 2 * 60_000];
    a.render(&mut buf);
    b.skip(60_000);
    assert_eq!((a.position(), b.position()), (60_000, 60_000));
    let (mut x, mut y) = (vec![0f32; 2 * 22050], vec![0f32; 2 * 22050]);
    a.render(&mut x);
    b.render(&mut y);
    let env = |v: &[f32]| v.chunks(882).skip(1).map(|c| c.iter().map(|s| s * s).sum::<f32>().sqrt()).collect::<Vec<_>>();
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

#[test]
fn skip_fades_in() {
    // After a skip the output ramps up from silence rather than starting mid-wave; with declicking off it
    // starts exactly where it is.
    let first = |declick: bool| {
        let mut s = Song::new("%5@0 o5 l1 a;");
        s.set_declick(declick);
        s.skip(10_000);
        let mut buf = vec![0f32; 2 * 512];
        s.render(&mut buf);
        buf
    };
    let (on, off) = (first(true), first(false));
    let peak = |v: &[f32]| v.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak(&off[..16]) > 0.01, "should land mid-note");
    assert!(peak(&on[..2]) < peak(&off) * 0.01, "first frame {}", on[0]);
    assert_eq!(on[2 * 300..], off[2 * 300..], "fully faded in by then");
}

#[test]
fn skip_crossfades_from_what_played() {
    // Skipping mid-playback carries on from the audio that would have come next, so the output doesn't jump;
    // after `forget_rendered` it fades in from silence instead.
    const SINE: &str = "%5@0 o5 l1 a;";
    let mut next = Song::new(SINE);
    let mut buf = vec![0f32; 2 * 10_001];
    next.render(&mut buf);
    let (last, then) = ((buf[2 * 9_999], buf[2 * 9_999 + 1]), (buf[2 * 10_000], buf[2 * 10_000 + 1]));
    for forget in [false, true] {
        let mut s = Song::new(SINE);
        let mut out = vec![0f32; 2 * 10_000];
        s.render(&mut out);
        assert_eq!((out[2 * 9_999], out[2 * 9_999 + 1]), last);
        if forget { s.forget_rendered() }
        s.skip(10_037); // (lands at another phase of the wave)
        let mut out = vec![0f32; 2 * 512];
        s.render(&mut out);
        if forget {
            assert!(out[0].abs() + out[1].abs() < 0.01, "should start from silence: {:?}", &out[..2]);
        } else {
            assert!((out[0] - then.0).abs() + (out[1] - then.1).abs() < 0.01, "{:?} after {then:?}", &out[..2]);
        }
        let step = out.windows(4).map(|w| (w[2] - w[0]).abs()).fold(0f32, f32::max);
        assert!(step < 0.05, "jumps by {step}");
    }
}
