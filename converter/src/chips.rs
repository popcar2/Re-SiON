//! Sound chip models for VGM playback. Each keeps the registers (and the timers that change the sound on
//! their own: envelopes, length counters, sweeps) and reports, once per frame, how each of its channels sounds.
//! Tones use SiON's chip-like waveforms: pulse widths (`%1@n`, `%8@n`), triangle (`%1@8`), sampled noise
//! (`%2@n`, `%1@9`), wavetables (`%4@n`) and FM voices (`%6@n`, in `fm.rs`).

use crate::pcm::{dpcm, Capture, Hit};
use crate::score::{hz_key, noise_key, noise_level, Defs, Frame, Stream};

pub trait Chip {
    /// A register write (`port` selects the register bank on chips that have two).
    fn write(&mut self, port: u8, reg: u8, val: u8);
    /// Let `secs` seconds pass.
    fn run(&mut self, _secs: f64) {}
    /// How many channels `frame` reports.
    fn chans(&self) -> usize;
    /// Append one frame per channel (`None`: silent), then forget this frame's key-ons.
    fn frame(&mut self, st: &mut Stream, defs: &mut Defs, out: &mut Vec<Option<Frame>>);
    /// What this chip can't convert and was asked to play, if anything.
    fn warnings(&self) -> Vec<String> { vec![] }
    /// The time (VGM frames, 44100 per second) of the writes that follow.
    fn at(&mut self, _t: f64) {}
    /// A VGM data block for this chip: `kind` (the block type), loaded at `start` in its memory.
    fn data(&mut self, _kind: u8, _start: usize, _bytes: &[u8]) {}
    /// The samples it played (`pcm.rs`), once the song has ended at `end`.
    fn hits(&mut self, _defs: &mut Defs, _end: f64) -> Vec<Hit> { vec![] }
    /// A write of byte `src` of the VGM's sample bank (YM2612 DAC data from `0x8n` and DAC streams).
    fn write_src(&mut self, port: u8, reg: u8, val: u8, _src: usize) { self.write(port, reg, val) }
    /// Capture samples exactly, frame by frame (`pcm.rs`), instead of once each at their own rate.
    fn set_exact(&mut self, _exact: bool) {}
    /// The driver's frame rate (frames per second), once known.
    fn set_fps(&mut self, _fps: f64) {}
}

/// Release as fast as the chip does (SiON's default release rate 28 rings for most of a second).
const CUT: &str = "s63";

fn frame(st: &mut Stream, instr: &str, pitch: f32, amp: f32, pan: i32, trig: bool) -> Option<Frame> {
    (amp > 0.0 && pitch.is_finite()).then(|| Frame { pitch: pitch.clamp(0.0, 119.0), amp, tone: st.tone(instr, CUT, false), pan, trig, pos: 0.0, lfo: (0, 0), rel: false })
}

/// Audible frequency range for tones; outside it a channel is treated as silent (drivers park unused
/// channels at extreme periods, or play samples by writing volumes).
fn audible(hz: f64) -> bool { (16.0..=16000.0).contains(&hz) }

// ---------------------------------------------------------------------------------------------------------
/// Texas Instruments SN76489 (Master System, Game Gear, Mega Drive, BBC Micro...): three square waves and a
/// noise channel with 2 dB volume steps.
pub struct Sn76489 { clock: f64, latch: usize, tone: [u16; 3], noise: u8, vol: [u8; 4], stereo: u8, trig: bool }

impl Sn76489 {
    pub fn new(clock: u32) -> Self { Sn76489 { clock: clock as f64, latch: 0, tone: [0; 3], noise: 0, vol: [15; 4], stereo: 0xff, trig: false } }
}

