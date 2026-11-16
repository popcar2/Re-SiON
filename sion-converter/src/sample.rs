//! Tracker samples → SiON sounds. Normally each sample is embedded as it is (`#PCMB`, played with `%7`); see
//! `pcm`. With `--wavetables` (or when all 256 PCM slots are taken) it becomes the nearest thing SiON can play
//! without one:
//!
//! - A loop of up to 1024 frames (the usual chiptune sample: one cycle of a square, saw, custom wave) is the
//!   wavetable itself (`#WAV`, `%4@n`), played at the same pitch.
//! - A longer periodic sample becomes one cycle of it as a wavetable; a one-shot sample also gets a decay
//!   envelope fitted to how the sample fades.
//! - Anything not periodic (drums, effects) becomes SiON's white noise (`%2@0`) at a rate that matches the
//!   sample's brightness, with the same kind of decay.

use crate::score::{hz_key, noise_key, Defs};
use crate::tracker::Sample;

pub struct Sound {
    pub instr: String,
    pub env: String,
    /// The SiON envelope shapes the note (one-shot samples): slur pitch changes instead of restarting it.
    pub legato: bool,
    /// Level of the sound relative to a full-scale wave.
    pub gain: f32,
    kind: Kind,
}

enum Kind { Silent, Wave { cycle: f64 }, Noise { density: f64 }, Pcm { rate: f64 } }

impl Sound {
    /// Key (SiON pitch) for the sample played at `rate` frames per second.
    pub fn pitch(&self, rate: f64) -> f32 {
        match self.kind {
            Kind::Silent => f32::NAN,
            Kind::Wave { cycle } => hz_key(rate / cycle),
            Kind::Noise { density } => noise_key(rate * density),
            Kind::Pcm { rate: r } => (PCM_KEY as f64 + 12.0 * (rate / r).log2()) as f32,
        }
    }
    pub fn silent(&self) -> bool { matches!(self.kind, Kind::Silent) }
}

/// Root note of embedded samples: the sample's rate is its rate at this key.
const PCM_KEY: i32 = 60;

/// The sample as SiON plays it: data up to the loop end (frames after it are never heard), rate as an f32.
pub fn as_sion(s: &Sample) -> sion::Sample {
    let end = s.lp.map_or(s.data.len(), |l| l.1.min(s.data.len()));
    sion::Sample { data: s.data[..end].to_vec(), rate: s.c4 as f32, key: PCM_KEY, lp: s.lp.map(|l| l.0), bidi: s.lp.is_some_and(|l| l.2), ..Default::default() }
}

/// The sample itself, as `#PCMB` (8-bit data stays 8-bit, so both are stored losslessly), or None when no PCM
/// slot is left.
pub fn pcm(s: &Sample, defs: &mut Defs) -> Option<Sound> {
    let p = as_sion(s);
    if p.data.iter().all(|v| *v == 0.0) { return Some(Sound { instr: String::new(), env: String::new(), legato: false, gain: 0.0, kind: Kind::Silent }) }
    let n = crate::pcm::def(defs, &p.data, p.rate, p.lp, p.bidi)?;
    Some(Sound { instr: "%7".into(), env: format!("@{n}s63"), legato: true, gain: 1.0, kind: Kind::Pcm { rate: p.rate as f64 } })
}

/// SiON's decay speed in dB per second for envelope rate `r` (as `synth.rs` computes it).
fn decay_db(r: i32) -> f64 { crate::score::env_inc(r) * 44100.0 * 6.0206 / 64.0 }

/// Envelope commands after the tone number: a sustained sound releases at once (the tracker stops it), a
/// one-shot decays at the sample's own speed.
fn envelope(decay: Option<f64>) -> String {
    match decay {
        None => "s63".into(),
        Some(db) => {
            let r = (1..60).min_by(|&a, &b| (decay_db(a).ln() - db.ln()).abs().total_cmp(&(decay_db(b).ln() - db.ln()).abs())).unwrap();
            format!(",63,{r},0,63,15")
        }
    }
}

