//! One SiON sound channel: up to four FM operators, LFO, state-variable filter, plucked string, PCM sample.

use crate::m::M;
use alloc::{vec, vec::Vec};
use core::f32::consts::TAU;

pub const RATE: f32 = 44100.0;
pub const MIN: i32 = i32::MIN;
const OFF: f32 = 832.0; // envelope level (1/64 octave units) treated as silence

/// Envelope table: values and optional loop point.
pub type Table = (Vec<i32>, Option<usize>);

#[derive(Clone)]
pub struct Voice { pub n: usize, pub alg: usize, pub fb: i32, pub fbc: usize, pub fratio: f32, pub ops: Vec<Op>, pub init: Vec<u8> }

/// A PCM sample for `%7` (pitched) and `%10` (sampler): frames in -1..1, played at `rate` frames per second
/// at note `key` (MIDI numbering, 69 = `o5a` = 440 Hz). With a loop, playback continues from frame `lp` after
/// the last frame (`bidi`: back and forth, with the end frames played once per turn); without one it stops there.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample { pub data: Vec<f32>, pub rate: f32, pub key: i32, pub lp: Option<usize>, pub bidi: bool }

impl Sample {
    /// Loop period in frames, if the sample loops.
    fn period(&self) -> Option<(usize, usize)> {
        let n = self.data.len();
        let l = self.lp.filter(|&l| l < n)?;
        Some((l, if self.bidi && n - l > 1 { 2 * (n - l) - 2 } else { n - l }))
    }

    /// Frame `u` counted from the start along the loop (0 past the end of a one-shot).
    #[cfg_attr(feature = "small", optimize(speed))]
    pub fn at(&self, u: usize) -> f32 {
        let n = self.data.len();
        if u < n { return self.data[u] }
        let Some((l, per)) = self.period() else { return 0.0 };
        let k = (u - l) % per;
        self.data[if k < n - l { l + k } else { l + per - k }]
    }

    /// Bring a position back into the loop's first period; None once a one-shot has ended.
    pub fn wrap(&self, u: usize) -> Option<usize> {
        if u < self.data.len() { return Some(u) }
        self.period().map(|(l, per)| l + (u - l) % per)
    }
}

pub struct Shared {
    pub samples: Vec<Option<Sample>>,
    /// Which samples are NES DPCM (`#PCMB` flags 4): their frames are the level of the console's delta modulation
    /// channel, which a channel playing them keeps the way the console does (see `Chan::dpcm`).
    pub dpcm: Vec<bool>,
    pub waves: Vec<Vec<f32>>,
    pub noise: [Vec<f32>; 6],
    pub tables: Vec<Option<Table>>,
    pub voices: Vec<Option<Voice>>,
    pub fps: f32,
    pub quant: i32,
    pub tmode: (u8, f64),
    pub vmode: (i32, i32, i32),
    pub rev: (i32, i32),
    /// `#FBMODE{chip}` (an extension): FM feedback from the sum of the operator's last two outputs at half the
    /// depth each, as the Yamaha chips do; SiON feeds back the last output alone.
    pub fb2: bool,
    pub sign: [i32; 7],
    /// 1024-step sine (as SiON's wave tables) and constant-power pan gains for pan 0..128.
    pub sine: Vec<f32>,
    pub pan: Vec<f32>,
    /// Gain for attenuation -256..2304 in 1/64 octave units, at 1/256 octave steps (SiON's log table resolution is similar).
    pub att: Vec<f32>,
}

impl Shared {
    pub fn sample(&self, i: usize) -> Option<&Sample> { self.samples.get(i)?.as_ref() }
    pub fn voice(&self, i: usize) -> Option<&Voice> { self.voices.get(i)?.as_ref() }
}

/// White, pulse, short, high-pass, pink and Game Boy noise tables.
pub fn noise() -> [Vec<f32>; 6] {
    let (mut w, mut p, mut j) = (vec![], vec![], 1i32);
    for _ in 0..32768 {
        j = (((j << 13) ^ (j << 14)) & 0x4000) | (j >> 1);
        w.push((j & 0x7fff) as f32 / 16384.0 - 1.0);
        p.push(if j & 1 == 1 { 1.0 } else { -1.0 });
    }
    j = 1;
    let short = (0..1024).map(|_| { j = (((j << 8) ^ (j << 14)) & 0x4000) | (j >> 1); if j & 1 == 1 { 1.0 } else { -1.0 } }).collect();
    let (mut j, mut o) = (0xffffi32, 0);
    let gb = (0..128).map(|_| { j = j.wrapping_add(j).wrapping_add(((j >> 6) ^ (j >> 5)) & 1); o ^= j & 1; if o == 1 { 1.0 } else { -1.0 } }).collect();
    let mut v = (w[0] - w[32767]) * 0.0625;
    let hi = (0..32768).map(|i| { if i > 0 { v = (v + w[i] - w[i - 1]) * 0.0625 } v * 16.0 }).collect();
    let mut b = [0f32; 3];
    let pink = w.iter().map(|&x| {
        b = [0.99765 * b[0] + x * 0.099046, 0.963 * b[1] + x * 0.2965164, 0.57 * b[2] + x * 1.0526913];
        (b[0] + b[1] + b[2] + x * 0.1848) * 0.125
    }).collect();
    [w, p, short, hi, pink, gb]
}

fn sine(sh: &Shared, p: f32) -> f32 { sh.sine[(p * 1024.0) as usize & 1023] }

/// Build the shared sine, pan and attenuation tables.
pub fn tables() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    ((0..1024).map(|i| (i as f32 * TAU / 1024.0).sin()).collect(), (0..=128).map(|i| (i as f32 * core::f32::consts::PI / 256.0).sin()).collect(),
     (0..10240).map(|i| (-(i as f32 / 4.0 - 256.0) / 64.0).exp2()).collect())
}

/// 2^(-x/64) by table lookup; silent past the end.
fn gain(sh: &Shared, x: f32) -> f32 { sh.att.get(((x + 256.0) * 4.0).max(0.0) as usize).copied().unwrap_or(0.0) }

/// 2^e for small integer e, built from the float's bits.
fn pow2(e: i32) -> f32 { f32::from_bits(((e.clamp(-126, 127) + 127) as u32) << 23) }

/// Wrap a phase into 0..1 (fast path when it already is).
fn wrap(x: f32) -> f32 { if (0.0..1.0).contains(&x) { x } else { x.rem_euclid(1.0) } }