impl Chip for Sn76489 {
    fn write(&mut self, port: u8, _reg: u8, d: u8) {
        if port == 1 { self.stereo = d; return }
        if d & 0x80 != 0 { self.latch = ((d >> 4) & 7) as usize }
        let l = self.latch;
        match (l, d & 0x80 != 0) {
            (6, _) => { self.noise = d & 7; self.trig = true }
            (l, _) if l & 1 == 1 => self.vol[l / 2] = d & 15,
            (l, true) => self.tone[l / 2] = (self.tone[l / 2] & 0x3f0) | (d & 15) as u16,
            (l, false) => self.tone[l / 2] = (self.tone[l / 2] & 15) | ((d & 0x3f) as u16) << 4,
        }
    }
    fn chans(&self) -> usize { 4 }
    fn frame(&mut self, st: &mut Stream, _: &mut Defs, out: &mut Vec<Option<Frame>>) {
        let amp = |v: u8| if v >= 15 { 0.0 } else { 10f32.powf(-0.1 * v as f32) };
        let pan = |c: usize| match (self.stereo >> (c + 4) & 1, self.stereo >> c & 1) { (1, 1) => Some(0), (1, 0) => Some(-64), (0, 1) => Some(64), _ => None };
        for c in 0..3 {
            let hz = self.clock / (32.0 * self.tone[c].max(1) as f64);
            out.push(pan(c).filter(|_| audible(hz)).and_then(|p| frame(st, "%1@4", hz_key(hz), amp(self.vol[c]), p, false)));
        }
        // Noise: the shift register steps at clock/512, /1024, /2048 or with tone 3; "periodic" mode is a
        // 1/16 pulse at a sixteenth of that rate.
        let n = if self.noise & 3 == 3 { self.tone[2].max(1) as f64 } else { (16 << (self.noise & 3)) as f64 };
        let rate = self.clock / (32.0 * n);
        let (f, level) = if self.noise & 4 != 0 { (("%2@1", noise_key(rate)), noise_level(rate)) } else { (("%8@1", hz_key(rate / 16.0)), 1.0) };
        out.push(pan(3).and_then(|p| frame(st, f.0, f.1, amp(self.vol[3]) * level, p, self.trig)));
        self.trig = false;
    }
}

// ---------------------------------------------------------------------------------------------------------
/// General Instrument AY-3-8910 / Yamaha YM2149 (MSX, ZX Spectrum 128, Atari ST, Amstrad CPC), also the SSG
/// part of the YM2203/2608/2610: three squares sharing one noise generator and one envelope generator.
/// Reported as three tone channels, then three noise channels (one per channel that mixes the noise in).
pub struct Ay8910 { clock: f64, r: [u8; 16], env: f64, trig: bool }

/// Output level per volume step (the chip's DAC is roughly 3 dB per step).
const AY_LEVEL: [f32; 16] = [0.0, 0.0105, 0.0152, 0.0221, 0.0321, 0.0467, 0.0637, 0.1068, 0.1237, 0.2050, 0.2928, 0.3728, 0.4925, 0.6353, 0.8056, 1.0];

impl Ay8910 {
    pub fn new(clock: f64) -> Self { Ay8910 { clock, r: [0; 16], env: 0.0, trig: false } }
    fn env_period(&self) -> f64 { (self.r[11] as u32 | (self.r[12] as u32) << 8).max(1) as f64 }
    /// Envelope level 0..15 after `self.env` steps.
    fn env_level(&self) -> usize {
        let (shape, p) = (self.r[13], self.env as u64);
        let (cont, att, alt, hold) = (shape & 8 != 0, shape & 4 != 0, shape & 2 != 0, shape & 1 != 0);
        let (cycle, i) = (p / 16, (p % 16) as usize);
        let up = |a: bool| if a { i } else { 15 - i };
        match cycle {
            0 => up(att),
            _ if !cont => 0,
            _ if hold => if att ^ alt { 15 } else { 0 },
            c => up(att ^ (alt && c % 2 == 1)),
        }
    }
}

