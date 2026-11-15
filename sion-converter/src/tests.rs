//! PCM conversion of tracker modules: SiON's rendering of the converted MML must match the tracker
//! performance mixed straight from the samples (`play::render`).

use crate::tracker::*;
use crate::{module_to_mml, play, snr, Options};

/// (Without declicking: the reference mix doesn't ramp either, so the conversion itself is what's compared.)
fn render(mml: &str) -> Vec<f32> { crate::render_raw(mml, 60.0 * 30.0) }

/// Convert, render, and compare with the reference: (SNR in dB, MML).
fn compare(m: &Module) -> (f64, String) {
    let out = module_to_mml(m, &Options { exact: true, ..Options::default() });
    let (_, _, tr) = play::to_score(m, false, false);
    let want = play::render(m, &tr);
    let got = render(&out.mml);
    assert_eq!(render(&out.plain), got, "minified MML must sound like the plain conversion");
    (snr(&want, &got), out.mml)
}

fn cell(note: u8, ins: u8, fx: Fx) -> Cell { Cell { note: note + 1, ins, vol: Vol::None, fx } }
fn fx(fx: Fx) -> Cell { Cell { fx, ..Cell::default() } }

/// Pseudo-random noise, `n` frames, 8-bit values.
fn noise8(n: usize, decay: f32) -> Vec<f32> {
    let mut x = 12345u32;
    (0..n).map(|i| { x = x.wrapping_mul(1103515245).wrapping_add(12345); ((x >> 16) as u8 as i8 as f32 * (-(i as f32) * decay).exp()).round() / 128.0 }).collect()
}

/// A linear-slide (IT-style) module from `rows` of cells on `chans` channels.
fn module(kind: Kind, linear: bool, samples: Vec<Sample>, instruments: Vec<Instrument>, chans: usize, rows: Vec<Vec<Cell>>) -> Module {
    let cells = rows.iter().flat_map(|r| (0..chans).map(|c| r.get(c).copied().unwrap_or_default())).collect();
    Module {
        kind, chans, orders: vec![0], restart: 0, patterns: vec![Pattern { rows: rows.len(), cells }], instruments, samples, speed: 6, tempo: 125, gvol: 128,
        pan: vec![-32, 32, 0, 16][..chans].to_vec(), chvol: vec![64; chans], linear, compat_g: false, note_ofs: 0, warnings: vec![], title: String::new(),
    }
}

fn plain(n: usize) -> Vec<Instrument> {
    (0..n).map(|s| Instrument { map: (0..120).map(|k| (k as u8, s as u16 + 1)).collect(), venv: None, penv: None, fadeout: 0, gvol: 128, pan: None, nna: 0, name: String::new() }).collect()
}

fn samples() -> Vec<Sample> {
    // 1: 16-bit looped sine-ish cycle with harmonics; 2: 8-bit decaying noise one-shot; 3: 16-bit ping-pong saw.
    let tone: Vec<f32> = (0..64).map(|i| { let p = i as f32 / 64.0 * std::f32::consts::TAU; ((p.sin() * 0.6 + (3.0 * p).sin() * 0.2) * 32768.0).round() / 32768.0 }).collect();
    let saw: Vec<f32> = (0..300).map(|i| ((i as f32 / 300.0 * 1.6 - 0.8) * 32768.0).round() / 32768.0).collect();
    vec![
        Sample { data: tone, lp: Some((0, 64, false)), vol: 64, gvol: 64, pan: None, c4: 8363.0 * 2.0 },
        Sample { data: noise8(9000, 0.0004), lp: None, vol: 48, gvol: 64, pan: None, c4: 22050.0 },
        Sample { data: saw, lp: Some((100, 300, true)), vol: 56, gvol: 64, pan: None, c4: 16726.0 },
    ]
}