/// Move a sample cursor on by `d` frames: whole frames into `pos`, the rest kept in `frac`.
fn advance(pos: &mut usize, frac: &mut f32, d: f32) {
    *frac += d;
    let k = *frac as usize;
    (*pos, *frac) = (*pos + k, *frac - k as f32);
}

/// `p % 0.5` for a phase in 0..1.
fn half(p: f32) -> f32 { if p >= 0.5 { p - 0.5 } else { p } }

fn tri(p: f32) -> f32 { if p < 0.25 { 4.0 * p } else if p < 0.75 { 2.0 - 4.0 * p } else { 4.0 * p - 4.0 } }
fn saw(p: f32) -> f32 { if p < 0.5 { 2.0 * p } else { 2.0 * p - 2.0 } }

/// Sample waveform `pg` (SiON "pgType") at phase `p` in 0..1.
#[cfg_attr(feature = "small", optimize(speed))]
pub fn wave(sh: &Shared, pg: i32, p: f32) -> f32 {
    const KNM: [i8; 32] = [-80, -112, -16, 96, 64, 16, 64, 96, 32, -16, 64, 112, 80, 0, 32, 48, -16, -96, 0, 80, 16, -64, -48, -16, -96, -128, -80, 0, -48, -112, -80, -32];
    let at = |t: &[f32], p: f32| if t.is_empty() { 0.0 } else { t[((p * t.len() as f32) as usize).min(t.len() - 1)] };
    let b = |v: bool| if v { 1.0 } else { -1.0 };
    match pg {
        0 => sine(sh, p),
        1 => saw(p),
        2 => 1.0 - 2.0 * p,
        3 => { let i = (p * 32.0) as i32; (match i { 0..=7 => i, 8..=23 => 15 - i, _ => i - 32 }) as f32 / 8.0 }
        4 => tri(p),
        5 => b(p < 0.5),
        6 | 16 => at(&sh.noise[0], p),
        17 | 26 => at(&sh.noise[1], p),
        18..=21 => at(&sh.noise[pg as usize - 16], p),
        7 => KNM[(p * 32.0) as usize & 31] as f32 / 128.0,
        8 => -p,
        9 => p,
        11 => -0.96875 + (p * 32.0).floor() * 0.0625,
        24 => (p < 0.0625) as i32 as f32,
        25 => at(&sh.noise[2], p / 11.0),
        32..=63 => {
            let (g, v) = ((pg - 32) / 8, (pg - 32) % 8);
            let base = |q: f32| match g {
                0 => sine(sh, q),
                1 => sine(sh, q + if q < 0.125 { q } else if q < 0.375 { 0.25 - q } else if q < 0.625 { q - 0.5 } else if q < 0.875 { 0.75 - q } else { q - 1.0 }),
                2 => tri(q),
                _ => saw(q),
            };
            match v {
                0 => base(p),
                1 => if p < 0.5 { base(p) } else { 0.0 },
                2 => base(half(p)),
                3 => if half(p) < 0.25 { base(half(p)) } else { 0.0 },
                4 => if p < 0.5 { base(2.0 * p) } else { 0.0 },
                5 => if p < 0.5 { base(half(2.0 * p)) } else { 0.0 },
                6 => match g { 0 => b(p < 0.5), 1 => (p < 0.5) as i32 as f32, 2 => (half(p) < 0.25) as i32 as f32, _ => (p < 0.25) as i32 as f32 },
                _ if g > 0 => 0.0,
                _ => if p < 0.25 { 1.0 - sine(sh, p) } else if p >= 0.75 { sine(sh, 1.0 - p) - 1.0 } else { 0.0 },
            }
        }
        64..=79 => b(p * 16.0 < (pg - 64) as f32),
        80..=95 => { let (x, j) = (p * 32.0, (pg - 80) as f32); if x < j { 1.0 } else if x < 2.0 * j { -1.0 } else { 0.0 } }
        128..=255 => {
            let k = pg - 128;
            let j = (k - 64).abs();
            if j > 59 { return if k < 64 { saw(p) } else { 1.0 - 2.0 * p } }
            let iv = 256 >> (j >> 3);
            let a = (512 - iv + ((iv * (j & 7)) >> 4)) as f32 / 512.0;
            let v = |x: f32| if x < a { x / a } else { 1.0 - (x - a) / (1.0 - a) };
            match (k < 64, p < 0.5) { (true, true) => v(2.0 * p), (true, false) => -v(2.0 - 2.0 * p), (false, true) => v(1.0 - 2.0 * p), _ => -v(2.0 * p - 1.0) }
        }
        256..=511 => at(&sh.waves[(pg - 256) as usize & 255], p),
        _ => 0.0,
    }
}

/// Convert a velocity/expression value (0..512) to attenuation, per volume mode.
fn vtl(mode: i32, i: i32) -> f32 {
    match (i, mode) {
        (..=0, _) => OFF,
        (449.., _) => -192.0,
        (257.., _) => (256 - i) as f32,
        (_, 1) => (256 - i) as f32 * 4.0,
        (_, 2) => (256 - i) as f32 * 2.6667,
        (_, 3) => (256 - i) as f32 * 2.0,
        (_, 4) => (256 - i) as f32 * 1.3333,
        _ => -64.0 * (i as f32 / 256.0).log2(),
    }
}

#[derive(Clone)]
pub struct Op {
    pub pg: i32, pub pt: u8, pub ar: i32, pub dr: i32, pub sr: i32, pub rr: i32, pub sl: i32, pub tl: i32, pub ksr: i32, pub ksl: i32,
    pub fmul: i32, pub dt1: i32, pub det: i32, pub ams: i32, pub phase: i32, pub fix: i32, pub erst: bool,
    /// SSG-type envelope (`@se` 8..17, 0 = off), its loop count and level-mapping table.
    pub ssg: i32, ssg_st: usize, ltab: u8,
    pi: i32, pf: f32, kc: i32, ph: f32, step: f32, lv: f32, st: u8, rate: f32, tgt: f32, out: f32, fin: bool, tlo: f32,
    /// The output before `out` (for `#FBMODE{chip}`).
    pout: f32,
}

