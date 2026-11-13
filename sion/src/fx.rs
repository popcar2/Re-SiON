//! SiON's `#EFFECT` units, processing interleaved stereo frames in place.

use crate::synth::{Table, RATE};
use crate::m::M;
use alloc::{vec, vec::Vec};
use core::f32::consts::{PI, TAU};

const NAMES: [&[u8]; 22] = [b"delay", b"reverb", b"chorus", b"autopan", b"ws", b"dist", b"comp", b"ds", b"speaker", b"stereo", b"eq", b"lf", b"hf", b"bf", b"nf", b"pf", b"af", b"lb", b"hb", b"nlf", b"nhf", b"vowel"];

pub struct Fx { k: usize, p: [f32; 8], s: Vec<f32>, i: usize, c: i32, t: [Option<Table>; 2] }

/// Biquad coefficients [b0, b1, b2, a1, a2] for filter kind 11..=18 (lf hf bf nf pf af lb hb).
fn biquad(k: usize, freq: f32, band: f32, gain: f32) -> [f32; 5] {
    let (omg, a) = (freq * TAU / RATE, 10f32.powf(gain * 0.025));
    let (cos, sin) = (omg.cos(), omg.sin());
    let alp = sin * (0.34657359 * band * omg / sin).sinh();
    let ia0 = 1.0 / (1.0 + alp);
    let (a1, a2) = (-2.0 * cos * ia0, (1.0 - alp) * ia0);
    match k {
        11 => [(1.0 - cos) * ia0 / 2.0, (1.0 - cos) * ia0, (1.0 - cos) * ia0 / 2.0, a1, a2],
        12 => [(1.0 + cos) * ia0 / 2.0, -(1.0 + cos) * ia0, (1.0 + cos) * ia0 / 2.0, a1, a2],
        13 => [alp * ia0, 0.0, -alp * ia0, a1, a2],
        14 => [1.0, -(1.0 + cos) * ia0, 1.0, a1, a2],
        15 => { let i = 1.0 / (1.0 + alp / a); [(1.0 + alp * a) * i, -2.0 * cos * i, (1.0 - alp * a) * i, -2.0 * cos * i, (1.0 - alp / a) * i] }
        16 => [a2, a1, 1.0, a1, a2],
        _ => {
            let (c, sq) = (if k == 17 { cos } else { -cos }, sin * 0.5 * ((a + 1.0 / a) * (1.0 / band.max(1.0) - 1.0) + 2.0).sqrt() * a.sqrt() * 2.0);
            let (i, s) = (1.0 / ((a + 1.0) + (a - 1.0) * c + sq), if k == 17 { 1.0 } else { -1.0 });
            [((a + 1.0) - (a - 1.0) * c + sq) * a * i, s * 2.0 * ((a - 1.0) - (a + 1.0) * c) * a * i, ((a + 1.0) - (a - 1.0) * c - sq) * a * i,
             s * -2.0 * ((a - 1.0) + (a + 1.0) * c) * i, ((a + 1.0) + (a - 1.0) * c - sq) * i]
        }
    }
}

