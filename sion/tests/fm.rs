//! FM feedback: SiON's (the last output) and the Yamaha chips' (`#FBMODE{chip}`: the sum of the last two).

use sion::Song;

fn render(mml: &str, frames: usize) -> Vec<f32> {
    let mut out = vec![0f32; frames * 2];
    Song::new(mml).render(&mut out);
    out.iter().step_by(2).copied().collect()
}

/// Algorithm 7 with S1 alone at full level, feedback `fb`.
fn voice(fb: i32) -> String {
    let op = |tl: i32| format!("31,0,0,15,0,{tl},0,1,0,0");
    format!("#OPN@0{{7,{fb},{},{},{},{}}};", op(0), op(127), op(127), op(127))
}

/// Power of the sample-to-sample difference against the signal's: high for a tone broken into noise.
fn roughness(x: &[f32]) -> f32 {
    let x = &x[4410..];
    x.windows(2).map(|w| (w[1] - w[0]).powi(2)).sum::<f32>() / x.iter().map(|v| v * v).sum::<f32>()
}

#[test]
fn chip_feedback_is_the_same_depth() {
    // Shallow feedback barely changes from one output to the next: both ways give the same tone.
    let (a, b) = (render(&format!("{}%6@0o4a1;", voice(3)), 22050), render(&format!("#FBMODE{{chip}};{}%6@0o4a1;", voice(3)), 22050));
    let diff: f32 = a.iter().zip(&b).map(|(x, y)| (x - y).powi(2)).sum::<f32>() / a.iter().map(|v| v * v).sum::<f32>();
    assert!(diff < 1e-3, "{diff}");
}

#[test]
fn chip_feedback_stays_tonal() {
    // At level 5 SiON's loop already breaks into oscillation at the sample rate; the chips' sum cancels there.
    let (sion, chip) = (roughness(&render(&format!("{}%6@0o4a1;", voice(5)), 22050)), roughness(&render(&format!("#FBMODE{{chip}};{}%6@0o4a1;", voice(5)), 22050)));
    assert!(chip * 4.0 < sion, "chip {chip}, SiON {sion}");
}
