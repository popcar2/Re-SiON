//! Sample playback in VGM chips (YM2612 DAC, NES DPCM and `$4011`, YM2608 / YM2610 ADPCM) as embedded samples
//! (`#PCMB`, played with `%7`): each time a chip starts a sample is a `Hit`, which `vgm.rs` turns into a note.
//!
//! Each sound is stored once, at its own rate, and every hit plays (part of) it:
//!
//! - Samples a chip plays from memory (ADPCM, DPCM) are decoded once and played at the chip's rate; ADPCM is kept
//!   in 8 bits, scaled to the sample's peak.
//! - DAC bytes a driver streams from the VGM's sample bank (`0x8n`, DAC streams) are the bank itself: each stretch
//!   of the bank that is played is one sample, and a hit plays it from where it starts reading, at the rate it
//!   reads (on average: drivers' write timing jitters by a few frames, which is left out).
//! - Values a program writes directly (the DAC, `$4011`) are sampled at the run's usual write rate: each write
//!   lasts as many frames as its gap spans at that rate (one, for the jittery gaps of a write loop; more where the
//!   program pauses), and runs that come out the same share a sample.
//!
//! With `exact` everything is instead captured as the chip outputs it, frame by frame at 44100 Hz (VGM times are
//! 44100 Hz frames, so that is exact), and ADPCM in 16 bits: exact, but a sample per hit, and much larger.

use crate::score::Defs;
use std::collections::HashMap;

/// One sample playback on a chip voice: start and end (VGM frames, 44100 per second), whether it stops before its
/// sample does (`cut`), the `#PCMB` number and the frame of it the hit starts on, the sample's rate at key 60
/// (frames per second), the key it plays at, its level and pan, and level and key changes while it sounds.
#[derive(Clone, Debug)]
pub struct Hit {
    pub voice: usize,
    pub at: f64,
    pub end: f64,
    pub cut: bool,
    pub smp: usize,
    pub ofs: f64,
    pub rate: f64,
    pub key: f64,
    pub amp: f32,
    pub pan: i32,
    pub vols: Vec<(f64, f32)>,
    pub keys: Vec<(f64, f64)>,
    /// NES DPCM: the frames are the DMC's level, which the channel playing them holds between hits (each starts
    /// from the level the one before left), so all of them play on one track.
    pub dmc: bool,
}

impl Hit {
    pub fn new(voice: usize, at: f64, end: f64, smp: usize, rate: f64, key: f64, amp: f32, pan: i32) -> Hit {
        Hit { voice, at, end, cut: false, smp, ofs: 0.0, rate, key, amp, pan, vols: vec![], keys: vec![], dmc: false }
    }
}

/// URL-safe base64, as `#PCMB` reads it (no `/`, so a sample can't contain a `//` comment).
pub fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::with_capacity(bytes.len() * 4 / 3 + 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..=c.len() { s.push(A[(n >> (18 - 6 * k)) as usize & 63] as char) }
    }
    s
}

/// `#PCMB` number of a sample played at `rate` at key 60 (frames in -1..1; 8-bit data stays 8-bit, so both
/// depths are stored losslessly), or None when all 256 slots are taken.
pub fn def(defs: &mut Defs, data: &[f32], rate: f32, lp: Option<usize>, bidi: bool) -> Option<usize> {
    let wide = data.iter().any(|v| (v * 128.0).fract() != 0.0);
    let bytes: Vec<u8> = if wide { data.iter().flat_map(|v| ((v * 32768.0) as i16).to_le_bytes()).collect() } else { data.iter().map(|v| (v * 128.0) as i8 as u8).collect() };
    let flags = wide as u8 | (bidi as u8) << 1;
    defs.pcm(format!("{{{}}}{rate},60,{},{flags}", b64(&bytes), lp.map_or(-1, |l| l as i64)))
}

/// The frames in 8 bits, scaled to their peak: the frames, and the gain that restores their level.
pub fn small(data: &[f32]) -> (Vec<f32>, f32) {
    let peak = data.iter().fold(0f32, |p, v| p.max(v.abs()));
    if peak == 0.0 { return (data.to_vec(), 1.0) }
    (data.iter().map(|v| (v / peak * 127.0).round() / 128.0).collect(), peak * 128.0 / 127.0)
}

