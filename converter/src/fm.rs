//! Yamaha FM chips for VGM playback: the OPN family (YM2612 / YM3438 of the Mega Drive, YM2203, YM2608,
//! YM2610) and the OPM (YM2151). Each key-on's patch becomes a SiON FM voice (`#OPN@` / `#OPM@`, `%6@n`),
//! with the carriers' total level taken out of the voice and played as the note's volume, so volume fades
//! and the same patch at different volumes share one voice. Samples (the YM2612 DAC, YM2610 ADPCM-A and -B,
//! YM2608 ADPCM-B) become embedded samples (`pcm.rs`). The OPN LFO becomes SiON's (`@lfo`, `mp`, `ma`), and what
//! a driver does to a channel after its key-off (pitch sweeps, silencing it) shapes the release. SSG-EG, the
//! OPM's LFO, the channel 3 special mode and the YM2608 rhythm sounds (in a ROM inside the chip, which VGM files
//! don't carry) are not converted.

use crate::chips::{Ay8910, Chip};
use crate::pcm::{adpcm_a, adpcm_b, def, small, Bank, Capture, Hit};
use crate::score::{hz_key, Defs, Frame, Stream};

/// One FM channel's registers, operators in SiON's order (S1, S2, S3, S4: the modulation chain of algorithm 0).
#[derive(Clone, Default)]
struct Ch {
    /// Per operator: dt1/mul, tl, ks/ar, am/d1r, dt2/d2r, d1l/rr.
    op: [[u8; 6]; 4],
    alg: u8,
    fb: u8,
    pan: i32,
    keyed: u8,
    trig: bool,
    /// Pitch as (OPN: fnum | block << 11) or (OPM: kc << 8 | kf).
    freq: u32,
    /// Latched high byte of an OPN frequency write.
    hi: u8,
    /// LFO sensitivity: amplitude (0..3) and pitch (0..7), OPN.
    ams: u8,
    pms: u8,
    /// Frames since the key-off, while the release may still be heard.
    off: u32,
}

/// The OPN LFO: per step of its counter (128 steps a cycle), FM samples (clock / divider) per frequency setting.
const LFO_STEP: [f64; 8] = [108.0, 77.0, 71.0, 67.0, 62.0, 44.0, 8.0, 5.0];
/// OPN vibrato depth per PMS, as SiON's `mp` (1/64 semitones: 0, 3.4, 6.7, 10, 14, 20, 40, 80 cents), and tremolo
/// depth per AMS as `ma` on operators with amplitude modulation on (`a1`: 0, 1.4, 5.9, 11.8 dB).
const PMS: [i32; 8] = [0, 2, 4, 6, 9, 13, 26, 51];
const AMS: [i32; 4] = [0, 8, 32, 63];

/// Carrier operators (in S1..S4 order) per algorithm.
const CARRIERS: [u8; 8] = [0b1000, 0b1000, 0b1000, 0b1000, 0b1010, 0b1110, 0b1110, 0b1111];

impl Ch {
    /// The voice (with carrier levels relative to the loudest carrier) and that carrier's gain.
    fn voice(&self, opm: bool) -> (Vec<i32>, f32) {
        let car = CARRIERS[self.alg as usize & 7];
        let min = (0..4).filter(|i| car >> i & 1 != 0).map(|i| self.op[i][1] & 127).min().unwrap_or(0);
        // Feedback: the chips feed S1 the sum of its last two outputs, SiON its last output alone, so SiON's level
        // one higher is the same depth (OPN/OPM 1..7 → 2..8; measured against libvgm's YM2612). Just where the
        // loop turns noisy (deep feedback on a loud S1) the chip's sum keeps it partly tonal and SiON's goes to
        // noise at once: those voices come out hissier.
        let fb = (self.fb & 7) as i32;
        let mut v = vec![(self.alg & 7) as i32, if fb > 0 { fb + 1 } else { 0 }];
        for (i, o) in self.op.iter().enumerate() {
            let tl = (o[1] & 127) - if car >> i & 1 != 0 { min } else { 0 };
            let (ar, dr, sr, rr, sl, ks, ml, dt1) = (o[2] & 31, o[3] & 31, o[4] & 31, o[5] & 15, o[5] >> 4, o[2] >> 6, o[0] & 15, o[0] >> 4 & 7);
            v.extend([ar, dr, sr, rr, sl, tl, ks, ml, dt1].map(|x| x as i32));
            if opm { v.push((o[4] >> 6) as i32) }
            // Amplitude modulation on (OPN; the OPM's LFO isn't converted).
            v.push((!opm && o[3] & 0x80 != 0) as i32);
        }
        (v, 10f32.powf(-0.75 * min as f32 / 20.0))
    }