impl Fx {
    /// Build an effect from its MML name and arguments (NaN = default).
    pub fn new(name: &[u8], a: &[f32; 16], tables: &[Option<Table>]) -> Option<Fx> {
        let k = NAMES.iter().position(|n| *n == name)?;
        let g = |i: usize, d: f32| if a[i].is_nan() { d } else { a[i] };
        let (mut p, mut s, mut t) = ([0.0; 8], vec![], [None, None]);
        match k {
            0 => { p = [(g(0, 250.0) * (RATE / 1000.0)).min(65535.0), (g(1, 25.0) / 100.0).clamp(-0.999, 0.999), (a[2] == 1.0) as i32 as f32, g(3, 100.0) / 100.0, 0., 0., 0., 0.]; s = vec![0.0; 131072] }
            1 => { p = [g(0, 70.0) / 100.0, g(1, 40.0) / 100.0, (g(2, 80.0) / 100.0).clamp(-0.99, 0.99), g(3, 100.0) / 100.0, 0., 0., 0., 0.]; s = vec![0.0; 16384] }
            2 => {
                let off = (g(0, 20.0) * (RATE / 1000.0)).clamp(5.0, 4095.0).floor();
                let (freq, depth) = (g(2, 4.0).max(0.01), g(3, 20.0).min(off - 4.0));
                let size = ((depth * TAU).floor()).min((RATE / 4.0 / freq).floor()).max(1.0);
                p = [off, (g(1, 20.0) / 100.0).clamp(-0.999, 0.999), size, depth, g(4, 50.0) / 100.0, if g(5, 1.0) != 0.0 { -1.0 } else { 1.0 }, (RATE / (size * freq)).floor().max(4.0), 0.];
                s = vec![0.0; 8192];
            }
            3 => p = [(RATE / 256.0 / (g(0, 1.0) * 0.5).max(1e-3)).floor().max(4.0), g(1, 100.0) / 100.0, 0., 0., 0., 0., 0., 0.],
            4 => { let d = (g(0, 50.0) / 100.0).min(0.99998); p[0] = 2.0 * d / (1.0 - d); p[1] = g(1, 100.0) / 100.0 }
            5 => {
                let post = (-g(1, 18.0) / 6.0).exp2();
                p[0] = (-g(0, -60.0) / 6.0).exp2() * post;
                p[1] = post;
                p[2] = (g(2, 2400.0) > 0.0) as i32 as f32;
                p[3..8].copy_from_slice(&biquad(11, g(2, 2400.0).max(1.0), g(3, 1.0), 0.0));
                s = vec![0.0; 4];
            }
            6 => {
                let w = (g(1, 50.0) * (RATE / 1000.0)).max(1.0).floor();
                p = [(g(0, 70.0) / 100.0).powi(2), w, if g(2, 20.0) == 0.0 { 0.5 } else { (-1.0 / (g(2, 20.0) * (RATE / 1000.0))).exp2() }, if g(3, 20.0) == 0.0 { 2.0 } else { (1.0 / (g(3, 20.0) * (RATE / 1000.0))).exp2() }, (g(4, 6.0) / 6.0).exp2(), g(5, 50.0) / 100.0, 2.0, 0.0];
                s = vec![0.0; w as usize];
            }
            7 => { p = [(1 << (g(0, 0.0) as u32).min(12)) as f32, (g(1, 16.0).clamp(1.0, 30.0)).exp2(), g(2, 2.0), 0., 0., 0., 0., 0.]; s = vec![0.0; 4] }
            8 => { p[0] = (1.0 - (g(0, 20.0) / 100.0).powi(2)).max(0.1); s = vec![0.0; 4] }
            9 => {
                let (w, c, inv) = (g(1, 140.0) / 100.0 * PI / 4.0, (g(2, 0.0) / 100.0 + 0.5) * PI / 2.0, if g(0, 0.0) != 0.0 { -1.0 } else { 1.0 });
                let mut m = [(c - w).cos(), (c - w).sin(), (c + w).cos() * inv, (c + w).sin() * inv];
                let l = ((m[0] + m[2]).powi(2) + (m[1] + m[3]).powi(2)).sqrt();
                if l > 0.01 { m = m.map(|v| v / l) }
                p[..4].copy_from_slice(&m);
                p[4] = (w == 0.0 && g(2, 0.0) == 0.0 && inv > 0.0) as i32 as f32;
            }
            10 => { p = [g(0, 100.0) / 100.0, g(1, 100.0) / 100.0, g(2, 100.0) / 100.0, 2.0 * (g(3, 880.0) * PI / RATE).sin(), 2.0 * (g(4, 5000.0) * PI / RATE).sin(), 0., 0., 0.]; s = vec![0.0; 22] }
            11..=18 => { p[..5].copy_from_slice(&biquad(k, g(0, [800., 5500., 3000., 3000., 3000., 3000., 3000., 5500.][k - 11]), g(1, 1.0), g(2, 6.0))); s = vec![0.0; 8] }
            21 => {
                // Six peaking formants (frequency snapped to 1/32 octave from 50 Hz), then output level.
                const D: [f32; 12] = [800., 30., 1300., 24., 2200., 12., 3500., 9., 4500., 6., 5500., 6.];
                p[0] = g(0, 100.0) / 100.0;
                for j in 0..6 {
                    let fi = ((g(1 + 2 * j, D[2 * j]).log2() - 5.643856) * 32.0).trunc().clamp(0.0, 1023.0);
                    s.extend(biquad(15, 50.0 * (fi / 32.0).exp2(), 1.0, g(2 + 2 * j, D[2 * j + 1]).trunc().clamp(-32.0, 95.0)));
                }
                s.resize(54, 0.0);
            }
            _ => {
                let tb = |i: usize| tables.get(g(i, 255.0) as usize).cloned().flatten().filter(|_| g(i, 255.0) < 255.0);
                t = [tb(0), tb(1)];
                p[0] = (RATE / g(2, 20.0).max(1.0)).floor().max(44.0);
                p[1] = t[0].as_ref().map_or(128.0, |t| t.0[0] as f32);
                p[2] = t[1].as_ref().map_or(0.0, |t| t.0[0] as f32 / 129.0);
                s = vec![0.0; 4];
            }
        }
        Some(Fx { k, p, s, i: 0, c: 0, t })
    }