impl Op {
    /// `chan`: the channel's initial operator (square) rather than a voice definition's default (sine).
    pub fn new(chan: bool) -> Op {
        Op {
            pg: if chan { 5 } else { 0 }, pt: 0, ar: 63, dr: 0, sr: 0, rr: if chan { 28 } else { 63 }, sl: 0, tl: 0, ksr: !chan as i32, ksl: 0,
            fmul: 128, dt1: 0, det: 0, ams: chan as i32, phase: 0, fix: 0, erst: false, ssg: 0, ssg_st: 0, ltab: 0,
            pi: 0, pf: 0.0, kc: 0, ph: 0.0, step: 0.0, lv: OFF, st: 0, rate: 0.0, tgt: 0.0, out: 0.0, fin: true, tlo: 0.0, pout: 0.0,
        }
    }

    pub fn mul(&mut self, m: i32) { self.fmul = if m & 15 > 0 { (m & 15) << 7 } else { 64 } }

    /// Default pitch table of a waveform: sampled noise runs at a rate, not a frequency.
    pub fn pt_of(pg: i32) -> u8 { match pg { 6 | 16..=21 => 1, 26 => 3, _ => 0 } }

    #[cfg_attr(feature = "small", optimize(speed))]
    fn pitch(&mut self, sh: &Shared, pm: i32) {
        const DT1: [u8; 96] = *b"\0\0\0\0\x01\x01\x01\x01\x01\x01\x01\x01\x02\x02\x02\x02\x02\x03\x03\x03\x04\x04\x04\x05\x05\x06\x06\x07\x08\x08\x08\x08\
\x01\x01\x01\x01\x02\x02\x02\x02\x02\x03\x03\x03\x04\x04\x04\x05\x05\x06\x06\x07\x08\x08\x09\x0a\x0b\x0c\x0d\x0e\x10\x10\x10\x10\
\x02\x02\x02\x02\x02\x03\x03\x03\x04\x04\x04\x05\x05\x06\x06\x07\x08\x08\x09\x0a\x0b\x0c\x0d\x0e\x10\x11\x13\x14\x16\x16\x16\x16";
        const FC: [f32; 16] = [4., 8., 16., 32., 64., 96., 128., 160., 202., 254., 380., 508., 762., 1016., 2034., 4068.];
        if self.fix > 0 { self.pi = self.fix }
        let n = (self.pi >> 6) & 127;
        let i = n + n / 3;
        self.kc = if i < 16 { i } else { (i - 16).min(127) };
        let idx = self.pi + self.det + pm;
        let (semi, k) = (((idx & 8191) as f32 + self.pf) / 768.0, (idx >> 6) as usize);
        let len = match self.pg { 6 | 16 | 17 | 19 | 20 | 26 => 32768.0, 21 => 128.0, 256.. => sh.waves[(self.pg - 256) as usize & 255].len().max(1) as f32, _ => 1024.0 };
        let hz = match self.pt {
            1 => 0.018581361 * semi.exp2() * RATE / len,
            3 => 2.536 / (32 - (k & 31)) as f32 * RATE / len,
            4 => if k & 31 == 0 { 0.0 } else { 5.0737 / (k & 31) as f32 * RATE / len },
            5 => 40.584 / FC[k & 15] * RATE / len,
            6 => { let i = k & 63; 23.777 / (if i < 4 { [2, 4, 8, 12][i] } else { (4 + i % 4) << (i / 4 + 1) }) as f32 * RATE / len }
            _ => 8.175799 * semi.exp2(),
        };
        let dt = if self.dt1 & 3 == 0 { 0.0 } else { DT1[(self.dt1 as usize & 3) * 32 - 32 + (self.kc as usize >> 2)] as f32 * 0.0533 };
        self.step = (hz + if self.dt1 > 3 { -dt } else { dt }) * self.fmul as f32 / 128.0 / RATE;
    }

    /// Enter envelope state `st` (1 attack, 2 decay, 3 sustain, 4 release, 0 off), skipping states as SiON does.
    fn shift(&mut self, mut st: u8) {
        let ks = self.kc >> (5 - self.ksr);
        let inc = |r: i32, zero: bool| if r == 0 && zero { 0.0 } else { let r = (r + ks).min(60); 2.064e-4 * (1 << (r >> 2)) as f32 * (4 + (r & 3)) as f32 / 8.0 };
        let sl = if self.sl == 15 { 992.0 } else { (self.sl << 5) as f32 };
        // SSG-EG level tables per type 8..17: [attack skipped?][loop step].
        const SSG: [[[u8; 3]; 2]; 10] = [[[3, 3, 3], [1, 3, 3]], [[1, 6, 6], [1, 6, 6]], [[2, 1, 2], [1, 2, 1]], [[2, 5, 5], [1, 5, 5]], [[4, 4, 4], [2, 4, 4]],
            [[2, 5, 5], [2, 5, 5]], [[1, 2, 1], [2, 1, 2]], [[1, 6, 6], [2, 6, 6]], [[1, 1, 1], [1, 1, 1]], [[2, 2, 2], [2, 2, 2]]];
        let ssg_ar = self.ar >= if self.ssg == 8 || self.ssg == 12 { 56 } else { 60 };
        self.ltab = 0;
        if st == 1 {
            self.ssg_st = match self.ssg_st { usize::MAX => 0, 0 => 1, 1 => 2, _ => 1 };
            if self.ar + ks < 62 {
                if self.erst { self.lv = OFF }
                (self.st, self.rate) = (1, inc(self.ar, true));
                return;
            }
            st = 2;
        }
        // SSG levels run 0..128, scaled by 4 through the level table.
        let ssg = (self.ssg > 0).then(|| SSG[self.ssg as usize - 8][ssg_ar as usize][self.ssg_st]);
        let (slv, bottom) = if let Some(t) = ssg { self.ltab = t; ((sl / 4.0).min(128.0), 128.0) } else { (sl, OFF) };
        if st == 2 && sl > 0.0 { (self.st, self.lv, self.tgt, self.rate) = (2, 0.0, slv, inc(self.dr, true)); return }
        if st == 2 || st == 3 { (self.st, self.lv, self.tgt, self.rate) = (3, slv, bottom, inc(self.sr, true)); return }
        self.ltab = (self.ssg > 0) as u8;
        if st == 4 && self.lv < OFF { (self.st, self.tgt, self.rate) = (4, OFF, inc(self.rr, false)); return }
        self.ltab = 0;
        (self.st, self.lv, self.rate) = (0, OFF, 0.0);
    }