/// The run of a captured output: its pan, and every write as (time, value).
struct Run { pan: i32, w: Vec<(f64, f32)> }

/// An output written value by value at VGM frame times (the YM2612 DAC, `$4011`). A run of changes ends once
/// the value has held still for `GAP` frames (or the output is switched off).
pub struct Capture {
    voice: usize,
    /// Output value → sample frame: `(v - center) * scale`; and the level of a full-scale frame.
    center: f32,
    scale: f32,
    amp: f32,
    pub exact: bool,
    cur: f32,
    /// Time of the last change.
    last: f64,
    run: Option<Run>,
    done: Vec<Run>,
}

const GAP: f64 = 2205.0;

impl Capture {
    pub fn new(voice: usize, center: f32, scale: f32, amp: f32) -> Capture {
        Capture { voice, center, scale, amp, exact: false, cur: center, last: 0.0, run: None, done: vec![] }
    }

    /// The output starts at `v` instead of the center (the value before any writes).
    pub fn start_at(&mut self, v: f32) { self.cur = v }

    /// The program writes `v` at frame `t`.
    pub fn set(&mut self, t: f64, v: f32, pan: i32) {
        if self.run.is_some() && t - self.last >= GAP { self.stop(t) }
        if self.run.is_none() {
            if v == self.cur { return }
            self.run = Some(Run { pan, w: vec![] });
        }
        let r = self.run.as_mut().unwrap();
        // (Writes at the same time: only the last counts.)
        if r.w.last().is_some_and(|w| w.0 == t) { r.w.pop(); }
        r.w.push((t, v));
        if v != self.cur { (self.cur, self.last) = (v, t) }
    }

    /// The output is switched off at frame `t` (or the song ends): the run ends with its last change.
    pub fn stop(&mut self, _t: f64) {
        if let Some(mut r) = self.run.take() {
            let last = self.last;
            r.w.retain(|w| w.0 <= last);
            // (A few changes only move the output's DC level, as a driver setting the DMC level does: a click
            // at most. Sounds written value by value take many.)
            if r.w.windows(2).filter(|p| p[0].1 != p[1].1).count() >= 4 { self.done.push(r) }
        }
    }