/// Normalized autocorrelation: the period (fractional) with the strongest repetition, and how strong it is.
fn period(x: &[f32], tmax: usize) -> Option<(f64, f32)> {
    let tmax = tmax.min(x.len() / 2);
    if tmax < 4 { return None }
    let w = x.len() - tmax;
    let e0: f32 = x[..w].iter().map(|v| v * v).sum();
    let mut r = vec![0f32; tmax + 1];
    for t in 2..=tmax {
        let (mut c, mut e1) = (0f32, 0f32);
        for i in 0..w { c += x[i] * x[i + t]; e1 += x[i + t] * x[i + t] }
        r[t] = if e0 > 0.0 && e1 > 0.0 { c / (e0 * e1).sqrt() } else { 0.0 };
    }
    // Neighbouring frames of a smooth sound always correlate: only look past the first dip.
    let dip = (2..tmax).find(|&t| r[t] < 0.2)?;
    let best = r[dip..].iter().copied().fold(0f32, f32::max);
    if best <= 0.0 { return None }
    // The first strong peak, so a multiple of the period isn't taken for it.
    let t = (dip.max(3)..tmax).find(|&t| r[t] >= 0.9 * best && r[t] >= r[t - 1] && r[t] >= r[t + 1])?;
    let (a, b, c) = (r[t - 1], r[t], r[t + 1]);
    let d = a - 2.0 * b + c;
    let off = if d.abs() > 1e-9 { (0.5 * (a - c) / d).clamp(-0.5, 0.5) } else { 0.0 };
    Some((t as f64 + off as f64, b))
}

/// `n` points of the `len`-frame cycle starting at `x[0]` (wrapping), DC removed and scaled to ±127.
fn table(x: &[f32], len: f64, n: usize) -> (Vec<i32>, f32) {
    let at = |p: f64| { let p = p % x.len() as f64; let (i, f) = (p as usize, (p.fract()) as f32); x[i] * (1.0 - f) + x[(i + 1) % x.len()] * f };
    let mut v: Vec<f32> = (0..n).map(|i| at(i as f64 * len / n as f64)).collect();
    let mean = v.iter().sum::<f32>() / n as f32;
    for s in &mut v { *s -= mean }
    let peak = v.iter().fold(0f32, |p, s| p.max(s.abs()));
    if peak < 1e-5 { return (vec![0; n], 0.0) }
    (v.iter().map(|s| (s / peak * 127.0).round().clamp(-128.0, 127.0) as i32).collect(), peak)
}

fn pow2(n: f64) -> usize { (n.ceil() as usize).next_power_of_two().clamp(32, 1024) }

/// RMS level per block of 256 frames.
fn levels(x: &[f32]) -> Vec<f32> { x.chunks(256).map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt()).collect() }