    /// Advance the envelope and return its level, or None when silent.
    #[cfg_attr(feature = "small", optimize(speed))]
    fn eg(&mut self, fr: f32) -> Option<f32> {
        match self.st {
            0 => return None,
            1 => { self.lv -= self.rate * fr * (1.0 + self.lv / 16.0); if self.lv <= 0.0 { self.shift(2) } }
            s => { self.lv += self.rate * fr; if self.lv >= self.tgt { self.shift(if s == 3 && self.ssg > 0 { 1 } else { s + 1 }) } }
        }
        let (l, hi) = (self.lv, self.lv >= 256.0);
        Some(match self.ltab { 0 => l, 1 => if hi { 1024.0 } else { l * 4.0 }, 2 => if hi { 0.0 } else { 512.0 - l * 4.0 },
            3 => if hi { 1024.0 } else { 512.0 + l * 4.0 }, 4 => if hi { 512.0 } else { 1024.0 - l * 4.0 }, 5 => 0.0, _ => 1024.0 })
    }

    /// Advance the envelope `n` samples at once, as that many `eg` calls would (to rounding), stage by stage.
    /// Returns how many of them it was sounding for.
    fn leap(&mut self, n0: f32, fr: f32) -> f32 {
        let mut n = n0;
        while self.st != 0 && n > 0.0 {
            let a = self.rate * fr;
            // Attack: lv + 16 shrinks by (1 - a/16) per sample, until lv reaches 0. Other stages: lv rises by a, to tgt.
            let c = 1.0 - a / 16.0;
            let k = if a <= 0.0 { f32::INFINITY } else if self.st == 1 { if c > 0.0 && self.lv > 0.0 { (16.0 / (self.lv + 16.0)).log2() / c.log2() } else { 0.0 } }
                else { (self.tgt - self.lv) / a };
            let k = (-(-k).floor()).max(1.0); // samples until the stage ends
            let m = k.min(n);
            if self.st == 1 { self.lv = (self.lv + 16.0) * (m * c.log2()).exp2() - 16.0 } else { self.lv += a * m }
            if k <= n { self.shift(if self.st == 1 { 2 } else if self.st == 3 && self.ssg > 0 { 1 } else { self.st + 1 }) }
            n -= m;
        }
        n0 - n
    }

    #[cfg_attr(feature = "small", optimize(speed))]
    fn level(&self, sh: &Shared, lv: f32, am: f32) -> f32 {
        let ksl = if self.ksl == 0 { 0 } else { self.kc >> (5 - self.ksl) };
        let am = if self.ams == 0 { 0.0 } else { am * pow2(self.ams - 3) };
        gain(sh, lv + (((self.tl + ksl) * 8) as f32 + self.tlo).min(OFF) + am)
    }

    #[cfg_attr(feature = "small", optimize(speed))]
    fn run(&mut self, sh: &Shared, md: f32, am: f32, fr: f32) -> f32 {
        self.ph = wrap(self.ph + self.step);
        self.pout = self.out;
        self.out = match self.eg(fr) {
            Some(lv) => (wave(sh, self.pg, wrap(self.ph + md)) * self.level(sh, lv, am)).clamp(-1.0, 1.0),
            None => 0.0,
        };
        self.out
    }
}

pub struct Chan {
    pub ops: Vec<Op>, pub n: usize, pub alg: usize, pub act: usize, pub fb: i32, pub fbc: usize, pub fratio: f32,
    pub inp: (i32, usize), pub ring: (i32, usize), pub outp: (i32, usize),
    /// Operator pitches need recomputing (set when pitch, LFO or operator settings change).
    pub stale: bool,
    pub lfo: i32, pub lfo_tab: Vec<i32>, pub lfo_step: f32, lfo_ph: usize, lfo_t: f32, pub amd: i32, pub pmd: i32, am: f32, pm: i32,
    pub vol: [f32; 8], pub pan: i32, pub vm: (i32, i32), xv: (i32, i32),
    pub pitch: i32, pub on: bool,
    /// Fraction of a pitch step (1/64 semitone) on top of `pitch`, from a fractional `k`.
    pub fine: f32,
    /// PCM module (7 or 10, else -1), its sample number (`%7@n`), and the playing sample, frame, fraction and step.
    pub pcm: i32, pub smp: usize, sidx: usize, spos: usize, sfrac: f32, sstep: f32,
    /// Fraction of a frame added to the `@ph` start frame; how late (in output frames, 0..1) the last event
    /// came, so a PCM note's start or pitch change can be placed at its exact time.
    /// (`slate`: set by a key-on, used by this frame's pitch update only.)
    pub sfrac0: f32, pub slate: f32,
    /// Frames a PCM note plays from its start frame before stopping (0: all), and how many are left.
    pub slen: f32, sleft: f32,
    /// Playing DPCM samples: whether this channel does (its sample is one), whether the sample is still moving
    /// and whether it has yet to start, the one before (sample, frame, fraction, step, frames left) playing on
    /// until it does, the level reached (a frame value), the offset that joins a sample to the level the one
    /// before left (fading out), the gain, and the DC blocker's last input and output. `rejoin`: the track has
    /// gone back to its loop point, so the next sample joins the level wherever the end of the track left it.
    pub rejoin: bool,
    dm: bool, dplay: bool, dnew: bool, dprev: Option<(usize, usize, f32, f32, f32)>, dlv: f32, doff: f32, dg: f32, dhp: (f32, f32),
    /// Per operator (from `ALG`): the pipe it writes, the pipe modulating it (0: none), whether it adds to that pipe.
    wire: [(u8, u8, bool); 4],
    pub f_on: bool, pub f_type: usize, f_cut: [i32; 6], f_time: [i32; 6], f_res: f32, f_v: [f32; 3], cutoff: i32, pub cut_off: i32, f_st: usize, f_step: i32, f_next: i32, f_inc: i32, f_cnt: i32,
    pub ks: bool, kbuf: Vec<f32>, kidx: f32, kout: f32, pub k_lpf: f32, pub kx: f32,
}

/// Operator wiring per algorithm, one letter per operator ('a' + flags):
/// 1 = write pipe 1, 2/4 = modulated by pipe 0/1, 8 = carrier; 'K' = 'k' that also adds to its own modulator.
const ALG: [&[&str]; 4] = [
    &["i"],
    &["ak", "ii", "iK"],
    &["ack", "aak", "ibm", "aki", "bmm", "iii", "aKi"],
    &["acck", "aack", "abek", "acak", "akbm", "bmmm", "akii", "iiii", "ibfm", "ibmi", "aaak", "ibbm", "aKbm"],
];

