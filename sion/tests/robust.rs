//! MML typed by hand can be anything: songs must neither crash nor go to NaN.

use sion::Song;

fn render(mml: &str) -> Vec<f32> {
    let mut out = vec![0f32; 2 * 44100];
    Song::new(mml).render(&mut out);
    out
}

#[test]
fn a_voice_that_selects_itself_does_not_recurse_forever() {
    let v = "1,0,0, 0,31,0,0,8,0,30,0,1,0,0,0,0,0,0";
    for mml in [format!("#@1{{{v}}}@1; %6 @1 o5c4;"), format!("#@1{{{v}}}@2; #@2{{{v}}}@1; %6 @1 o5c4;"), "#PCMVOICE1{64}%7@1; %7 @1 o5c4;".into()] {
        render(&mml);
    }
}

#[test]
fn filters_at_0_hz_stay_finite() {
    for f in ["lf", "hf", "bf", "nf", "pf", "af", "lb", "hb"] {
        let out = render(&format!("#EFFECT0{{{f}0}}; %5 o5c1;"));
        assert!(out.iter().all(|v| v.is_finite()), "{f}0");
        assert!(out.iter().any(|v| *v != 0.0), "{f}0");
    }
}
