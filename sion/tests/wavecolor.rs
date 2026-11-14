//! `#WAVCOLOR` wavetables (`%4`).

use sion::Song;

fn render(mml: &str) -> Vec<f32> {
    let mut out = vec![0f32; 8192 * 2];
    Song::new(mml).render(&mut out);
    out.iter().step_by(2).copied().collect()
}

#[test]
fn wave_color_matches_the_same_wav() {
    // `f` in the lowest digit alone is a full-scale sine, so it should sound like a sine `#WAV`.
    let sine: Vec<String> = (0..64).map(|i| ((i as f32 + 0.5) / 64.0 * core::f32::consts::TAU).sin().mul_add(127.0, -0.5).round().to_string()).collect();
    let a = render("#WAVCOLOR0{0000000f}; %4@0 o5 c1;");
    let b = render(&format!("#WAV0{{{}}}; %4@0 o5 c1;", sine.join(",")));
    let c = render("#WAVC0{0x0000000f}; %4@0 o5 c1;");
    let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
    assert!(rms(&a) > 0.01);
    assert!((rms(&a) / rms(&b) - 1.0).abs() < 0.02, "{} vs {}", rms(&a), rms(&b));
    assert_eq!(a, c);
}

#[test]
fn top_digit_picks_the_base_wave() {
    // A lone fundamental on base wave 1 (`%5@1`, saw) sounds like the saw itself.
    let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
    let saw = render("#WAVCOLOR0{1000000f}; %4@0 o5 c1;");
    let sine = render("#WAVCOLOR0{0000000f}; %4@0 o5 c1;");
    let five = render("%5@1 o5 c1;");
    assert!((rms(&saw) / rms(&five) - 1.0).abs() < 0.02, "{} vs {}", rms(&saw), rms(&five));
    assert!((rms(&saw) / rms(&sine) - 1.0).abs() > 0.05);
}