    /// The channel this frame. `lfo`: the LFO's cycle (1/60 s frames), once the song has turned it on, and whether
    /// it's on. `fps`: driver frames per second.
    fn frame(&mut self, st: &mut Stream, defs: &mut Defs, opm: bool, hz: f64, lfo: (Option<f64>, bool), fps: f64) -> Option<Frame> {
        // (A key-on and key-off within one frame still plays a note, a frame long.)
        let trig = std::mem::take(&mut self.trig);
        if self.keyed != 0 || trig { self.off = 0 } else { self.off += 1 }
        if !(8.0..=12000.0).contains(&hz) { return None }
        let (v, amp) = self.voice(opm);
        if self.keyed == 0 && !trig {
            // Released: the release is heard (to 45 dB down) for as long as the slowest carrier's release rate
            // takes, at most two seconds; the driver may sweep its pitch or turn it off meanwhile.
            let car = CARRIERS[self.alg as usize & 7];
            let rr = (0..4).filter(|i| car >> i & 1 != 0).map(|i| (self.op[i][5] & 15) as i32).min().unwrap_or(15);
            let r = (rr * 4 + 2).min(60);
            let per_sample = crate::score::env_inc(r);
            if self.off as f64 > (480.0 / per_sample / 44100.0 * fps).min(2.0 * fps) { return None }
            return Some(Frame { pitch: hz_key(hz).clamp(0.0, 119.0), amp, tone: 0, pan: self.pan, trig: false, pos: 0.0, lfo: (0, 0), rel: true });
        }
        let n = defs.voice(if opm { "OPM@" } else { "OPN@" }, &v);
        let rate = lfo.0.map_or(String::new(), |c| lfo_cmd(defs, c));
        let tone = st.tone("%6", &format!("@{n}{rate}"), true);
        let am = !opm && self.op.iter().any(|o| o[3] & 0x80 != 0);
        let depth = if lfo.1 { (PMS[self.pms as usize & 7], if am { AMS[self.ams as usize & 3] } else { 0 }) } else { (0, 0) };
        Some(Frame { pitch: hz_key(hz).clamp(0.0, 119.0), amp, tone, pan: self.pan, trig, pos: 0.0, lfo: depth, rel: false })
    }
}

/// SiON's `@lfo` for an LFO cycle of `frames` (1/60 s): its cycle is a whole number of frames, so a cycle that
/// rounds badly becomes several cycles of SiON's triangle in one custom waveform (`#TABLE`, 256 steps) over a whole
/// number of frames (within 0.5%).
fn lfo_cmd(defs: &mut Defs, frames: f64) -> String {
    let err = |k: f64| { let n = (frames * k).round().max(1.0); (n, (n / k / frames - 1.0).abs()) };
    if err(1.0).1 < 0.005 { return format!("@lfo{}", err(1.0).0) }
    // (At most 8 cycles: at least 32 steps each, as many as the chip's vibrato has.)
    let k = (1..=8).find(|&k| err(k as f64).1 < 0.005)
        .unwrap_or_else(|| (1..=8).min_by(|&a, &b| err(a as f64).1.total_cmp(&err(b as f64).1)).unwrap_or(1));
    let (n, _) = err(k as f64);
    // Each cycle: from the middle up to the top, down to the bottom, back to the middle, as SiON's triangle.
    let mut body = vec![];
    for j in 0..k {
        let (a, b) = (256 * j / k, 256 * (j + 1) / k);
        let l = b - a;
        let (q1, q3) = ((l + 2) / 4, (3 * l + 2) / 4);
        body.push(format!("(128,255){q1}(255,0){}(0,128){}", q3 - q1, l - q3));
    }
    format!("@lfo{n},{}", defs.table(&body.concat()))
}