/// Decode an `ALG` letter into (flags, pipe written, pipe modulating it, adds to that pipe).
fn wiring(c: u8) -> (u8, (u8, u8, bool)) {
    let f = if c == b'K' { 10 } else { c - b'a' };
    let (out, md) = (f & 1, (f >> 1) & 3);
    (f, (out, md, c == b'K' || md != out + 1))
}

impl Chan {
    pub fn new() -> Chan {
        let mut c = Chan::new_raw();
        c.vol[0] = 0.5;
        c.init(0);
        c
    }

    /// Reset everything except volumes and pan (what a `%` module change does).
    pub fn init(&mut self, module: i32) {
        // Pipe wiring survives module changes so `#FM` connections set before `%` stay in place.
        let keep = (self.vol, self.pan, self.vm, self.xv, self.inp, self.outp);
        // (The DMC level stays where it is when the module is set again.)
        let dmc = (self.rejoin, self.dm, self.dplay, self.dnew, self.dprev, self.dlv, self.doff, self.dg, self.dhp, self.sidx, self.spos, self.sfrac, self.sstep, self.slen, self.sleft);
        *self = Chan { ops: vec![Op::new(true)], ks: module == 11, pcm: if module == 7 || module == 10 { module } else { -1 }, kbuf: vec![0.0; if module == 11 { 5400 } else { 0 }], ..Chan::new_raw() };
        (self.vol, self.pan, self.vm, self.xv, self.inp, self.outp) = keep;
        if self.pcm >= 0 && dmc.1 { (self.rejoin, self.dm, self.dplay, self.dnew, self.dprev, self.dlv, self.doff, self.dg, self.dhp, self.sidx, self.spos, self.sfrac, self.sstep, self.slen, self.sleft) = dmc }
        self.lfo_time(333.0);
        if self.ks {
            let o = &mut self.ops[0];
            (o.ar, o.dr, o.rr, o.sl, o.phase, o.pg, o.pt) = (48, 48, 63, 15, -1, 20, 1);
            o.mul(1);
        }
    }

    fn new_raw() -> Chan {
        Chan {
            ops: vec![], n: 1, alg: 0, act: 0, fb: 0, fbc: 0, fratio: 1.0, inp: (0, 0), ring: (0, 0), outp: (0, 0), stale: true,
            lfo: 2, lfo_tab: vec![], lfo_step: 0.0, lfo_ph: 0, lfo_t: 1.0, amd: 0, pmd: 0, am: 0.0, pm: 0,
            vol: [0.0; 8], pan: 64, vm: (0, 0), xv: (128, 256), pitch: 0, on: false, fine: 0.0, pcm: -1, smp: 0, sidx: 0, spos: 0, sfrac: 0.0, sstep: 0.0, sfrac0: 0.0, slate: 0.0, slen: 0.0, sleft: 0.0,
            rejoin: false, dm: false, dplay: false, dnew: false, dprev: None, dlv: 0.0, doff: 0.0, dg: 0.0, dhp: (0.0, 0.0), wire: [wiring(b'i').1; 4],
            f_on: false, f_type: 0, f_cut: [128, 128, 128, 128, 0, 128], f_time: [0, 0, 0, i32::MAX, 0, i32::MAX], f_res: 1.0, f_v: [0.0; 3], cutoff: 128, cut_off: 0, f_st: 5, f_step: i32::MAX, f_next: 0, f_inc: 0, f_cnt: i32::MAX,
            ks: false, kbuf: vec![], kidx: 0.0, kout: 0.0, k_lpf: 0.875, kx: 1.0,
        }
    }

    pub fn lfo_time(&mut self, ms: f32) { self.lfo_step = 1.0 / (ms * 0.17294118).max(1e-3) }

    pub fn lfo_init(&mut self, shape: i32, tab: Vec<i32>) {
        (self.lfo, self.lfo_tab, self.lfo_ph, self.lfo_t, self.amd, self.pmd, self.am, self.pm) = (shape, tab, 0, 1.0, 0, 0, 0.0, 0);
    }

    fn lfo_val(&self) -> i32 {
        let i = self.lfo_ph as i32;
        if let Some(v) = self.lfo_tab.get(self.lfo_ph) { return (*v).clamp(0, 255) }
        let v = match self.lfo & 3 {
            0 => 255 - i,
            1 => if i < 128 { 255 } else { 0 },
            2 => match i { 0..=63 => 2 * i + 128, 64..=127 => 2 * (127 - i) + 128, 128..=191 => 126 - 2 * (i - 128), _ => 126 - 2 * (255 - i) },
            _ => (i * 197 + 61) * 113 % 255,
        };
        if self.lfo > 3 { 255 - v } else { v }
    }

    pub fn set_am(&mut self, d: i32) { self.amd = d; self.am = (self.lfo_val() * d) as f32 / 32.0 }
    pub fn set_pm(&mut self, d: i32) { self.pmd = d; self.pm = if d == 0 { 0 } else { ((self.lfo_val() * 2 - 255) * d) >> 8 } }

    /// Set operator count (5 = two-oscillator "analog" mode) and algorithm.
    pub fn set_alg(&mut self, cnt: usize, alg: usize) {
        let ops = if cnt == 5 { 2 } else { cnt.clamp(1, 4) };
        self.ops.resize(ops, Op::new(true));
        (self.n, self.alg, self.act, self.fb) = (if cnt == 5 { 5 } else { ops }, alg, ops - 1, 0);
        let code = ALG[ops - 1].get(alg).filter(|_| cnt != 5).map_or(&b"iiii"[..], |c| c.as_bytes());
        for ((o, w), &c) in self.ops.iter_mut().zip(&mut self.wire).zip(code) { let (f, wc) = wiring(c); (o.fin, *w) = (f & 8 != 0, wc) }
        self.volume(self.xv.0, self.xv.1);
    }

    pub fn set_voice(&mut self, v: &Voice) {
        self.fratio = v.fratio;
        self.set_alg(v.n, v.alg);
        (self.fb, self.fbc) = (v.fb, if v.fbc < self.ops.len() { v.fbc } else { 0 });
        self.f_type = 0;
        self.filter(&[128, 0, 0, 0, 0, 0, 128, 64, 32, 128]);
        for (o, p) in self.ops.iter_mut().zip(&v.ops) {
            *o = Op { pi: o.pi, pf: o.pf, kc: o.kc, ph: o.ph, lv: o.lv, st: o.st, rate: o.rate, tgt: o.tgt, out: o.out, fin: o.fin, tlo: o.tlo, pout: o.pout, ..p.clone() };
        }
    }