impl Chip for Ay8910 {
    fn write(&mut self, _: u8, reg: u8, d: u8) {
        if reg > 15 { return }
        self.r[reg as usize] = d;
        if reg == 13 { self.env = 0.0; self.trig = true }
    }
    fn run(&mut self, secs: f64) {
        self.env += secs * self.clock / (16.0 * self.env_period());
        if self.env > 1e9 { self.env = 32.0 + self.env % 32.0 }
    }
    fn chans(&self) -> usize { 6 }
    fn frame(&mut self, st: &mut Stream, _: &mut Defs, out: &mut Vec<Option<Frame>>) {
        let mix = self.r[7];
        let env_hz = self.clock / (256.0 * self.env_period());
        let shape = self.r[13] & 15;
        let looping = shape & 8 != 0 && shape & 1 == 0;
        let mut noise = [None; 3];
        for c in 0..3 {
            let tp = (self.r[c * 2] as u32 | ((self.r[c * 2 + 1] & 15) as u32) << 8).max(1) as f64;
            let hz = self.clock / (16.0 * tp);
            let (tone_on, noise_on, v) = (mix >> c & 1 == 0, mix >> (c + 3) & 1 == 0, self.r[8 + c]);
            // Envelope mode: a slow envelope is a volume change; a fast repeating one is heard as a tone of its
            // own (the "buzzer" bass) or, under a square, as about half volume.
            let fast = looping && env_hz > 20.0;
            let amp = if v & 16 == 0 { AY_LEVEL[v as usize & 15] } else if fast { 0.5 } else { AY_LEVEL[self.env_level()] };
            out.push(if tone_on && audible(hz) {
                frame(st, "%1@4", hz_key(hz), amp, 0, false)
            } else if !tone_on && !noise_on && v & 16 != 0 && fast {
                let (instr, f) = match shape { 10 | 14 => ("%5@4", env_hz / 2.0), 12 => ("%5@9", env_hz), _ => ("%5@8", env_hz) };
                frame(st, instr, hz_key(f), 0.7, 0, false)
            } else { None });
            if noise_on { noise[c] = Some(amp) }
        }
        let rate = self.clock / (16.0 * (self.r[6] & 31).max(1) as f64);
        for a in noise { out.push(a.and_then(|a| frame(st, "%2@1", noise_key(rate), a * noise_level(rate), 0, self.trig))) }
        self.trig = false;
    }
}

// ---------------------------------------------------------------------------------------------------------
/// Ricoh 2A03 (NES/Famicom APU): two pulse waves, a triangle and noise, with the hardware envelopes,
/// length counters, linear counter and sweeps; and the delta modulation channel's samples (DPCM from memory, and
/// levels written to `$4011`) as embedded samples.
pub struct NesApu {
    cpu: f64,
    r: [u8; 0x18],
    /// Quarter-frame clock phase.
    seq: f64,
    step: u32,
    len: [u8; 4],
    /// Envelope (start flag, divider, decay level) for pulse 1, pulse 2, noise.
    env: [(bool, u8, u8); 3],
    timer: [u16; 2],
    sweep: [(u8, bool); 2],
    lin: (u8, bool),
    trig: [bool; 4],
    fds: bool,
    dmc: Dmc,
}

/// The NES delta modulation channel.
struct Dmc {
    /// CPU memory ($8000-$FFFF from VGM data blocks), the time, the output level.
    mem: Vec<u8>,
    t: f64,
    level: u8,
    /// The playing sample: until when, its hit, and the level it ends on.
    play: Option<(f64, Hit, u8)>,
    raw: Capture,
    hits: Vec<Hit>,
    /// Decoded samples by (address, length, starting level, loop): frames and the level they end on.
    cache: Vec<((usize, usize, u8, bool), Vec<f32>, u8)>,
    /// Per hit: the decoded sample, the rate it started at, whether it loops (numbered in `hits`).
    played: Vec<(usize, f64, bool)>,
}

/// DMC output step in the level scale the other channels use (pulse 15 = 1.0 for a ±1 wave): the APU's linear
/// mix weighs a DMC step 0.00335 and a pulse step 0.00752.
const DMC_UNIT: f32 = 0.00335 / 0.00752 * 2.0 / 15.0;

const NES_LEN: [u8; 32] = [10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22, 192, 24, 72, 26, 16, 28, 32, 30];