    pub fn hits(&mut self, defs: &mut Defs, _end: f64) -> Vec<Hit> {
        self.stop(0.0);
        // SiON has 256 sample slots: if the runs don't fit (in the slots left, less a few for other chips), join
        // the closest ones (the output holding its value in between, as it did) until they do.
        let distinct = self.done.iter().map(|r| r.w.iter().map(|w| w.1.to_bits()).collect::<Vec<_>>()).collect::<std::collections::HashSet<_>>().len();
        let budget = 256usize.saturating_sub(defs.pcm.len() + 16).max(1);
        if distinct > budget {
            while self.done.len() > budget {
                let gap = |k: usize| self.done[k].w[0].0 - self.done[k - 1].w[self.done[k - 1].w.len() - 1].0;
                let k = (1..self.done.len()).min_by(|&a, &b| gap(a).total_cmp(&gap(b))).unwrap();
                let next = self.done.remove(k);
                self.done[k - 1].w.extend(next.w);
            }
        }
        let (mut out, mut held) = (vec![], vec![]);
        let mut shared: HashMap<Vec<u32>, (usize, f64)> = HashMap::new();
        let frame = |v: f32| (v - self.center) * self.scale;
        for r in std::mem::take(&mut self.done) {
            // The usual gap between writes (the median); each write held for its gap in those units.
            let mut gaps: Vec<f64> = r.w.windows(2).map(|p| p[1].0 - p[0].0).collect();
            gaps.sort_by(f64::total_cmp);
            let usual = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0);
            if self.exact || r.w.len() < 8 || usual <= 1.0 { held.push(r); continue }
            let (mut data, mut starts) = (vec![], vec![]);
            for (k, w) in r.w.iter().enumerate() {
                let n = r.w.get(k + 1).map_or(1, |next| (((next.0 - w.0) / usual).round() as usize).max(1));
                starts.push(data.len());
                data.extend(std::iter::repeat_n(frame(w.1), n));
            }
            let rate = 44100.0 * data.len() as f64 / (r.w[r.w.len() - 1].0 - r.w[0].0 + usual);
            let Some(first) = data.iter().position(|v| *v != 0.0) else { continue };
            let last = data.iter().rposition(|v| *v != 0.0).unwrap();
            // (Silence trimmed from the front: the sample starts at the write that begins its sound.)
            let k0 = starts.iter().rposition(|&s| s <= first).unwrap();
            let first = starts[k0];
            data = data[first..=last].to_vec();
            let at = r.w[k0].0;
            let key: Vec<u32> = data.iter().map(|v| v.to_bits()).collect();
            let (n, base) = match shared.get(&key) {
                Some(&s) => s,
                None => { let Some(n) = def(defs, &data, rate as f32, None, false) else { continue }; shared.insert(key, (n, rate)); (n, rate) }
            };
            out.push(Hit::new(self.voice, at, at + data.len() as f64 * 44100.0 / rate, n, base, 60.0 + 12.0 * (rate / base).log2(), self.amp, r.pan));
        }
        out.extend(self.held(defs, held));
        out
    }

    /// Runs captured as the output was, frame by frame at 44100 Hz (each written value held until the next).
    fn held(&self, defs: &mut Defs, runs: Vec<Run>) -> Vec<Hit> {
        let done: Vec<(f64, i32, Vec<f32>)> = runs.into_iter().map(|r| {
            let mut data = vec![];
            for (k, w) in r.w.iter().enumerate() {
                let next = r.w.get(k + 1).map_or(w.0 + 1.0, |n| n.0);
                data.extend(std::iter::repeat_n((w.1 - self.center) * self.scale, (next - w.0) as usize));
            }
            (r.w[0].0, r.pan, data)
        }).collect();
        let mut out = vec![];
        for (at, pan, data) in done {
            // Leading and trailing frames at the resting level are silence.
            let Some(first) = data.iter().position(|v| *v != 0.0) else { continue };
            let last = data.iter().rposition(|v| *v != 0.0).unwrap();
            let Some(n) = def(defs, &data[first..=last], 44100.0, None, false) else { continue };
            let at = at + first as f64;
            out.push(Hit::new(self.voice, at, at + (last - first + 1) as f64, n, 44100.0, 60.0, self.amp, pan));
        }
        out
    }
}

/// DAC bytes streamed from a VGM sample bank: runs of consecutive bank bytes, written steadily.
pub struct Bank {
    voice: usize,
    amp: f32,
    /// Runs: first write time, last write time, first byte, byte count, pan.
    cur: Option<(f64, f64, usize, usize, i32)>,
    runs: Vec<(f64, f64, usize, usize, i32)>,
}

impl Bank {
    pub fn new(voice: usize, amp: f32) -> Bank { Bank { voice, amp, cur: None, runs: vec![] } }

    /// Bank byte `at` is written to the DAC at frame `t`.
    pub fn write(&mut self, t: f64, at: usize, pan: i32) {
        if let Some(c) = &mut self.cur {
            // The next byte, soon after: the same run.
            if at == c.2 + c.3 && t - c.1 <= 256.0 { (c.1, c.3) = (t, c.3 + 1); return }
        }
        self.stop();
        self.cur = Some((t, t, at, 1, pan));
    }

    pub fn stop(&mut self) { if let Some(c) = self.cur.take() { self.runs.push(c) } }