/// Register slot (0, 4, 8, 12 / 0, 8, 16, 24 order) → operator index in S1..S4 order.
const SLOT: [usize; 4] = [0, 2, 1, 3];

/// Which OPN.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind { Ym2612, Ym2203, Ym2608, Ym2610 }

pub struct Opn {
    kind: Kind,
    clock: f64,
    /// Clock divider of the FM part (144 for YM2612/2608/2610, 72 for YM2203).
    div: f64,
    ch: Vec<Ch>,
    ssg: Option<Ay8910>,
    dac: bool,
    /// Channel 3 mode, rhythm, ADPCM fed by the CPU.
    used: (bool, bool, bool),
    /// LFO register (0x22): on, and its cycle in 1/60 s frames once it has been turned on.
    lfo_on: bool,
    lfo_cycle: Option<f64>,
    /// Driver frames per second.
    fps: f64,
    t: f64,
    /// YM2612 DAC output: values written directly, and bytes streamed from the VGM's sample bank (`dac_bank`).
    cap: Capture,
    bank: Bank,
    dac_bank: Vec<u8>,
    exact: bool,
    /// ADPCM-A registers and the six channels' playing hits; ADPCM-B registers and its hit.
    ra: [u8; 0x30],
    play_a: [Option<(Hit, Region)>; 6],
    rb: [u8; 0x10],
    play_b: Option<(Hit, Region)>,
    /// ADPCM-A and ADPCM-B memory (VGM ROM blocks), and decoded regions: `#PCMB` number, frame count, and the
    /// gain that restores the level of 8-bit samples.
    rom: [Vec<u8>; 2],
    decoded: Vec<(Region, Option<usize>, usize, f32)>,
    hits: Vec<(Hit, Region)>,
}

/// An ADPCM sample in memory: ADPCM-A (0) or -B (1), first byte, end (A: exclusive, B: inclusive), repeating.
type Region = (u8, usize, usize, bool);

impl Opn {
    /// `chans`: 3 (YM2203) or 6. `ssg`: the AY-compatible part's clock, if the chip has one.
    pub fn new(kind: Kind, clock: u32, div: f64, chans: usize, ssg: Option<f64>) -> Self {
        let ch = vec![Ch { pan: 0, ..Ch::default() }; chans];
        let mut rb = [0; 0x10];
        (rb[0x0c], rb[0x0d]) = (0xff, 0xff);
        Opn {
            kind, clock: (clock & 0x3fff_ffff) as f64, div, ch, ssg: ssg.map(Ay8910::new), dac: false, used: (false, false, false), t: 0.0,
            lfo_on: false, lfo_cycle: None, fps: 60.0,
            // The DAC replaces channel 6: a full-scale value is a full-scale channel (both 9 bits in the mix).
            cap: Capture::new(0, 128.0, 1.0 / 128.0, 1.0), bank: Bank::new(0, 1.0), dac_bank: vec![], exact: false,
            ra: [0; 0x30], play_a: Default::default(), rb, play_b: None, rom: [vec![], vec![]], decoded: vec![], hits: vec![],
        }
    }