impl NesApu {
    pub fn new(clock: u32) -> Self {
        let dmc = Dmc { mem: vec![0; 0x10000], t: 0.0, level: 0, play: None, raw: Capture::new(1, 64.0, 1.0 / 128.0, 128.0 * DMC_UNIT), hits: vec![], cache: vec![], played: vec![] };
        // Channels start enabled: rips of NSF music start after the player's init, which enables them.
        let mut r = [0; 0x18];
        r[0x15] = 0x0f;
        NesApu { cpu: (clock & 0x7fff_ffff) as f64, r, seq: 0.0, step: 0, len: [0; 4], env: [(false, 0, 0); 3], timer: [0; 2], sweep: [(0, false); 2], lin: (0, false), trig: [false; 4], fds: false, dmc }
    }
    /// DMC output rate (bits per second) for rate index `i`: NTSC or PAL timer periods, by the CPU clock.
    fn dmc_rate(&self, i: u8) -> f64 {
        const NTSC: [u16; 16] = [428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 142, 128, 106, 84, 72, 54];
        const PAL: [u16; 16] = [398, 354, 316, 298, 276, 236, 210, 198, 176, 148, 132, 118, 98, 78, 66, 50];
        self.cpu / if self.cpu < 1.7e6 { PAL } else { NTSC }[i as usize & 15] as f64
    }
    /// Bring the DMC up to now: a sample that has played out ends, leaving the level where it got to.
    fn dmc_settle(&mut self) {
        let t = self.dmc.t;
        if let Some((until, ..)) = &self.dmc.play { if *until <= t { let (_, h, l) = self.dmc.play.take().unwrap(); self.dmc.level = l; self.dmc.hits.push(h) } }
    }
    /// $4015 with the DMC bit: start the sample at $4012 / $4013 unless one is playing.
    fn dmc_start(&mut self) {
        self.dmc_settle();
        if self.dmc.play.is_some() { return }
        let (addr, len, lp) = (0xc000 + self.r[0x12] as usize * 64, self.r[0x13] as usize * 16 + 1, self.r[0x10] & 0x40 != 0);
        let key = (addr, len, self.dmc.level, lp);
        let (frames, end_level) = match self.dmc.cache.iter().find(|c| c.0 == key) {
            Some(c) => (c.1.clone(), c.2),
            None => { let (f, l) = dpcm(&self.dmc.mem, addr, len, self.dmc.level); self.dmc.cache.push((key, f.clone(), l)); (f, l) }
        };
        let rate = self.dmc_rate(self.r[0x10]);
        let t = self.dmc.t;
        let until = if lp { f64::INFINITY } else { t + frames.len() as f64 * 44100.0 / rate };
        // (`smp` indexes `played` until the samples are numbered in `hits`.)
        let i = self.dmc.cache.iter().position(|c| c.0 == key).unwrap();
        self.dmc.played.push((i, rate, lp));
        let h = Hit::new(0, t, until, self.dmc.played.len() - 1, rate, 60.0, 128.0 * DMC_UNIT, 0);
        self.dmc.play = Some((until, h, if lp { self.dmc.level } else { end_level }));
    }
    /// Stop the playing sample now (DMC disabled).
    fn dmc_stop(&mut self) {
        self.dmc_settle();
        if let Some((_, mut h, _)) = self.dmc.play.take() { (h.end, h.cut) = (self.dmc.t, true); self.dmc.hits.push(h) }
    }
    fn sweep_target(&self, c: usize) -> i32 {
        let (t, s) = (self.timer[c] as i32, self.r[c * 4 + 1]);
        let d = t >> (s & 7);
        if s & 8 != 0 { t - d - (c == 0) as i32 } else { t + d }
    }
    fn muted(&self, c: usize) -> bool { self.timer[c] < 8 || self.sweep_target(c) > 0x7ff }
    fn quarter(&mut self) {
        for (i, reg) in [(0, 0), (1, 4), (2, 12)] {
            let (r, e) = (self.r[reg], &mut self.env[i]);
            if e.0 { *e = (false, r & 15, 15) } else if e.1 == 0 {
                e.1 = r & 15;
                if e.2 > 0 { e.2 -= 1 } else if r & 0x20 != 0 { e.2 = 15 }
            } else { e.1 -= 1 }
        }
        let r8 = self.r[8];
        if self.lin.1 { self.lin.0 = r8 & 0x7f } else if self.lin.0 > 0 { self.lin.0 -= 1 }
        if r8 & 0x80 == 0 { self.lin.1 = false }
    }
    fn half(&mut self) {
        for (c, halt) in [(0, self.r[0] & 0x20), (1, self.r[4] & 0x20), (2, self.r[8] & 0x80), (3, self.r[12] & 0x20)] {
            if halt == 0 && self.len[c] > 0 { self.len[c] -= 1 }
        }
        for c in 0..2 {
            let s = self.r[c * 4 + 1];
            let (div, reload) = self.sweep[c];
            if div == 0 && s & 0x80 != 0 && s & 7 != 0 && !self.muted(c) { self.timer[c] = self.sweep_target(c).clamp(0, 0x7ff) as u16 }
            self.sweep[c] = if div == 0 || reload { ((s >> 4) & 7, false) } else { (div - 1, false) };
        }
    }
}