    /// Each stretch of the bank that was played (overlapping or touching runs joined) is one sample, at the rate
    /// most of its runs read it; each run plays it from its first byte at its own rate. Silent runs are left out.
    pub fn hits(&mut self, defs: &mut Defs, bank: &[u8]) -> Vec<Hit> {
        self.stop();
        let runs: Vec<_> = self.runs.drain(..).filter(|r| r.3 >= 2 && r.1 > r.0 && bank.get(r.2..(r.2 + r.3).min(bank.len())).is_some_and(|b| b.iter().any(|v| *v != 0x80))).collect();
        let mut order: Vec<usize> = (0..runs.len()).collect();
        order.sort_by_key(|&k| runs[k].2);
        let mut out = vec![];
        let mut k = 0;
        while k < order.len() {
            let (lo, mut hi, mut group) = (runs[order[k]].2, runs[order[k]].2 + runs[order[k]].3, vec![order[k]]);
            k += 1;
            while k < order.len() && runs[order[k]].2 <= hi { hi = hi.max(runs[order[k]].2 + runs[order[k]].3); group.push(order[k]); k += 1 }
            let hi = hi.min(bank.len());
            let rate_of = |r: &(f64, f64, usize, usize, i32)| 44100.0 * (r.3 - 1) as f64 / (r.1 - r.0);
            let mut rates: Vec<f64> = group.iter().map(|&g| rate_of(&runs[g])).collect();
            rates.sort_by(f64::total_cmp);
            // (Rounded to 1/1000 Hz, so the number written is the rate the notes are tuned against.)
            let base = (rates[rates.len() / 2] * 1000.0).round() / 1000.0;
            let data: Vec<f32> = bank[lo..hi].iter().map(|&v| (v as f32 - 128.0) / 128.0).collect();
            let Some(n) = def(defs, &data, base as f32, None, false) else { continue };
            for &g in &group {
                let r = runs[g];
                let rate = rate_of(&r);
                let mut h = Hit::new(self.voice, r.0, r.1 + 44100.0 / rate, n, base, 60.0 + 12.0 * (rate / base).log2(), self.amp, r.4);
                (h.ofs, h.cut) = ((r.2 - lo) as f64, r.2 + r.3 < hi);
                out.push(h);
            }
        }
        out
    }
}

/// YM2610 / YM2608 ADPCM-A: 4-bit ADPCM with a 12-bit accumulator that wraps; `rom[start..end]`, high nibble
/// first. Frames in -1..1 (the accumulator / 2048, exact in 16 bits).
pub fn adpcm_a(rom: &[u8], start: usize, end: usize) -> Vec<f32> {
    const STEPS: [u16; 49] = [16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143, 157,
        173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552];
    const INC: [i32; 8] = [-1, -1, -1, -1, 2, 5, 7, 9];
    let (mut acc, mut step, mut out) = (0i32, 0i32, vec![]);
    for &b in rom.get(start.min(rom.len())..end.min(rom.len())).unwrap_or(&[]) {
        for nib in [b >> 4, b & 15] {
            let d = (2 * (nib & 7) as i32 + 1) * STEPS[step as usize] as i32 / 8;
            acc = (acc + if nib & 8 != 0 { -d } else { d }) & 0xfff;
            step = (step + INC[(nib & 7) as usize]).clamp(0, 48);
            out.push(((acc << 20) >> 20) as f32 / 2048.0);
        }
    }
    out
}

/// YM2610 / YM2608 ADPCM-B (delta-T): `rom[start..=end]`, high nibble first, 16-bit accumulator. Frames in -1..1.
pub fn adpcm_b(rom: &[u8], start: usize, end: usize) -> Vec<f32> {
    const SCALE: [i32; 8] = [57, 57, 57, 57, 77, 102, 128, 153];
    let (mut acc, mut step, mut out) = (0i32, 127i32, vec![]);
    for &b in rom.get(start.min(rom.len())..(end + 1).min(rom.len())).unwrap_or(&[]) {
        for nib in [b >> 4, b & 15] {
            let d = (2 * (nib & 7) as i32 + 1) * step / 8;
            acc = (acc + if nib & 8 != 0 { -d } else { d }).clamp(-32768, 32767);
            step = (step * SCALE[(nib & 7) as usize] / 64).clamp(127, 24576);
            out.push(acc as f32 / 32768.0);
        }
    }
    out
}

/// NES DPCM: `len` bytes from CPU address `addr` (wrapping from $FFFF to $8000), least significant bit first,
/// each bit moving the 7-bit output level from `level` up or down by 2. Frames: the level relative to where it
/// started, / 128 (exact in 8 bits); and the level it ends on.
pub fn dpcm(mem: &[u8], addr: usize, len: usize, level: u8) -> (Vec<f32>, u8) {
    let (mut l, mut out, mut a) = (level as i32, vec![0.0], addr);
    for _ in 0..len {
        let b = mem.get(a).copied().unwrap_or(0);
        a = if a >= 0xffff { 0x8000 } else { a + 1 };
        for k in 0..8 {
            if b >> k & 1 != 0 { if l <= 125 { l += 2 } } else if l >= 2 { l -= 2 }
            out.push((l - level as i32) as f32 / 128.0);
        }
    }
    (out, l as u8)
}