pub fn analyze(s: &Sample, defs: &mut Defs) -> Sound {
    let silent = Sound { instr: String::new(), env: String::new(), legato: false, gain: 0.0, kind: Kind::Silent };
    let x = &s.data;
    if x.len() < 2 || x.iter().all(|v| v.abs() < 1e-4) { return silent }
    let wave = |defs: &mut Defs, t: Vec<i32>, env: String, legato: bool, gain: f32, cycle: f64| {
        let n = defs.wave(&t);
        Sound { instr: "%4".into(), env: format!("@{n}{env}"), legato, gain, kind: Kind::Wave { cycle } }
    };
    if let Some((ls, le, ping)) = s.lp {
        let mut lp: Vec<f32> = x[ls..le].to_vec();
        if ping && lp.len() > 2 { let back: Vec<f32> = lp[1..lp.len() - 1].iter().rev().copied().collect(); lp.extend(back) }
        let n = lp.len();
        if n <= 1024 {
            let (t, peak) = table(&lp, n as f64, pow2(n as f64));
            if peak > 0.0 { return wave(defs, t, envelope(None), false, peak, n as f64) }
            return silent;
        }
        // A long loop: one cycle of it, if it repeats; else the whole loop squeezed into one table (it does
        // repeat at the loop length), unless it is bright enough to be heard as noise.
        let win = &lp[..n.min(4096)];
        if let Some((t, _)) = period(win, 2048).filter(|p| p.1 > 0.5) {
            let (tb, peak) = table(win, t, pow2(t).min(512));
            if peak > 0.0 { return wave(defs, tb, envelope(None), false, peak, t) }
        }
        if crossing_density(win) < 0.15 {
            let (tb, peak) = table(&lp, n as f64, 1024);
            if peak > 0.0 { return wave(defs, tb, envelope(None), false, peak, n as f64) }
        }
        return noise(win, None, defs);
    }
    // One-shot: analyze around the loudest part; fit the decay from there on.
    let lv = levels(x);
    let (pk, &pl) = lv.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
    if pl < 1e-4 { return silent }
    let secs = 256.0 / s.c4.max(1.0);
    let tail = lv[pk..].iter().position(|&l| l < pl / 32.0);
    let db = match tail {
        Some(k) if k > 0 => 30.0 / (k as f64 * secs),
        Some(_) => 200.0,
        None => {
            let last = lv.last().copied().unwrap_or(pl).max(1e-6);
            (20.0 * (pl / last).log10() as f64 / ((lv.len() - pk) as f64 * secs).max(secs)).max(1.0)
        }
    };
    let decay = Some(db.max(1.0));
    let start = pk * 256;
    let win = &x[start..(start + 4096).min(x.len())];
    if let Some((t, _)) = period(win, 2048.min(win.len() / 2)).filter(|p| p.1 > 0.6) {
        // Start the cycle at a rising zero crossing so the table has no jump.
        let z = (1..win.len().min(t as usize + 2)).find(|&i| win[i - 1] <= 0.0 && win[i] > 0.0).unwrap_or(0);
        let (tb, peak) = table(&win[z..], t, pow2(t).min(512));
        if peak > 0.0 { return wave(defs, tb, envelope(decay), true, peak, t) }
    }
    noise(win, decay, defs)
}

/// Zero crossings per frame, times two: about 1 for white noise, 2 / period for a tone.
fn crossing_density(win: &[f32]) -> f64 {
    let mean = win.iter().sum::<f32>() / win.len().max(1) as f32;
    let crossings = win.windows(2).filter(|p| (p[0] - mean) * (p[1] - mean) < 0.0).count();
    (2.0 * crossings as f64 / win.len().max(2) as f64).clamp(0.002, 1.0)
}

/// A sound without a clear period: noise at its brightness, or (very dull ones, like kick drums) a low tone
/// at its zero-crossing rate.
fn noise(win: &[f32], decay: Option<f64>, defs: &mut Defs) -> Sound {
    let mean = win.iter().sum::<f32>() / win.len().max(1) as f32;
    let density = crossing_density(win);
    if density < 0.1 {
        let t = 2.0 / density;
        let z = (1..win.len()).find(|&i| win[i - 1] <= mean && win[i] > mean).unwrap_or(0);
        if z + (t as usize) < win.len() {
            let (tb, peak) = table(&win[z..], t, pow2(t).min(512));
            if peak > 0.0 {
                let n = defs.wave(&tb);
                return Sound { instr: "%4".into(), env: format!("@{n}{}", envelope(decay)), legato: decay.is_some(), gain: peak, kind: Kind::Wave { cycle: t } };
            }
        }
    }
    let rms = (win.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / win.len().max(1) as f32).sqrt();
    let env = match decay { None => "@0s63".into(), Some(_) => format!("@0{}", envelope(decay)) };
    Sound { instr: "%2".into(), env, legato: decay.is_some(), gain: (rms / 0.577).min(1.0), kind: Kind::Noise { density } }
}