#[test]
fn samples_are_embedded_losslessly() {
    let m = module(Kind::Xm, true, samples(), plain(3), 1, vec![vec![cell(48, 1, Fx::None)], vec![cell(50, 2, Fx::None)], vec![cell(52, 3, Fx::None)]]);
    let out = module_to_mml(&m, &Options { exact: true, ..Options::default() });
    for (i, s) in m.samples.iter().enumerate() {
        let def = out.mml.split(';').find(|d| d.trim_start().starts_with(&format!("#PCMB{i}{{"))).unwrap_or_else(|| panic!("no #PCMB{i}"));
        // The definition decodes to exactly the source frames (up to the loop end).
        let want = crate::sample::as_sion(s);
        let flags: Vec<&str> = def.rsplit('}').next().unwrap().split(',').collect();
        // (Arguments at their defaults are left off the end: flags 0.)
        assert_eq!(flags.get(3).map_or("0", |f| f.trim()), match i { 0 => "1", 1 => "0", _ => "3" }, "bit depth and ping-pong flags {def:.40}");
        assert_eq!(want.data.len(), if i == 2 { 300 } else { s.data.len() });
    }
}

#[test]
fn linear_module_matches_the_reference() {
    use Fx::*;
    let mut ins = plain(3);
    // Instrument 2 rings on (IT new-note action "continue") with a volume envelope and fadeout.
    ins[1].nna = 1;
    ins[1].venv = Some(Env { pts: vec![(0, 64), (10, 40), (40, 20)], sustain: Option::None, lp: Option::None });
    ins[1].fadeout = 1024;
    let rows = vec![
        vec![cell(48, 1, None), cell(36, 2, None), cell(60, 3, Pan(200))],
        vec![fx(VolSlide(0x02)), fx(None), fx(PanSlide(0x20))],
        vec![cell(55, 1, Arp(0x37)), cell(38, 2, Offset(0x10)), fx(Vibrato(0x46))],
        vec![fx(PortaUp(0x08)), cell(36, 2, None), fx(Vibrato(0x00))],
        vec![cell(43, 1, TonePorta(0x10)), fx(None), cell(57, 3, Tremolo(0x58))],
        vec![fx(TonePorta(0x00)), cell(41, 2, Retrig(0x03)), fx(VolSlide(0x40))],
        vec![fx(Tempo(150)), fx(None), cell(62, 3, Offset(0x01))],
        vec![cell(50, 1, NoteCut(3)), cell(CUT - 1, 0, None), fx(PortaDown(0x10))],
        vec![fx(None), fx(None), fx(None)],
        vec![cell(52, 1, SetVol(20)), fx(None), cell(OFF - 1, 0, None)],
        vec![fx(None); 3], vec![fx(None); 3], vec![fx(None); 3], vec![fx(None); 3],
    ];
    let m = module(Kind::It, true, samples(), ins, 3, rows);
    let (db, mml) = compare(&m);
    println!("linear module: {db:.1} dB, {} bytes", mml.len());
    assert!(db > 45.0, "SiON's rendering differs from the tracker mix: {db:.1} dB");
}

#[test]
fn amiga_module_matches_the_reference() {
    use Fx::*;
    // Amiga periods don't fall on SiON's 1/64-semitone pitch grid; they are rounded to it (within 1/128 semitone).
    let rows = vec![
        vec![cell(24, 1, None), cell(36, 2, None), cell(31, 3, None)],
        vec![fx(VolSlide(0x01)), fx(None), fx(Vibrato(0x34))],
        vec![cell(27, 1, Offset(0x01)), cell(36, 2, Offset(0x08)), fx(Vibrato(0x00))],
        vec![fx(PortaDown(0x03)), fx(None), cell(34, 3, None)],
        vec![cell(29, 1, None), fx(None), fx(None)],
        vec![fx(None); 3], vec![fx(None); 3], vec![fx(None); 3],
    ];
    let m = module(Kind::Mod, false, samples(), plain(3), 3, rows);
    let (db, _) = compare(&m);
    println!("amiga module: {db:.1} dB");
    assert!(db > 50.0, "SiON's rendering differs from the tracker mix: {db:.1} dB");
}