impl Chip for NesApu {
    fn write(&mut self, _: u8, reg: u8, d: u8) {
        if reg >= 0x20 { self.fds = true; return }
        let reg = reg as usize;
        if reg < 0x18 { self.r[reg] = d }
        let enabled = |c: usize, r: &[u8]| r[0x15] >> c & 1 != 0;
        match reg {
            0x01 | 0x05 => self.sweep[reg / 4].1 = true,
            0x02 | 0x06 => self.timer[reg / 4] = (self.timer[reg / 4] & 0x700) | d as u16,
            0x03 | 0x07 => {
                let c = reg / 4;
                self.timer[c] = (self.timer[c] & 0xff) | ((d & 7) as u16) << 8;
                if enabled(c, &self.r) { self.len[c] = NES_LEN[d as usize >> 3] }
                (self.env[c].0, self.trig[c]) = (true, true);
            }
            0x0b => {
                if enabled(2, &self.r) { self.len[2] = NES_LEN[d as usize >> 3] }
                (self.lin.1, self.trig[2]) = (true, true);
            }
            0x0f => {
                if enabled(3, &self.r) { self.len[3] = NES_LEN[d as usize >> 3] }
                (self.env[2].0, self.trig[3]) = (true, true);
            }
            0x10 => {
                // A rate change bends a playing sample.
                self.dmc_settle();
                let rate = self.dmc_rate(d);
                if let Some((_, h, _)) = &mut self.dmc.play { h.keys.push((self.dmc.t, 60.0 + 12.0 * (rate / h.rate).log2())) }
            }
            0x11 => { self.dmc.level = d & 0x7f; self.dmc.raw.set(self.dmc.t, (d & 0x7f) as f32, 0) }
            0x13 => {}
            0x15 => {
                for c in 0..4 { if d >> c & 1 == 0 { self.len[c] = 0 } }
                if d & 0x10 != 0 { self.dmc_start() } else { self.dmc_stop() }
            }
            0x17 => { self.seq = 0.0; self.step = 0; if d & 0x80 != 0 { self.quarter(); self.half() } }
            _ => {}
        }
    }
    fn run(&mut self, secs: f64) {
        let five = self.r[0x17] & 0x80 != 0;
        self.seq += secs * if five { 192.0 } else { 240.0 };
        while self.seq >= 1.0 {
            self.seq -= 1.0;
            self.step = (self.step + 1) % if five { 5 } else { 4 };
            if !(five && self.step == 4) { self.quarter() }
            if self.step.is_multiple_of(2) && !(five && self.step == 4) { self.half() }
        }
    }
    fn chans(&self) -> usize { 4 }
    fn frame(&mut self, st: &mut Stream, _: &mut Defs, out: &mut Vec<Option<Frame>>) {
        for c in 0..2 {
            let r0 = self.r[c * 4];
            let v = if r0 & 0x10 != 0 { r0 & 15 } else { self.env[c].2 };
            let hz = self.cpu / (16.0 * (self.timer[c] as f64 + 1.0));
            let duty = ["%1@1", "%1@2", "%1@4", "%1@6"][r0 as usize >> 6];
            let on = self.len[c] > 0 && !self.muted(c) && audible(hz);
            out.push(if on { frame(st, duty, hz_key(hz), v as f32 / 15.0, 0, self.trig[c]) } else { None });
        }
        let t = (self.r[10] as u32 | ((self.r[11] & 7) as u32) << 8) as f64;
        let hz = self.cpu / (32.0 * (t + 1.0));
        let on = self.len[2] > 0 && self.lin.0 > 0 && t >= 2.0 && audible(hz);
        out.push(if on { frame(st, "%1@8", hz_key(hz), 1.13, 0, self.trig[2]) } else { None });
        // Noise: SiON's APU noise (`%1@9` long, `%1@10` short) takes the period index from the note.
        let (rc, re) = (self.r[12], self.r[14]);
        let v = if rc & 0x10 != 0 { rc & 15 } else { self.env[2].2 };
        let instr = if re & 0x80 != 0 { "%1@10" } else { "%1@9" };
        out.push(if self.len[3] > 0 { frame(st, instr, (48 + (re & 15)) as f32, v as f32 / 15.0 * 0.66, 0, self.trig[3]) } else { None });
        self.trig = [false; 4];
    }
    fn warnings(&self) -> Vec<String> {
        let mut w = vec![];
        if self.fds { w.push("Famicom Disk System sound is not converted".into()) }
        w
    }
    fn at(&mut self, t: f64) { self.dmc.t = t }
    fn set_exact(&mut self, exact: bool) { self.dmc.raw.exact = exact }
    /// NES RAM writes (block type 0xC2): the program's memory, where DPCM samples live.
    fn data(&mut self, kind: u8, start: usize, bytes: &[u8]) {
        if kind != 0xc2 { return }
        for (k, b) in bytes.iter().enumerate() { if let Some(m) = self.dmc.mem.get_mut(start + k) { *m = *b } }
    }
    fn hits(&mut self, defs: &mut Defs, end: f64) -> Vec<Hit> {
        self.dmc.t = end;
        self.dmc_settle();
        if let Some((_, mut h, _)) = self.dmc.play.take() { h.end = h.end.min(end); self.dmc.hits.push(h) }
        // Number the samples played (a looping one loops all but its first frame, the level it starts from).
        let mut out: Vec<Hit> = vec![];
        for mut h in self.dmc.hits.drain(..) {
            let (i, rate, lp) = self.dmc.played[h.smp];
            let Some(n) = crate::pcm::def(defs, &self.dmc.cache[i].1, rate as f32, lp.then_some(1), false) else { continue };
            h.smp = n;
            out.push(h);
        }
        out.extend(self.dmc.raw.hits(defs, end));
        out
    }
}