    /// `#PCMB` number, frame count and level gain of a region, decoded once, played at `rate` at key 60: in 8 bits
    /// scaled to its peak, or (`exact`) as decoded.
    fn decode(&mut self, defs: &mut Defs, r: Region, rate: f64) -> (Option<usize>, usize, f32) {
        if let Some(d) = self.decoded.iter().find(|d| d.0 == r) { return (d.1, d.2, d.3) }
        let (which, start, end, lp) = r;
        let frames = if which == 0 { adpcm_a(&self.rom[0], start, end) } else { adpcm_b(&self.rom[1], start, end) };
        let (frames, gain) = if self.exact { (frames, 1.0) } else { small(&frames) };
        let n = if frames.is_empty() { None } else { def(defs, &frames, rate as f32, lp.then_some(0), false) };
        self.decoded.push((r, n, frames.len(), gain));
        (n, frames.len(), gain)
    }

    fn pan(l: bool, r: bool) -> i32 { match (l, r) { (true, false) => -64, (false, true) => 64, _ => 0 } }

    /// ADPCM-A level of channel `c`: total and channel attenuation in a pseudo-logarithmic multiply and shift,
    /// against an FM channel (both as the YM2610 mixes them).
    fn level_a(&self, c: usize) -> f32 {
        let vol = (self.ra[0x08 + c] & 0x1f ^ 0x1f) as i32 + (self.ra[0x01] & 0x3f ^ 0x3f) as i32;
        if vol >= 63 || self.ra[0x08 + c] & 0xc0 == 0 { 0.0 } else { (15 - (vol & 7)) as f32 / 4.0 / (1 << (vol >> 3)) as f32 }
    }

    /// ADPCM-B level: linear 0..255, against an FM channel.
    fn level_b(&self) -> f32 { if self.rb[0x01] & 0xc0 == 0 { 0.0 } else { self.rb[0x0b] as f32 / 64.0 } }

    fn end_hit(h: Option<(Hit, Region)>, t: f64, out: &mut Vec<(Hit, Region)>) {
        if let Some((mut h, r)) = h { h.end = h.end.min(t); if h.end > h.at { out.push((h, r)) } }
    }

    fn write_a(&mut self, reg: u8, d: u8) {
        let reg = reg as usize;
        if reg >= 0x30 { return }
        self.ra[reg] = d;
        let t = self.t;
        match reg {
            0x00 => for c in (0..6).filter(|c| d >> c & 1 != 0) {
                Self::end_hit(self.play_a[c].take(), t, &mut self.hits);
                if d & 0x80 != 0 { continue }
                let start = (self.ra[0x10 + c] as usize | (self.ra[0x18 + c] as usize) << 8) << 8;
                let end = ((self.ra[0x20 + c] as usize | (self.ra[0x28 + c] as usize) << 8) + 1) << 8;
                // (`smp` is set when `hits` decodes the region.)
                let pan = Self::pan(self.ra[0x08 + c] & 0x80 != 0, self.ra[0x08 + c] & 0x40 != 0);
                self.play_a[c] = Some((Hit::new(c + 1, t, f64::INFINITY, 0, self.clock / 432.0, 60.0, self.level_a(c), pan), (0, start, end, false)));
            },
            0x01 => for c in 0..6 { let l = self.level_a(c); if let Some((h, _)) = &mut self.play_a[c] { h.vols.push((t, l)) } },
            0x08..=0x0d => { let c = reg - 8; let l = self.level_a(c); if let Some((h, _)) = &mut self.play_a[c] { h.vols.push((t, l)) } }
            _ => {}
        }
    }