    /// `@` operator parameters: ar, dr, sr, rr, sl, tl, ksr, ksl, mul, dt1, detune, ams, phase, fixed note.
    pub fn params(&mut self, a: &[i32]) {
        let o = &mut self.ops[self.act];
        for (k, &v) in a.iter().enumerate().filter(|(_, v)| **v != MIN) {
            match k {
                0 => o.ar = v & 63, 1 => o.dr = v & 63, 2 => o.sr = v & 63, 3 => o.rr = v & 63, 4 => o.sl = v & 15, 5 => o.tl = v.clamp(0, 127),
                6 => o.ksr = v & 3, 7 => o.ksl = v & 3, 8 => o.mul(v), 9 => o.dt1 = v & 7, 10 => o.det = v, 11 => o.ams = v & 3, 12 => o.phase = v, 13 => o.fix = v << 6,
                _ => {}
            }
        }
    }

    pub fn volume(&mut self, x: i32, vel: i32) {
        self.xv = (x, vel);
        if self.ks { self.kx = x as f32 / 128.0 }
        let tl = vtl(self.vm.1, if self.ks { 256 } else { x * 2 }) + vtl(self.vm.0, vel);
        for o in &mut self.ops { o.tlo = if o.fin { tl } else { 0.0 } }
    }

    pub fn release_rate(&mut self, rr: i32, all: bool) {
        if self.ks { self.k_lpf = 1.0 - rr as f32 / 64.0 }
        else if all { for o in self.ops.iter_mut().filter(|o| o.fin) { o.rr = rr & 63 } }
        else { self.ops[self.act].rr = rr & 63 }
    }

    /// `@f` cutoff, resonance, attack/decay/decay/release rates, then cutoff targets.
    pub fn filter(&mut self, a: &[i32]) {
        let rate = |r: i32| match r & 63 { 0 => 0, r @ 1..=59 => (2.36514 * (1 << (14 - (r >> 2))) as f32 * ((r & 3) as f32 * 0.125 + 0.5) + 0.5) as i32, _ => 1 };
        self.f_cut = [a[0], a[6], a[7], a[8], 0, a[9]].map(|c| c.clamp(0, 128));
        self.f_time = [rate(a[2]), rate(a[3]), rate(a[4]), i32::MAX, rate(a[5]), i32::MAX];
        self.f_res = (-(a[1].clamp(0, 9)) as f32).exp2();
        self.f_on = a[0] < 128 || a[1] > 0 || a[2] > 0 || a[5] > 0;
    }

    fn f_shift(&mut self, mut st: usize) {
        while st != 3 && st < 5 {
            if self.f_time[st] > 0 {
                (self.f_st, self.f_step, self.f_next) = (st, self.f_time[st], self.f_cut[st + 1]);
                self.f_inc = if self.cutoff < self.f_next { 1 } else { -1 };
                if self.cutoff != self.f_next { self.f_cnt = self.f_step; return }
            }
            st += 1;
        }
        (self.f_st, self.f_step, self.f_cnt, self.f_next, self.f_inc) = (st, i32::MAX, i32::MAX, self.cutoff + 1, 0);
    }

    pub fn note_on(&mut self) {
        for o in &mut self.ops {
            match o.phase { 255 => {} -1 => o.ph = (o.ph * 7919.0 + 0.137).fract(), p => o.ph = (p & 255) as f32 / 256.0 }
            o.ssg_st = usize::MAX;
            o.shift(1);
        }
        self.lfo_ph = 0;
        // A PCM note starts at frame `@ph` (moving at no speed until its pitch is set); a negative one is that
        // many frames of silence before the sample.
        let ph = self.ops[0].phase as f32 + self.sfrac0;
        // (A DPCM sample still playing plays on until the new one starts.)
        self.dprev = (self.dm && self.dplay).then_some((self.sidx, self.spos, self.sfrac.max(0.0), self.sstep, if self.slen > 0.0 { self.sleft } else { f32::INFINITY }));
        (self.spos, self.sfrac, self.sstep, self.sleft, self.dplay, self.dnew) = (ph.max(0.0) as usize, if ph < 0.0 { ph } else { self.sfrac0 }, 0.0, self.slen, true, true);
        if self.f_on { self.cutoff = self.f_cut[0]; self.f_shift(0) }
        if self.ks { self.kout = 0.0; for v in &mut self.kbuf { *v *= 0.3 } }
        self.on = true;
    }

    pub fn note_off(&mut self) {
        for o in &mut self.ops { o.shift(4) }
        if self.f_on { self.f_shift(4) }
        self.on = false;
    }

    /// Advance `n` samples without producing output, for seeking: envelopes, PCM playback position, LFO,
    /// filter sweep and oscillator phases move on as `n` calls of `sample` would (to rounding); what only
    /// lasts a moment (feedback, filter and plucked-string memory) is left to settle in the samples after.
    pub fn leap(&mut self, sh: &Shared, n: u64) {
        if self.pcm >= 0 && self.dm { self.dpcm_leap(sh, n as f32) }
        if !self.ks && !self.f_on && self.silent() { return }
        let (live, fr, nf) = (self.ops[0].st != 0, self.fratio, n as f32);
        let eg = if self.pcm >= 0 || self.n == 5 { 1 } else { self.ops.len() };
        // Phases and the LFO only move while the channel sounds (see `sample`).
        let on = self.ops[..eg].iter_mut().fold(0f32, |on, o| on.max(o.leap(nf, fr)));
        let on = if self.ks || self.f_on { nf } else { on };
        for o in &mut self.ops { o.ph = wrap(o.ph + o.step * on) }
        if self.pmd != 0 || self.amd > 0 {
            self.lfo_t -= self.lfo_step * on;
            if self.lfo_t < 0.0 {
                let k = (-self.lfo_t * fr).floor() + 1.0;
                self.lfo_t += k / fr;
                self.lfo_ph = (self.lfo_ph + k as usize) & 255;
                let (a, p) = (self.amd, self.pmd);
                self.set_am(a);
                self.set_pm(p);
                self.stale = true;
            }
        }
        if let (true, Some(s)) = (self.pcm >= 0 && live && !self.dm, sh.sample(self.sidx)) {
            advance(&mut self.spos, &mut self.sfrac, self.sstep * nf);
            let o = &mut self.ops[0];
            if self.spos >= s.data.len() { match s.wrap(self.spos) { Some(u) => self.spos = u, None => o.st = 0 } }
            if self.slen > 0.0 { self.sleft -= self.sstep * nf; if self.sleft <= 0.0 { o.st = 0 } }
        }
        if self.f_on {
            let mut m = n as i64;
            while self.f_cnt.max(1) as i64 <= m {
                m -= self.f_cnt.max(1) as i64;
                self.cutoff += self.f_inc;
                self.f_cnt = self.f_step;
                if self.cutoff == self.f_next { self.f_shift(self.f_st + 1) }
            }
            self.f_cnt -= m as i32;
        }
    }