// ---------------------------------------------------------------------------------------------------------
/// Game Boy (DMG) APU: two pulse waves (the first with a frequency sweep), a 32-step 4-bit wavetable channel
/// and noise; with envelopes, length counters and stereo panning.
pub struct GameBoy {
    r: [u8; 0x30],
    seq: f64,
    step: u32,
    on: [bool; 4],
    len: [u16; 4],
    /// Envelope (volume, timer) for channels 1, 2, 4.
    env: [(u8, u8); 3],
    freq: [u16; 3],
    sweep: (u8, bool),
    trig: [bool; 4],
}

impl GameBoy {
    pub fn new() -> Self { GameBoy { r: [0; 0x30], seq: 0.0, step: 0, on: [false; 4], len: [0; 4], env: [(0, 0); 3], freq: [0; 3], sweep: (0, false), trig: [false; 4] } }
    /// Register of channel `c` (0..3), index `i` (NRc0..NRc4).
    fn nr(&self, c: usize, i: usize) -> u8 { self.r[c * 5 + i] }
    fn dac(&self, c: usize) -> bool { if c == 2 { self.r[0x0a] & 0x80 != 0 } else { self.nr(c, 2) & 0xf8 != 0 } }
    fn sweep_calc(&mut self) -> u16 {
        let (f, s) = (self.freq[0], self.r[0]);
        let d = f >> (s & 7);
        let n = if s & 8 != 0 { f.wrapping_sub(d) } else { f + d };
        if n > 2047 { self.on[0] = false }
        n
    }
}