    fn write_b(&mut self, reg: u8, d: u8) {
        let reg = reg as usize;
        if reg >= 0x10 { return }
        self.rb[reg] = d;
        let t = self.t;
        let key = |rb: &[u8; 0x10]| { let dn = rb[0x09] as u32 | (rb[0x0a] as u32) << 8; 60.0 + 12.0 * (dn.max(1) as f64 / 65536.0).log2() };
        match reg {
            0x00 => {
                Self::end_hit(self.play_b.take(), t, &mut self.hits);
                if d & 0x80 == 0 || d & 0x01 != 0 { return }
                // The YM2610 always plays from its ROM; the YM2608 only with the external-memory bit (else the CPU
                // feeds it, which isn't converted).
                if self.kind == Kind::Ym2608 && d & 0x20 == 0 { self.used.2 = true; return }
                // Addresses count 256-byte units on the YM2610; on the YM2608 32 bytes (ROM, 8-bit RAM) or 4.
                let shift = match self.kind { Kind::Ym2610 => 8, _ if self.rb[0x01] & 3 != 0 => 5, _ => 2 };
                let start = (self.rb[0x02] as usize | (self.rb[0x03] as usize) << 8) << shift;
                let end = (((self.rb[0x04] as usize | (self.rb[0x05] as usize) << 8) + 1) << shift) - 1;
                let pan = Self::pan(self.rb[0x01] & 0x80 != 0, self.rb[0x01] & 0x40 != 0);
                self.play_b = Some((Hit::new(7, t, f64::INFINITY, 0, self.clock / 144.0, key(&self.rb), self.level_b(), pan), (1, start, end, d & 0x10 != 0)));
            }
            0x09 | 0x0a => { let k = key(&self.rb); if let Some((h, _)) = &mut self.play_b { h.keys.push((t, k)) } }
            0x01 | 0x0b => { let l = self.level_b(); if let Some((h, _)) = &mut self.play_b { h.vols.push((t, l)) } }
            _ => {}
        }
    }
}