    /// No operator is sounding (a filter or plucked string may still be ringing out).
    pub fn silent(&self) -> bool { self.ops.iter().all(|o| o.st == 0) }

    /// `leap` for a DPCM sample: it moves on (the one before it is taken to have finished), the level goes where
    /// it got to, and the DC blocker starts settled there.
    fn dpcm_leap(&mut self, sh: &Shared, nf: f32) {
        (self.dprev, self.dnew, self.doff) = (None, false, 0.0);
        if self.dplay {
            advance(&mut self.spos, &mut self.sfrac, self.sstep * nf);
            if let Some(s) = sh.sample(self.sidx) {
                match s.wrap(self.spos) { Some(u) => self.spos = u, None => { self.dplay = false; self.spos = s.data.len().saturating_sub(1) } }
                if self.sfrac >= 0.0 { self.dlv = s.data.get(self.spos).copied().unwrap_or(0.0) }
            }
            if self.slen > 0.0 { self.sleft -= self.sstep * nf; if self.sleft <= 0.0 { self.dplay = false } }
        }
        self.dhp = (self.dlv * self.dg, 0.0);
    }

    /// Whether a DPCM channel still has something to do: a sample moving, or its level's DC still settling.
    fn dmc_busy(&self) -> bool { self.dm && (self.dplay || self.dprev.is_some() || self.dhp.1.abs() > 1e-7 || (self.dlv + self.doff) * self.dg != self.dhp.0) }

    /// One frame of a DPCM sample, played as the NES's delta modulation channel plays it: its output is a level,
    /// which stays where the sample left it (at its end, where `@ph` stops it, or at key-off: the channel doesn't
    /// stop for it) until the next sample moves it on from there; a sample waiting for its start (a negative
    /// `@ph`) lets the one before it play on until then, so samples that the converter decoded each from the level
    /// the one before leaves join without a step however the notes fall on output frames. (Where a song loops
    /// back, the level the end of the song left isn't the one the samples after the loop point were decoded from:
    /// the first sample after it joins that level instead, and the difference fades out over a few tens of ms.)
    /// The DC comes out the way the console's output stage takes it out: a first-order high-pass, at 60 Hz, where
    /// the channel's low end matches an NSF player's (libgme's) within half a dB. (The console's own filters, 90 and
    /// 440 Hz, act on the whole mix, which SiON's other channels don't get either.) The note's envelope only sets
    /// the gain.
    fn dpcm(&mut self, sh: &Shared, am: f32) -> f32 {
        const HP: f32 = 1.0 / (1.0 + TAU * 60.0 / RATE);
        const FADE: f32 = 1.0 - 1.0 / (0.02 * RATE);
        // (A one-shot holds its last frame: past it there is no silence to slide towards.)
        let at = |s: &Sample, u: usize| if s.lp.is_some() { s.at(u) } else { s.data.get(u).or(s.data.last()).copied().unwrap_or(0.0) };
        let o = &mut self.ops[0];
        if let Some(lv) = o.eg(self.fratio) { if self.on { self.dg = o.level(sh, lv, am) } }
        if self.sfrac < 0.0 || !self.dplay {
            // Waiting to start (or done): the sample before plays on, or the level holds.
            if let Some((i, mut pos, mut frac, step, mut left)) = self.dprev {
                match sh.sample(i) {
                    Some(s) if left > 0.0 && (s.lp.is_some() || pos + 1 < s.data.len()) => {
                        self.dlv = at(s, pos) + (at(s, pos + 1) - at(s, pos)) * frac;
                        advance(&mut pos, &mut frac, step);
                        left -= step;
                        if let Some(u) = s.wrap(pos) { pos = u }
                        self.dprev = Some((i, pos, frac, step, left));
                    }
                    s => { if let Some(s) = s { if left > 0.0 { self.dlv = at(s, pos) } } self.dprev = None }
                }
            }
        } else if let Some(s) = sh.sample(self.sidx) {
            self.dprev = None;
            if core::mem::take(&mut self.dnew) && core::mem::take(&mut self.rejoin) { self.doff += self.dlv - at(s, 0) }
            self.dlv = at(s, self.spos) + (at(s, self.spos + 1) - at(s, self.spos)) * self.sfrac;
        }
        self.doff *= FADE;
        if self.dplay {
            advance(&mut self.spos, &mut self.sfrac, self.sstep);
            if let Some(s) = sh.sample(self.sidx) {
                match s.wrap(self.spos) { Some(u) => self.spos = u, None if self.spos + 1 >= s.data.len() => { self.dplay = false; self.dlv = at(s, self.spos) } None => {} }
            }
            if self.slen > 0.0 { self.sleft -= self.sstep; if self.sleft <= 0.0 { self.dplay = false } }
        }
        let u = (self.dlv + self.doff) * self.dg;
        self.dhp.1 = HP * (self.dhp.1 + u - self.dhp.0);
        self.dhp.0 = u;
        self.dhp.1
    }