impl Chip for GameBoy {
    fn write(&mut self, _: u8, reg: u8, d: u8) {
        if reg & 0x80 != 0 { return } // second chip
        let reg = reg as usize;
        if reg >= 0x30 { return }
        if reg == 0x16 && d & 0x80 == 0 { *self = GameBoy::new(); return }
        if self.r[0x16] & 0x80 == 0 && reg < 0x20 && reg != 0x16 { return }
        self.r[reg] = d;
        if reg >= 0x20 || reg >= 0x14 { return }
        let (c, i) = (reg / 5, reg % 5);
        match i {
            1 => self.len[c] = if c == 2 { 256 - d as u16 } else { 64 - (d & 63) as u16 },
            2 if !self.dac(c) => self.on[c] = false,
            3 if c < 3 => self.freq[c] = (self.freq[c] & 0x700) | d as u16,
            4 => {
                if c < 3 { self.freq[c] = (self.freq[c] & 0xff) | ((d & 7) as u16) << 8 }
                if d & 0x80 != 0 {
                    self.on[c] = self.dac(c);
                    self.trig[c] = true;
                    if self.len[c] == 0 { self.len[c] = if c == 2 { 256 } else { 64 } }
                    if c != 2 { let e = if c == 3 { 2 } else { c }; self.env[e] = (self.nr(c, 2) >> 4, self.nr(c, 2) & 7) }
                    if c == 0 {
                        self.sweep = (self.r[0] >> 4 & 7, self.r[0] & 0x77 != 0);
                        if self.r[0] & 7 != 0 { self.sweep_calc(); }
                    }
                }
            }
            _ => {}
        }
    }
    fn run(&mut self, secs: f64) {
        self.seq += secs * 512.0;
        while self.seq >= 1.0 {
            self.seq -= 1.0;
            self.step = (self.step + 1) & 7;
            if self.step.is_multiple_of(2) {
                for c in 0..4 {
                    if self.nr(c, 4) & 0x40 != 0 && self.len[c] > 0 {
                        self.len[c] -= 1;
                        if self.len[c] == 0 { self.on[c] = false }
                    }
                }
            }
            if self.step == 2 || self.step == 6 {
                let p = self.r[0] >> 4 & 7;
                if self.sweep.0 > 0 { self.sweep.0 -= 1 }
                if self.sweep.0 == 0 {
                    self.sweep.0 = if p == 0 { 8 } else { p };
                    if self.sweep.1 && p != 0 {
                        let n = self.sweep_calc();
                        if n <= 2047 && self.r[0] & 7 != 0 { self.freq[0] = n; self.sweep_calc(); }
                    }
                }
            }
            if self.step == 7 {
                for (e, c) in [(0, 0), (1, 1), (2, 3)] {
                    let r = self.nr(c, 2);
                    let (v, t) = &mut self.env[e];
                    if r & 7 == 0 { continue }
                    if *t > 0 { *t -= 1 }
                    if *t == 0 {
                        *t = r & 7;
                        if r & 8 != 0 && *v < 15 { *v += 1 } else if r & 8 == 0 && *v > 0 { *v -= 1 }
                    }
                }
            }
        }
    }
    fn chans(&self) -> usize { 4 }
    fn frame(&mut self, st: &mut Stream, defs: &mut Defs, out: &mut Vec<Option<Frame>>) {
        let (nr50, nr51) = (self.r[0x14], self.r[0x15]);
        let master = ((nr50 >> 4 & 7).max(nr50 & 7) + 1) as f32 / 8.0;
        let pan = |c: usize| match (nr51 >> (c + 4) & 1, nr51 >> c & 1) { (1, 1) => Some(0), (1, 0) => Some(-64), (0, 1) => Some(64), _ => None };
        for c in 0..2 {
            let hz = 131072.0 / (2048.0 - self.freq[c] as f64);
            let duty = ["%1@1", "%1@2", "%1@4", "%1@6"][self.nr(c, 1) as usize >> 6];
            let amp = self.env[c].0 as f32 / 15.0 * master;
            out.push(pan(c).filter(|_| self.on[c] && audible(hz)).and_then(|p| frame(st, duty, hz_key(hz), amp, p, self.trig[c])));
        }
        let hz = 65536.0 / (2048.0 - self.freq[2] as f64);
        let shift = [0.0, 1.0, 0.5, 0.25][(self.r[0x0c] >> 5 & 3) as usize];
        let wave: Vec<i32> = self.r[0x20..0x30].iter().flat_map(|b| [b >> 4, b & 15]).map(|s| s as i32 * 16 - 120).collect();
        out.push(pan(2).filter(|_| self.on[2] && audible(hz) && shift > 0.0).and_then(|p| {
            let instr = format!("%4@{}", defs.wave(&wave));
            frame(st, &instr, hz_key(hz), shift * master, p, self.trig[2])
        }));
        let r43 = self.r[0x12];
        let div = if r43 & 7 == 0 { 0.5 } else { (r43 & 7) as f64 };
        let rate = 524288.0 / div / (2u32 << (r43 >> 4)) as f64;
        let instr = if r43 & 8 != 0 { "%2@5" } else { "%2@1" };
        let amp = self.env[2].0 as f32 / 15.0 * master * noise_level(rate);
        out.push(pan(3).filter(|_| self.on[3]).and_then(|p| frame(st, instr, noise_key(rate), amp, p, self.trig[3])));
        self.trig = [false; 4];
    }
}