/// `SION_DEBUG=file.mod[:channel] cargo test --release --lib debug_file -- --ignored --nocapture`: where a real
/// module's conversion departs from the reference (the plain MML is written to `sion_debug.mml` in the temp dir).
#[test]
#[ignore]
fn debug_file() {
    let Ok(arg) = std::env::var("SION_DEBUG") else { return };
    let (path, ch) = match arg.rsplit_once(':') { Some((p, c)) => (p.to_string(), c.parse::<usize>().ok()), None => (arg.clone(), None) };
    let mut m = load(&std::fs::read(&path).unwrap()).unwrap();
    if let Some(keep) = ch {
        let chans = m.chans;
        for p in &mut m.patterns {
            for (i, c) in p.cells.iter_mut().enumerate() {
                if i % chans == keep { continue }
                let global = matches!(c.fx, Fx::Speed(_) | Fx::Tempo(_) | Fx::Jump(_) | Fx::Break(_) | Fx::PatLoop(_) | Fx::PatDelay(_) | Fx::FinePatDelay(_) | Fx::GlobalVol(_) | Fx::GlobalVolSlide(_) | Fx::TempoSlide(_));
                *c = Cell { fx: if global { c.fx } else { Fx::None }, ..Cell::default() };
            }
        }
    }
    let out = module_to_mml(&m, &Options { exact: true, ..Options::default() });
    let (_, _, tr) = play::to_score(&m, false, false);
    let want = play::render(&m, &tr);
    let got = render(&out.mml);
    println!("total {:.1} dB", snr(&want, &got));
    let w = 2 * 4410;
    let segs: Vec<(usize, f64)> = (0..want.len() / w).map(|k| (k, snr(&want[k * w..(k + 1) * w], &got[k * w..(k + 1) * w]))).collect();
    println!("per 100ms: {}", segs.iter().take(120).map(|s| format!("{:.0}", s.1)).collect::<Vec<_>>().join(" "));
    if let Some(&(k, db)) = segs.iter().find(|s| s.1 < 35.0) {
        println!("first bad window {k} ({db:.1} dB) at {:.1}s", k as f64 / 10.0);
        let mut t = 0.0;
        for (i, s) in tr.secs.iter().enumerate() {
            if t >= k as f64 / 10.0 - 0.05 && t < k as f64 / 10.0 + 0.12 {
                let row: Vec<String> = tr.chans.iter().enumerate().filter_map(|(c, x)| x.get(i).copied().flatten().map(|p| format!("c{c}:s{} p{:.1} r{:.0} a{:.3} pan{}", p.smp, p.pos, p.rate, p.amp, p.pan))).collect();
                println!("tick {i} t={t:.3}: {}", row.join(" | "));
            }
            t += s;
        }
    }
    std::fs::write(std::env::temp_dir().join("sion_debug.mml"), &out.plain).unwrap();
}

#[test]
fn data_written_compactly() {
    use crate::mml::compact;
    // Runs of one value as `(v)n` where shorter; other commands and syntax as they are.
    assert_eq!(compact("#WAV0{126,126,-18,-18,-18,-18,0,0,5}"), "#WAV0{(126)2,(-18)4,0,0,5}");
    assert_eq!(compact("#TABLE3{1,1,1,1}"), "#TABLE3{(1)4}");
    assert_eq!(compact("#TABLE2{(0,-768)4 -768}"), "#TABLE2{(0,-768)4 -768}");
    assert_eq!(compact("#OPN@0{1,1,1,1}"), "#OPN@0{1,1,1,1}");
    // (Runs stop at a table's loop point.)
    assert_eq!(compact("#TABLE4{0,0,0,0,|5,5,5,5,-3}"), "#TABLE4{(0)4,|(5)4,-3}");
    assert_eq!(compact("#TABLE5{|7,7,7,7,8}"), "#TABLE5{|(7)4,8}");
    // `#PCMB` arguments at their defaults (key 60, no loop, flags 0, level 0) left off the end.
    assert_eq!(compact("#PCMB0{AAAA}8400,60,-1,0"), "#PCMB0{AAAA}8400");
    assert_eq!(compact("#PCMB1{AAAA}8400,60,10,0"), "#PCMB1{AAAA}8400,60,10");
    assert_eq!(compact("#PCMB2{=1}8400,60,-1,4,30"), "#PCMB2{=1}8400,60,-1,4,30");
}