impl Chip for Opn {
    fn write(&mut self, port: u8, reg: u8, d: u8) {
        if port == 0 && reg < 0x10 {
            if let Some(s) = &mut self.ssg { s.write(0, reg, d) }
            return;
        }
        if reg < 0x30 {
            match (self.kind, port, reg) {
                (Kind::Ym2610, 1, _) => return self.write_a(reg, d),
                (Kind::Ym2608, 1, _) => return self.write_b(reg, d),
                (Kind::Ym2610, 0, 0x10..=0x1f) => return self.write_b(reg - 0x10, d),
                (_, 0, _) => {}
                _ => return,
            }
            match reg {
                0x28 => {
                    let c = (d & 3) as usize + if d & 4 != 0 { 3 } else { 0 };
                    if let Some(ch) = self.ch.get_mut(c).filter(|_| d & 3 != 3) {
                        let k = (d >> 4) & 15; // S1, S2, S3, S4
                        if k != 0 && ch.keyed == 0 { ch.trig = true }
                        ch.keyed = k;
                    }
                }
                0x2a if self.kind == Kind::Ym2612 => {
                    if self.dac { self.bank.stop(); let pan = self.ch.get(5).map_or(0, |c| c.pan); self.cap.set(self.t, d as f32, pan) }
                }
                0x2b if self.kind == Kind::Ym2612 => {
                    self.dac = d & 0x80 != 0;
                    if !self.dac { self.cap.stop(); self.bank.stop() }
                }
                // (The YM2203 has no LFO.)
                0x22 if self.kind != Kind::Ym2203 => {
                    self.lfo_on = d & 8 != 0;
                    if self.lfo_on {
                        // SiON's LFO cycle in 1/60 s frames (its cycle runs 0.39% long: 256 steps of 0.17294 ms·44.1 kHz).
                        let hz = self.clock / self.div / (LFO_STEP[d as usize & 7] * 128.0);
                        self.lfo_cycle = Some(60.0 / hz / (0.17294118 * 256.0 / 44.1));
                    }
                }
                0x27 if d & 0xc0 != 0 => self.used.0 = true,
                0x10..=0x1f => self.used.1 = true, // YM2608 rhythm
                _ => {}
            }
            return;
        }
        let n = (reg & 3) as usize;
        if n == 3 { return }
        let Some(ch) = self.ch.get_mut(n + 3 * port as usize) else { return };
        match reg {
            0x30..=0x9f => {
                let o = SLOT[((reg >> 2) & 3) as usize];
                let i = match reg >> 4 { 3 => 0, 4 => 1, 5 => 2, 6 => 3, 7 => 4, 8 => 5, _ => return };
                ch.op[o][i] = if i == 4 { d & 31 } else { d };
            }
            0xa4..=0xa6 => ch.hi = d,
            0xa0..=0xa2 => ch.freq = d as u32 | ((ch.hi & 7) as u32) << 8 | ((ch.hi >> 3 & 7) as u32) << 11,
            0xb0..=0xb2 => (ch.alg, ch.fb) = (d & 7, d >> 3 & 7),
            0xb4..=0xb6 => (ch.pan, ch.ams, ch.pms) = (match d >> 6 { 3 | 0 => 0, 2 => -64, _ => 64 }, d >> 4 & 3, d & 7),
            _ => {}
        }
    }
    fn run(&mut self, secs: f64) { if let Some(s) = &mut self.ssg { s.run(secs) } }
    fn chans(&self) -> usize { self.ch.len() + self.ssg.as_ref().map_or(0, |s| s.chans()) }
    fn frame(&mut self, st: &mut Stream, defs: &mut Defs, out: &mut Vec<Option<Frame>>) {
        let n = self.ch.len();
        for (i, ch) in self.ch.iter_mut().enumerate() {
            let (fnum, block) = ((ch.freq & 0x7ff) as f64, (ch.freq >> 11) as i32);
            let hz = fnum * 2f64.powi(block - 1) * self.clock / self.div / 1048576.0;
            let f = ch.frame(st, defs, false, hz, (self.lfo_cycle, self.lfo_on), self.fps);
            // Channel 6 plays samples while the DAC is on.
            out.push(f.filter(|_| !(self.dac && i == 5 && n == 6)));
        }
        if let Some(s) = &mut self.ssg {
            // The SSG part's level against a full-scale FM channel, as libvgm mixes them: a full-volume square
            // 6 dB under the FM channel on the YM2203, level with it on the YM2608 and YM2610.
            let level = if self.kind == Kind::Ym2203 { 0.355 } else { 0.708 };
            let from = out.len();
            s.frame(st, defs, out);
            for f in out[from..].iter_mut().flatten() { f.amp *= level }
        }
    }
    fn warnings(&self) -> Vec<String> {
        let mut w = vec![];
        if self.used.0 { w.push("FM channel 3 special mode is not converted".into()) }
        if self.used.1 { w.push("YM2608 rhythm sounds are not converted".into()) }
        if self.used.2 { w.push("ADPCM fed by the CPU (not from memory) is not converted".into()) }
        if let Some(s) = &self.ssg { w.extend(s.warnings()) }
        w
    }
    fn at(&mut self, t: f64) { self.t = t }
    fn set_exact(&mut self, exact: bool) { (self.exact, self.cap.exact) = (exact, exact) }
    fn set_fps(&mut self, fps: f64) { self.fps = fps }
    fn write_src(&mut self, port: u8, reg: u8, d: u8, src: usize) {
        if self.exact || self.kind != Kind::Ym2612 || port != 0 || reg != 0x2a { return self.write(port, reg, d) }
        if !self.dac { return }
        // A bank byte: the direct-write capture ends where the bank takes over.
        self.cap.stop();
        let pan = self.ch.get(5).map_or(0, |c| c.pan);
        self.bank.write(self.t, src, pan);
    }
    /// ROM blocks: 0x82 YM2610 ADPCM-A, 0x83 YM2610 ADPCM-B, 0x81 YM2608 ADPCM-B; and the YM2612 DAC's sample bank
    /// (type 0).
    fn data(&mut self, kind: u8, start: usize, bytes: &[u8]) {
        if kind == 0 && self.kind == Kind::Ym2612 {
            if self.dac_bank.len() < start + bytes.len() { self.dac_bank.resize(start + bytes.len(), 0x80) }
            self.dac_bank[start..start + bytes.len()].copy_from_slice(bytes);
            return;
        }
        let which = match (self.kind, kind) { (Kind::Ym2610, 0x82) => 0, (Kind::Ym2610, 0x83) | (Kind::Ym2608, 0x81) => 1, _ => return };
        let m = &mut self.rom[which];
        if m.len() < start + bytes.len() { m.resize(start + bytes.len(), 0) }
        m[start..start + bytes.len()].copy_from_slice(bytes);
    }
    fn hits(&mut self, defs: &mut Defs, end: f64) -> Vec<Hit> {
        for c in 0..6 { Self::end_hit(self.play_a[c].take(), end, &mut self.hits) }
        Self::end_hit(self.play_b.take(), end, &mut self.hits);
        let mut out = self.cap.hits(defs);
        out.extend(self.bank.hits(defs, &self.dac_bank));
        // Decode each ADPCM region played: one-shots end with their data, at the rate they started at.
        for (mut h, r) in std::mem::take(&mut self.hits) {
            let (Some(n), len, gain) = self.decode(defs, r, h.rate) else { continue };
            h.smp = n;
            h.amp *= gain;
            for v in &mut h.vols { v.1 *= gain }
            let natural = h.at + len as f64 * 44100.0 / h.play_rate();
            h.cut = h.end < natural;
            if !r.3 { h.end = h.end.min(natural) }
            if h.end > h.at { out.push(h) }
        }
        out
    }
}