    /// Recompute the operator pitches and PCM speed after a pitch, LFO or operator change; `late` is how far
    /// into the frame just played (0..1) the change came.
    pub fn refresh(&mut self, sh: &Shared, late: f32) {
        let (pm, pitch, fine, ks) = (self.pm, self.pitch, self.fine, self.ks);
        for o in &mut self.ops { if !ks { (o.pi, o.pf) = (pitch, fine) } o.pitch(sh, pm) }
        if self.pcm >= 0 {
            // `%10` picks the sample by note number and plays it at its own rate.
            self.sidx = if self.pcm == 10 { (pitch >> 6).max(0) as usize } else { self.smp };
            self.dm = sh.dpcm.get(self.sidx).copied().unwrap_or(false);
            let semi = |s: &Sample| if self.pcm == 7 { (((pitch + self.ops[0].det + pm - s.key * 64) as f32 + fine) / 768.0).exp2() } else { 1.0 };
            let step = sh.sample(self.sidx).map_or(0.0, |s| s.rate / RATE * semi(s));
            // The new speed applies from the event's exact time, partway into the frame just played.
            self.sfrac += (step - self.sstep) * late;
            while self.sfrac < 0.0 && self.spos > 0 { (self.spos, self.sfrac) = (self.spos - 1, self.sfrac + 1.0) }
            self.sstep = step;
        }
        self.stale = false;
    }

    /// Produce one sample. `pipes` carry signals between tracks (`@i`, `@o`, `@r`).
    #[cfg_attr(feature = "small", optimize(speed))]
    pub fn sample(&mut self, sh: &Shared, pipes: &mut [f32; 4]) -> f32 {
        let late = core::mem::take(&mut self.slate);
        if !self.ks && !self.f_on && self.silent() && !self.dmc_busy() { return 0.0 }
        if self.pmd != 0 || self.amd > 0 {
            self.lfo_t -= self.lfo_step;
            if self.lfo_t < 0.0 {
                self.lfo_t += 1.0 / self.fratio;
                self.lfo_ph = (self.lfo_ph + 1) & 255;
                let (a, p, pm0) = (self.amd, self.pmd, self.pm);
                self.set_am(a);
                self.set_pm(p);
                self.stale |= self.pm != pm0;
            }
        }
        if self.stale { self.refresh(sh, late) }
        let (am, pm, fr, pitch) = (self.am, self.pm, self.fratio, self.pitch);
        let fbin = if self.inp.0 > 0 { pipes[self.inp.1] * pow2(self.inp.0 - 3) } else if self.fb > 0 {
            let o = &self.ops[self.fbc];
            // (The chips' sum also cancels at the sample rate, so deep feedback doesn't break into oscillation.)
            if sh.fb2 { (o.out + o.pout) * pow2(self.fb - 8) } else { o.out * pow2(self.fb - 7) }
        } else { 0.0 };
        let mut x = if self.pcm >= 0 && self.dm { self.dpcm(sh, am) } else if self.pcm >= 0 {
            let o = &mut self.ops[0];
            match (sh.sample(self.sidx), o.eg(fr)) {
                (Some(s), Some(lv)) => {
                    // (Before the first frame, the note fades in from silence over one frame.)
                    let (a, b, f) = if self.sfrac < 0.0 { (0.0, s.at(0), (1.0 + self.sfrac).max(0.0)) } else { (s.at(self.spos), s.at(self.spos + 1), self.sfrac) };
                    o.out = (a + (b - a) * f) * o.level(sh, lv, am);
                    advance(&mut self.spos, &mut self.sfrac, self.sstep);
                    if self.spos >= s.data.len() { match s.wrap(self.spos) { Some(u) => self.spos = u, None => o.st = 0 } }
                    if self.slen > 0.0 { self.sleft -= self.sstep; if self.sleft <= 0.0 { o.st = 0 } }
                    o.out
                }
                _ => 0.0,
            }
        } else if self.n == 5 {
            let (a, rest) = self.ops.split_first_mut().unwrap();
            let b = &mut rest[0];
            let lv = a.eg(fr);
            let wrap_a = a.ph + a.step + fbin >= 1.0;
            a.ph = wrap(a.ph + a.step + if self.alg == 2 { fbin } else { 0.0 });
            b.ph = if self.alg == 2 && wrap_a { 0.0 } else { wrap(b.ph + b.step) };
            let (wa, wb) = (wave(sh, a.pg, wrap(a.ph + if self.alg == 2 { 0.0 } else { fbin })), wave(sh, b.pg, b.ph));
            a.out = match (lv, self.alg) {
                (None, _) => 0.0,
                (Some(lv), 1) => wa * wb * b.level(sh, lv, am),
                (Some(lv), 2) => wb * b.level(sh, lv, am),
                (Some(lv), _) => wa * a.level(sh, lv, am) + wb * b.level(sh, lv, am),
            };
            a.out
        } else {
            let mut p = [0f32; 2];
            for (i, (o, &(out, md, add))) in self.ops.iter_mut().zip(&self.wire).enumerate() {
                let x = o.run(sh, if i == 0 { fbin } else if md > 0 { p[md as usize - 1] * 4.0 } else { 0.0 }, am, fr);
                p[out as usize] = x + if add { p[out as usize] } else { 0.0 };
            }
            p[0]
        };
        if self.ring.0 > 0 { x *= pipes[self.ring.1] * self.ring.0 as f32 * 4.0 }
        if self.ks {
            let wl = RATE / (8.175799 * ((pitch + self.ops[0].det + pm).clamp(0, 8191) as f32 / 768.0).exp2());
            self.kidx += 1.0;
            // `kidx %= wl`, exactly: subtract the largest power-of-two multiple of `wl` that fits, each step exact.
            while self.kidx >= wl { let mut m = wl; while m * 2.0 <= self.kidx { m *= 2.0 } self.kidx -= m }
            let (t, (dec, lpf)) = (self.kidx as usize, if self.on { (0.98, self.k_lpf) } else { (0.75, 0.5) });
            self.kout = self.kout * dec + (self.kbuf[t] - self.kout * dec) * lpf + x;
            self.kbuf[t] = self.kout;
            x = self.kout * self.kx;
        }
        if self.f_on {
            let c = ((self.cutoff + self.cut_off).clamp(0, 128) as f32 / 128.0).powi(2);
            self.f_v[2] = x - self.f_v[0] - self.f_v[1] * self.f_res;
            self.f_v[1] += self.f_v[2] * c;
            self.f_v[0] += self.f_v[1] * c;
            x = self.f_v[self.f_type];
            self.f_cnt -= 1;
            if self.f_cnt <= 0 {
                self.cutoff += self.f_inc;
                self.f_cnt = self.f_step;
                if self.cutoff == self.f_next { self.f_shift(self.f_st + 1) }
            }
        }
        match self.outp.0 { 0 => return x, 1 => pipes[self.outp.1] = x, _ => pipes[self.outp.1] += x }
        0.0
    }
}