    /// True if tracks should feed this effect in mono (no panning), as SiON does for these units.
    pub fn mono(&self) -> bool { self.k == 5 || self.k == 21 || (self.k == 3 && self.p[1] != 0.0) }

    #[cfg_attr(feature = "small", optimize(speed))]
    pub fn run(&mut self, buf: &mut [[f32; 2]]) {
        let (p, s) = (self.p, &mut self.s);
        for f in buf.iter_mut() {
            match self.k {
                0 => {
                    let (r, w) = (self.i, (self.i + p[0] as usize) & 65535);
                    for c in 0..2 {
                        let n = s[r * 2 + (c ^ p[2] as usize)];
                        s[w * 2 + c] = f[c] - n * p[1];
                        f[c] = f[c] * (1.0 - p[3]) + n * p[3];
                    }
                    self.i = (r + 1) & 65535;
                }
                1 => {
                    let r = self.i;
                    let at = |d: f32| (r + (8191.0 * (1.0 - d.clamp(0.01, 0.99))) as usize) & 8191;
                    for c in 0..2 {
                        let n = (s[r * 2 + c] * 0.2 + s[at(p[0]) * 2 + c] * 0.3 + s[at(p[1]) * 2 + c] * 0.5) * p[2];
                        s[((r + 8191) & 8191) * 2 + c] = f[c] - n;
                        f[c] = f[c] * (1.0 - p[3]) + n * p[3];
                    }
                    self.i = (r + 1) & 8191;
                }
                2 => {
                    let d = (((self.c / p[6] as i32 % p[2] as i32) as f32) * TAU / p[2]).sin() * p[3];
                    self.c = (self.c + 1) % (p[6] * p[2]) as i32;
                    let (r, w) = (self.i, (self.i + p[0] as usize) & 4095);
                    for c in 0..2 {
                        let n = s[((r as i32 + (d * if c == 1 { p[5] } else { 1.0 } + 0.5) as i32) & 4095) as usize * 2 + c];
                        s[w * 2 + c] = f[c] - n * p[1];
                        f[c] = f[c] * (1.0 - p[4]) + n * p[4];
                    }
                    self.i = (r + 1) & 4095;
                }
                3 => {
                    let w = if p[1] == 0.0 { 1.0 } else { p[1] } * PI / 256.0;
                    let tab = |j: usize| (PI / 2.0 + ((j & 255) as f32 - 128.0) * w).sin();
                    let (c, sn) = (tab(self.i), tab(self.i + 128));
                    *f = if p[1] == 0.0 { [f[0] * c - f[1] * sn, f[0] * sn + f[1] * c] } else { [f[0] * c, f[0] * sn] };
                    self.c += 1;
                    if self.c >= p[0] as i32 { self.c = 0; self.i += 1 }
                }
                4 => *f = f.map(|n| (1.0 + p[0]) * p[1] * n / (1.0 + p[0] * n.abs())),
                5 => {
                    let n = (f[0] * p[0]).clamp(-p[1], p[1]);
                    let o = if p[2] > 0.0 { p[3] * n + p[4] * s[0] + p[5] * s[1] - p[6] * s[2] - p[7] * s[3] } else { n };
                    (s[1], s[0], s[3], s[2]) = (s[0], n, s[2], o);
                    *f = [o, o];
                }
                6 => {
                    let e = f[0] * f[0] + f[1] * f[1];
                    self.i = (self.i + 1) % s.len();
                    self.p[7] += e - s[self.i];
                    s[self.i] = e;
                    self.p[6] = (self.p[6] * if self.p[7] / p[1] > p[0] { p[2] } else { p[3] }).min(p[4]);
                    *f = f.map(|v| (v * self.p[6]).clamp(-1.0, 1.0) * p[5]);
                }
                7 => {
                    if p[2] == 1.0 { *f = [(f[0] + f[1]) * 0.5; 2] }
                    s[0] += f[0];
                    s[1] += f[1];
                    self.c += 1;
                    if self.c >= p[0] as i32 {
                        for c in 0..2 { s[c + 2] = (s[c] / p[0] * p[1]).trunc() / p[1]; s[c] = 0.0 }
                        self.c = 0;
                    }
                    *f = [s[2], s[3]];
                }
                8 => for c in 0..2 {
                    s[c] = s[c] * p[0] + f[c] - s[c + 2];
                    s[c + 2] = f[c];
                    f[c] = s[c];
                }
                9 => *f = if p[4] > 0.0 { [(f[0] + f[1]) * 0.70710678; 2] } else { [f[0] * p[0] + f[1] * p[1], f[0] * p[2] + f[1] * p[3]] },
                10 => for c in 0..2 {
                    let (n, s) = (f[c], &mut s[c * 11..c * 11 + 11]);
                    for (b, cf) in [(0, p[3]), (4, p[4])] {
                        s[b] += cf * (n - s[b]) + 2.3283064e-10;
                        for j in 1..4 { s[b + j] += cf * (s[b + j - 1] - s[b + j]) }
                    }
                    let (l, h) = (s[3], s[10] - s[7]);
                    f[c] = l * p[0] + (s[10] - h - l) * p[1] + h * p[2];
                    (s[10], s[9], s[8]) = (s[9], s[8], n);
                }
                11..=18 => for c in 0..2 {
                    let s = &mut s[c * 4..c * 4 + 4];
                    let o = (p[0] * f[c] + p[1] * s[0] + p[2] * s[1] - p[3] * s[2] - p[4] * s[3]).clamp(-1.0, 1.0);
                    (s[1], s[0], s[3], s[2]) = (s[0], f[c], s[2], o);
                    f[c] = o;
                }
                21 => {
                    let mut x = f[0];
                    for j in 0..6 {
                        let (c, t) = (&s[j * 5..j * 5 + 5], 30 + j * 4);
                        let o = c[0] * x + c[1] * s[t] + c[2] * s[t + 1] - c[3] * s[t + 2] - c[4] * s[t + 3];
                        (s[t + 1], s[t], s[t + 3], s[t + 2], x) = (s[t], x, s[t + 2], o, o);
                    }
                    *f = [(x * p[0]).clamp(-1.0, 1.0); 2];
                }
                _ => {
                    let cut = (self.p[1] / 128.0).powi(2).min(1.0);
                    let fb = if self.p[1] >= 128.0 { 0.0 } else { self.p[2] * (1.0 + 1.0 / (1.0 - cut)) };
                    for c in 0..2 {
                        s[c] += cut * (f[c] - s[c] + fb * (s[c] - s[c + 2]));
                        s[c + 2] += cut * (s[c] - s[c + 2]);
                        f[c] = if self.k == 19 { s[c + 2] } else { f[c] - s[c] };
                    }
                    self.c += 1;
                    if self.c >= p[0] as i32 {
                        (self.c, self.i) = (0, self.i + 1);
                        for (j, sc) in [(0, 1.0), (1, 129.0)] {
                            if let Some(t) = &self.t[j] { self.p[j + 1] = crate::tval(t, self.i) as f32 / sc }
                        }
                    }
                }
            }
        }
    }
}