/// Yamaha YM2151 (OPM): eight 4-operator channels (X68000, arcade boards, Sharp X1).
pub struct Opm { clock: f64, ch: Vec<Ch>, lfo: bool, fps: f64 }

impl Opm {
    pub fn new(clock: u32) -> Self { Opm { clock: (clock & 0x3fff_ffff) as f64, ch: vec![Ch::default(); 8], lfo: false, fps: 60.0 } }
}

impl Chip for Opm {
    fn write(&mut self, _: u8, reg: u8, d: u8) {
        let c = (reg & 7) as usize;
        match reg {
            0x08 => {
                let ch = &mut self.ch[(d & 7) as usize];
                let k = (d >> 3) & 15; // M1, C1, M2, C2 = S1, S2, S3, S4
                if k != 0 && ch.keyed == 0 { ch.trig = true }
                ch.keyed = k;
            }
            0x19 if d & 0x7f != 0 => self.lfo = true,
            0x20..=0x27 => {
                let ch = &mut self.ch[c];
                (ch.alg, ch.fb) = (d & 7, d >> 3 & 7);
                ch.pan = match d >> 6 { 3 | 0 => 0, 1 => -64, _ => 64 };
            }
            0x28..=0x2f => self.ch[c].freq = (self.ch[c].freq & 0xff) | (d as u32) << 8,
            0x30..=0x37 => self.ch[c].freq = (self.ch[c].freq & !0xff) | d as u32,
            0x40..=0xff => {
                let o = SLOT[((reg >> 3) & 3) as usize];
                let i = match reg >> 5 { 2 => 0, 3 => 1, 4 => 2, 5 => 3, 6 => 4, 7 => 5, _ => return };
                self.ch[c].op[o][i] = d;
            }
            _ => {}
        }
    }
    fn chans(&self) -> usize { 8 }
    fn frame(&mut self, st: &mut Stream, defs: &mut Defs, out: &mut Vec<Option<Frame>>) {
        for ch in &mut self.ch {
            // Key code: octave and a note code (0 = C#, with every fourth code unused), plus 1/64 fractions.
            let (kc, kf) = (ch.freq >> 8 & 0x7f, (ch.freq & 0xfc) as f64 / 256.0);
            let note = [0, 1, 2, 2, 3, 4, 5, 5, 6, 7, 8, 8, 9, 10, 11, 11][kc as usize & 15] as f64;
            let key = 61.0 + ((kc >> 4) as f64 - 4.0) * 12.0 + note + kf + 12.0 * (self.clock / 3579545.0).log2();
            let hz = 440.0 * 2f64.powf((key - 69.0) / 12.0);
            out.push(ch.frame(st, defs, true, hz, (None, false), self.fps));
        }
    }
    fn warnings(&self) -> Vec<String> { if self.lfo { vec!["FM LFO is not converted".into()] } else { vec![] } }
    fn set_fps(&mut self, fps: f64) { self.fps = fps }
}
