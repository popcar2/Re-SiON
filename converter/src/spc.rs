//! SPC (SNES music) reader. An SPC file is a snapshot of the SNES's sound unit: the SPC700 CPU's registers, its
//! 64 KB of RAM (the game's sound driver, the song and the samples) and the S-DSP's registers. `spc700.rs` runs
//! the driver from there, as on the console (timers, I/O ports, the IPL ROM), and a model of the DSP follows
//! what it is asked to play: key-ons and key-offs, each voice's sample (BRR blocks, loops) and pitch, and its
//! envelope, step by step at the DSP's 32 kHz rate, with the hardware's ADSR and GAIN modes and rate counter.
//!
//! Each key-on becomes a note playing its sample as an embedded one (`#PCMB`, decoded from BRR, at the pitch it
//! was played at): the hardware envelope as SiON's own (`@n,ar,dr,sr,rr,sl`), VOL as the note's volume and pan,
//! and the echo as a delay send (`#EFFECT1`, `@v`). Like an NSF, an SPC holds no song length: the song loops where
//! the driver's state comes round again, or where its writes repeat, or ends at the ID666 tag's play time.

use std::collections::HashMap;
use std::f64::consts::PI;

use crate::pcm::b64;
use crate::score::{hz_key, noise_key, noise_level, Defs, Note, Score};
use crate::spc700::{Bus, Cpu};

pub fn detect(d: &[u8]) -> bool { d.starts_with(b"SNES-SPC700 Sound File Data") }

/// How long to play an SPC that neither loops nor says how long it is.
const MAX_SECS: f64 = 600.0;
/// SPC700 cycles per second; the DSP makes one sample every 32 of them.
const CLOCK: f64 = 1_024_000.0;
const RATE: f64 = 32000.0;

/// The boot ROM mapped at $FFC0 while CONTROL bit 7 is set.
const IPL: [u8; 64] = [
    0xcd, 0xef, 0xbd, 0xe8, 0x00, 0xc6, 0x1d, 0xd0, 0xfc, 0x8f, 0xaa, 0xf4, 0x8f, 0xbb, 0xf5, 0x78,
    0xcc, 0xf4, 0xd0, 0xfb, 0x2f, 0x19, 0xeb, 0xf4, 0xd0, 0xfc, 0x7e, 0xf4, 0xd0, 0x0b, 0xe4, 0xf5,
    0xcb, 0xf4, 0xd7, 0x00, 0xfc, 0xd0, 0xf3, 0xab, 0x01, 0x10, 0xef, 0x7e, 0xf4, 0x10, 0xeb, 0xba,
    0xf6, 0xda, 0x00, 0xba, 0xf4, 0xc4, 0xf4, 0xdd, 0x5d, 0xd0, 0xdb, 0x1f, 0x00, 0x00, 0xc0, 0xff,
];

/// Envelope and noise rates: samples per step (0: never), and the phase of the DSP's shared counter.
const RATES: [u32; 32] = [0, 2048, 1536, 1280, 1024, 768, 640, 512, 384, 320, 256, 192, 160, 128, 96, 80, 64, 48, 40, 32, 24, 20, 16, 12, 10, 8, 6, 5, 4, 3, 2, 1];
const OFFSETS: [u32; 32] = [1, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 536, 0, 1040, 0, 0];

// DSP registers.
const MVOLL: usize = 0x0c;
const MVOLR: usize = 0x1c;
const EVOLL: usize = 0x2c;
const EVOLR: usize = 0x3c;
const KON: usize = 0x4c;
const KOFF: usize = 0x5c;
const FLG: usize = 0x6c;
const ENDX: usize = 0x7c;
const EFB: usize = 0x0d;
const PMON: usize = 0x2d;
const NON: usize = 0x3d;
const EON: usize = 0x4d;
const DIR: usize = 0x5d;
const EDL: usize = 0x7d;

fn mix(h: u64, v: u64) -> u64 { (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95) }

/// ID666 tag: names, and how long the song plays (with its fade), if given.
#[derive(Default)]
struct Tags { title: String, game: String, artist: String, secs: Option<f64> }

fn tags(d: &[u8]) -> Tags {
    if d.get(0x23) != Some(&26) || d.len() < 0x100 { return Tags::default() }
    let text = |a: usize, n: usize| String::from_utf8_lossy(&d[a..a + n]).split('\0').next().unwrap_or("").trim().to_string();
    // Text and binary tags differ in where the play time and artist are; a text tag's times are digits.
    let digits = |a: usize, n: usize| d[a..a + n].iter().all(|&c| c == 0 || c == b' ' || c.is_ascii_digit());
    let (secs, fade, artist) = if digits(0xa9, 3) && digits(0xac, 5) {
        let num = |a: usize, n: usize| text(a, n).parse::<f64>().unwrap_or(0.0);
        (num(0xa9, 3), num(0xac, 5), text(0xb1, 32))
    } else {
        ((d[0xa9] as u32 | (d[0xaa] as u32) << 8 | (d[0xab] as u32) << 16) as f64, u32::from_le_bytes([d[0xac], d[0xad], d[0xae], d[0xaf]]) as f64, text(0xb0, 32))
    };
    Tags { title: text(0x2e, 32), game: text(0x4e, 32), artist, secs: (secs > 0.0).then(|| secs + fade / 1000.0) }
}

/// One of the three timers: counting its base clock (8 kHz, or 64 kHz for timer 2) up to its target, then
/// stepping its 4-bit output counter, which a read clears.
#[derive(Default)]
struct Timer { on: bool, target: u8, div: u64, out: u8, period: u64, at: u64 }

impl Timer {
    fn sync(&mut self, now: u64) {
        let n = now / self.period - self.at / self.period;
        self.at = now;
        if !self.on || n == 0 { return }
        let t = if self.target == 0 { 256 } else { self.target as u64 };
        let total = self.div + n;
        self.out = ((self.out as u64 + total / t) & 15) as u8;
        self.div = total % t;
    }
}

const ATTACK: u8 = 0;
const DECAY: u8 = 1;
const SUSTAIN: u8 = 2;
const RELEASE: u8 = 3;

#[derive(Clone, Copy, Default)]
struct Voice { delay: u8, mode: u8, env: i32, hidden: i32, addr: u16, chunk: u8, interp: u32, header: u8 }

/// A decoded sample: its frames (16-bit), and the frame its loop starts on.
struct Brr { data: Vec<i16>, lp: Option<usize> }

/// A key-on: when it started (DSP samples), the voice, its sample (index into `Dsp::samples`), and all the DSP
/// registers as they were.
struct KeyOn { t: u64, voice: usize, smp: usize, regs: [u8; 128] }

/// The S-DSP as far as the conversion needs it: which voices play what, and their envelopes. (No audio: the
/// samples' sound is SiON's to make.)
struct Dsp {
    regs: [u8; 128],
    v: [Voice; 8],
    new_kon: u8,
    counter: u32,
    /// Samples made so far.
    t: u64,
    /// Register writes: (sample, register, value).
    writes: Vec<(u64, u8, u8)>,
    keyons: Vec<KeyOn>,
    /// Key-offs (KOFF, or a soft reset): (sample, voice).
    offs: Vec<(u64, usize)>,
    /// Each voice's envelope level (0..2047) every 32 samples (1 ms).
    envs: [Vec<u16>; 8],
    samples: Vec<Brr>,
    cache: HashMap<(u16, u16, u64), usize>,
}

impl Dsp {
    fn sync(&mut self, cycle: u64, ram: &[u8]) { while self.t < cycle / 32 { self.step(ram) } }

    fn read(&self, a: u8) -> u8 {
        let a = a as usize & 0x7f;
        match a & 15 {
            8 if a < 0x80 => (self.v[a >> 4].env >> 4) as u8,
            9 => 0,
            _ => self.regs[a],
        }
    }

    fn write(&mut self, a: u8, v: u8) {
        let a = a as usize;
        self.writes.push((self.t, a as u8, v));
        match a {
            KON => self.new_kon |= v,
            ENDX => self.regs[ENDX] = 0,
            _ => self.regs[a] = v,
        }
    }

    /// Where voice `i`'s sample `srcn` starts and loops, from the sample directory.
    fn dir(&self, ram: &[u8], srcn: u8) -> (u16, u16) {
        let e = (self.regs[DIR] as usize * 256 + srcn as usize * 4) & 0xffff;
        let w = |k: usize| ram[(e + k) & 0xffff] as u16 | (ram[(e + k + 1) & 0xffff] as u16) << 8;
        (w(0), w(2))
    }

    /// The sample a key-on plays, decoded from RAM as it is now (cached by its blocks' contents).
    fn sample(&mut self, ram: &[u8], start: u16, lp: u16) -> usize {
        let (blocks, at) = chain(ram, start, lp);
        let h = blocks.iter().fold(0u64, |h, b| b.iter().fold(h, |h, &x| mix(h, x as u64)));
        if let Some(&i) = self.cache.get(&(start, lp, h)) { return i }
        self.samples.push(Brr { data: decode(&blocks), lp: at.map(|i| i * 16) });
        self.cache.insert((start, lp, h), self.samples.len() - 1);
        self.samples.len() - 1
    }

    fn step(&mut self, ram: &[u8]) {
        self.counter = if self.counter == 0 { 0x77ff } else { self.counter - 1 };
        let even = self.t & 1 == 0;
        let (kon, koff) = if even { (std::mem::take(&mut self.new_kon), self.regs[KOFF]) } else { (0, 0) };
        let flg = self.regs[FLG];
        let counter = self.counter;
        let fires = |r: usize| RATES[r] != 0 && (counter + OFFSETS[r]) % RATES[r] == 0;
        for i in 0..8 {
            let (bit, r) = (1u8 << i, i * 16);
            if even && (koff & bit != 0 || flg & 0x80 != 0) && self.v[i].mode != RELEASE && self.v[i].delay == 0 {
                self.v[i].mode = RELEASE;
                self.offs.push((self.t, i));
            }
            if kon & bit != 0 {
                let (start, lp) = self.dir(ram, self.regs[r + 4]);
                let smp = self.sample(ram, start, lp);
                self.keyons.push(KeyOn { t: self.t + 5, voice: i, smp, regs: self.regs });
                self.regs[ENDX] &= !bit;
                let v = &mut self.v[i];
                (v.delay, v.mode) = (5, ATTACK);
            }
            if self.v[i].delay > 0 {
                let start = self.dir(ram, self.regs[r + 4]).0;
                let v = &mut self.v[i];
                if v.delay == 5 { (v.addr, v.chunk, v.header) = (start, 0, ram[start as usize]) }
                (v.env, v.hidden, v.interp) = (0, 0, 0);
                v.delay -= 1;
                continue;
            }
            // Envelope.
            let v = &mut self.v[i];
            let (adsr1, adsr2, gain) = (self.regs[r + 5], self.regs[r + 6], self.regs[r + 7]);
            let mut env = v.env;
            if v.mode == RELEASE {
                env = (env - 8).max(0);
                v.env = env;
            } else {
                let (rate, data);
                if adsr1 & 0x80 != 0 {
                    data = adsr2;
                    if v.mode >= DECAY {
                        env -= 1;
                        env -= env >> 8;
                        rate = if v.mode == DECAY { (adsr1 as usize >> 3 & 0x0e) + 0x10 } else { adsr2 as usize & 0x1f };
                    } else {
                        rate = (adsr1 as usize & 0x0f) * 2 + 1;
                        env += if rate < 31 { 0x20 } else { 0x400 };
                    }
                } else {
                    data = gain;
                    let mode = gain >> 5;
                    if mode < 4 {
                        env = gain as i32 * 0x10;
                        rate = 31;
                    } else {
                        rate = gain as usize & 0x1f;
                        match mode {
                            4 => env -= 0x20,
                            5 => { env -= 1; env -= env >> 8 }
                            _ => { env += 0x20; if mode > 6 && v.hidden as u32 >= 0x600 { env += 0x8 - 0x20 } }
                        }
                    }
                }
                if env >> 8 == (data >> 5) as i32 && v.mode == DECAY { v.mode = SUSTAIN }
                v.hidden = env;
                if env as u32 > 0x7ff {
                    env = if env < 0 { 0 } else { 0x7ff };
                    if v.mode == ATTACK { v.mode = DECAY }
                }
                if fires(rate) { v.env = env }
            }
            // Pitch and the BRR block being played.
            let p = self.regs[r + 2] as u32 | (self.regs[r + 3] as u32 & 0x3f) << 8;
            v.interp = (v.interp & 0x3fff) + p;
            if v.interp >= 0x4000 {
                v.chunk += 1;
                if v.chunk == 4 {
                    v.chunk = 0;
                    if v.header & 1 != 0 {
                        self.regs[ENDX] |= bit;
                        let lp = self.dir(ram, self.regs[r + 4]).1;
                        self.v[i].addr = lp;
                    } else {
                        self.v[i].addr = self.v[i].addr.wrapping_add(9);
                    }
                    let v = &mut self.v[i];
                    v.header = ram[v.addr as usize];
                }
            }
            // A block that ends without looping is silent: the voice stops there.
            let v = &mut self.v[i];
            if v.header & 3 == 1 || flg & 0x80 != 0 { (v.mode, v.env) = (RELEASE, 0) }
        }
        if self.t % 32 == 0 { for (e, v) in self.envs.iter_mut().zip(&self.v) { e.push(v.env as u16) } }
        self.t += 1;
    }
}

/// The 9-byte BRR blocks a sample plays, in order: from `start` to its end block, then (if it loops back to a
/// block not among those) from `lp` to the end again. A sample that ends without looping leaves out its last
/// block, which the DSP never plays. Also the block its loop starts on.
fn chain(ram: &[u8], start: u16, lp: u16) -> (Vec<[u8; 9]>, Option<usize>) {
    let read = |a: u16| -> [u8; 9] { std::array::from_fn(|k| ram[(a as usize + k) & 0xffff]) };
    let follow = |from: u16, out: &mut Vec<[u8; 9]>, addrs: &mut Vec<u16>| {
        let mut a = from;
        for _ in 0..0x2000 {
            let b = read(a);
            out.push(b);
            addrs.push(a);
            if b[0] & 1 != 0 { break }
            a = a.wrapping_add(9);
        }
    };
    let (mut blocks, mut addrs) = (vec![], vec![]);
    follow(start, &mut blocks, &mut addrs);
    if blocks.last().is_some_and(|b| b[0] & 3 == 1) { blocks.pop(); return (blocks, None) }
    if let Some(i) = addrs.iter().position(|&a| a == lp) { return (blocks, Some(i)) }
    let at = blocks.len();
    follow(lp, &mut blocks, &mut vec![]);
    if blocks.last().is_some_and(|b| b[0] & 3 == 1) { blocks.pop(); }
    let looped = blocks.len() > at;
    (blocks, looped.then_some(at))
}

/// BRR blocks → 16-bit frames, as the DSP decodes them (4-bit samples, a shift and one of four prediction
/// filters, kept to 15 bits).
fn decode(blocks: &[[u8; 9]]) -> Vec<i16> {
    let (mut out, mut p1, mut p2) = (Vec::with_capacity(blocks.len() * 16), 0i32, 0i32);
    for b in blocks {
        let (shift, filter) = (b[0] >> 4, b[0] >> 2 & 3);
        for k in 0..16 {
            let nib = (b[1 + k / 2] >> if k & 1 == 0 { 4 } else { 0 }) & 15;
            let n = ((nib as i32) << 28) >> 28;
            let mut s = if shift <= 12 { (n << shift) >> 1 } else if n < 0 { -2048 } else { 0 };
            let q = p2 >> 1;
            match filter {
                1 => s += (p1 >> 1) + ((-p1) >> 5),
                2 => s += p1 - q + (q >> 4) + ((p1 * -3) >> 6),
                3 => s += p1 - q + ((p1 * -13) >> 7) + ((q * 3) >> 4),
                _ => {}
            }
            let s = (s.clamp(-32768, 32767) * 2) as i16 as i32;
            (p2, p1) = (p1, s);
            out.push(s as i16);
        }
    }
    out
}

/// The sound unit as the driver sees it: RAM, the timers, the I/O ports, the DSP.
struct Apu {
    ram: Vec<u8>,
    rom: [u8; 64],
    ipl: bool,
    ports: [u8; 4],
    timers: [Timer; 3],
    addr: u8,
    dsp: Dsp,
    /// The RAM's contents as a sum of (address, value) hashes, kept up to date by every write.
    hash: u64,
    /// The DSP writes since the last timer tick (a hash), and whether a timer read found a tick.
    sig: u64,
    tick: bool,
}

/// A RAM cell's share of the RAM's hash (splitmix64 of address and value, so different changes can't cancel).
fn cell(a: u16, v: u8) -> u64 {
    let mut z = ((a as u64) << 8 | v as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Apu {
    /// The state the driver's future depends on.
    fn state(&self, cpu: &Cpu) -> u64 {
        let mut h = mix(self.hash, cpu.pc as u64 | (cpu.a as u64) << 16 | (cpu.x as u64) << 24 | (cpu.y as u64) << 32 | (cpu.sp as u64) << 40 | (cpu.psw as u64) << 48);
        for (k, &r) in self.dsp.regs.iter().enumerate() { if k & 15 != 8 && k & 15 != 9 && k != ENDX && k != KON { h = mix(h, (k as u64) << 8 | r as u64) } }
        for t in &self.timers { h = mix(h, t.on as u64 | (t.target as u64) << 8) }
        h
    }
}

impl Bus for Apu {
    fn read(&mut self, a: u16, cycle: u64) -> u8 {
        match a {
            0xf2 => self.addr,
            0xf3 => { self.dsp.sync(cycle, &self.ram); self.dsp.read(self.addr) }
            0xf4..=0xf7 => self.ports[a as usize - 0xf4],
            0xfd..=0xff => {
                let t = &mut self.timers[a as usize - 0xfd];
                t.sync(cycle);
                let v = std::mem::take(&mut t.out);
                if v != 0 { self.tick = true }
                v
            }
            0xf0 | 0xf1 | 0xfa..=0xfc => 0,
            0xffc0..=0xffff if self.ipl => self.rom[a as usize - 0xffc0],
            _ => self.ram[a as usize],
        }
    }

    fn write(&mut self, a: u16, v: u8, cycle: u64) {
        match a {
            0xf1 => {
                for (k, t) in self.timers.iter_mut().enumerate() {
                    t.sync(cycle);
                    let on = v >> k & 1 != 0;
                    if on && !t.on { (t.div, t.out) = (0, 0) }
                    t.on = on;
                }
                if v & 0x10 != 0 { self.ports[0] = 0; self.ports[1] = 0 }
                if v & 0x20 != 0 { self.ports[2] = 0; self.ports[3] = 0 }
                self.ipl = v & 0x80 != 0;
            }
            0xf2 => self.addr = v,
            0xf3 if self.addr < 0x80 => {
                self.dsp.sync(cycle, &self.ram);
                self.dsp.write(self.addr, v);
                self.sig = mix(self.sig, (self.addr as u64) << 8 | v as u64);
            }
            0xfa..=0xfc => { let t = &mut self.timers[a as usize - 0xfa]; t.sync(cycle); t.target = v }
            _ => {}
        }
        if !(0xf0..=0xff).contains(&a) {
            let old = self.ram[a as usize];
            self.hash = self.hash.wrapping_sub(cell(a, old)).wrapping_add(cell(a, v));
            self.ram[a as usize] = v;
        }
    }
}

/// What the driver played: the DSP's log, the song's end and loop point (DSP samples), and the tags.
struct Played { dsp: Dsp, regs0: [u8; 128], end: u64, lp: Option<u64>, tags: Tags }

fn play(d: &[u8], warnings: &mut Vec<String>) -> Result<Played, String> {
    if d.len() < 0x10180 { return Err("truncated SPC file".into()) }
    let tags = tags(d);
    let ram = d[0x100..0x10100].to_vec();
    let regs: [u8; 128] = d[0x10100..0x10180].try_into().unwrap();
    let mut rom = IPL;
    if let Some(r) = d.get(0x101c0..0x10200).filter(|r| r.iter().any(|&b| b != 0)) { rom.copy_from_slice(r) }
    let hash = ram.iter().enumerate().fold(0u64, |h, (a, &v)| h.wrapping_add(cell(a as u16, v)));
    let mut timers: [Timer; 3] = Default::default();
    for (k, t) in timers.iter_mut().enumerate() {
        (t.on, t.target, t.out, t.period) = (ram[0xf1] >> k & 1 != 0, ram[0xfa + k], ram[0xfd + k] & 15, if k == 2 { 16 } else { 128 });
    }
    let mut dsp = Dsp { regs, v: Default::default(), new_kon: 0, counter: 0, t: 0, writes: vec![], keyons: vec![], offs: vec![], envs: Default::default(), samples: vec![], cache: HashMap::new() };
    // (The snapshot's voices are silent: the DSP's own state isn't in the file.)
    for v in &mut dsp.v { v.mode = RELEASE }
    dsp.regs[KON] = 0;
    let mut apu = Apu { ipl: ram[0xf1] & 0x80 != 0, ports: [ram[0xf4], ram[0xf5], ram[0xf6], ram[0xf7]], addr: ram[0xf2], ram, rom, timers, dsp, hash, sig: 0, tick: false };
    let mut cpu = Cpu { pc: u16::from_le_bytes([d[0x25], d[0x26]]), a: d[0x27], x: d[0x28], y: d[0x29], psw: d[0x2a], sp: d[0x2b], ..Cpu::default() };

    // Run until the driver's state repeats (the song loops), or the time runs out.
    let max = (tags.secs.map_or(MAX_SECS, |s| s.max(MAX_SECS)) * CLOCK) as u64;
    let (mut seen, mut ticks, mut sigs) = (HashMap::new(), vec![], vec![]);
    let mut exact = None;
    while cpu.cycles < max && !cpu.halted {
        cpu.step(&mut apu);
        if apu.tick {
            apu.tick = false;
            let k = ticks.len();
            ticks.push(cpu.cycles);
            sigs.push(std::mem::take(&mut apu.sig));
            if let Some(j) = seen.insert(apu.state(&cpu), k) { exact = Some((j, k)); break }
        }
    }
    if cpu.halted { warnings.push("the driver stopped the CPU (SLEEP or STOP)".into()) }
    apu.dsp.sync(cpu.cycles, &apu.ram);
    let now = apu.dsp.t;
    let at = |cycles: u64| cycles / 32;
    let (end, lp) = match exact {
        Some((j, k)) => (at(ticks[k]), Some(at(ticks[j]))),
        None => {
            // The writes repeating (ticks with writes, so a driver's tempo counter can move them by a tick), with a
            // loop of at least a second; or the tag's play time; or all of it.
            let busy: Vec<usize> = (0..sigs.len()).filter(|&k| sigs[k] != 0).collect();
            let per_sec = busy.len() as f64 / (now as f64 / RATE).max(1.0);
            let sig: Vec<u64> = busy.iter().map(|&k| sigs[k]).collect();
            // Failing that (drivers whose vibrato runs on through the loop), the key-ons repeating: voice, sample
            // and pitch.
            let kons = &apu.dsp.keyons;
            let ksig: Vec<u64> = kons.iter().map(|k| { let r = k.voice * 16; mix(mix(k.voice as u64, k.smp as u64), (k.regs[r + 2] as u64) << 8 | k.regs[r + 3] as u64) }).collect();
            let kps = kons.len() as f64 / (now as f64 / RATE).max(1.0);
            match crate::nsf::repeat(&sig, per_sec as usize) {
                Some((j, p)) if j + p < busy.len() => (at(ticks[busy[j + p]]), Some(at(ticks[busy[j]]))),
                _ if let Some((j, p)) = crate::nsf::repeat(&ksig, (kps as usize).max(1)).filter(|&(j, p)| j + p < kons.len()) => (kons[j + p].t, Some(kons[j].t)),
                _ => match tags.secs {
                    Some(s) => ((s * RATE) as u64, None),
                    None => { warnings.push(format!("no loop found in the first {} minutes: converted that much", MAX_SECS / 60.0)); (now, None) }
                },
            }
        }
    };
    // A loop found by the driver's state can be several times round the music (its tempo counter coming back
    // into phase only then): if the key-ons repeat sooner within it, loop there.
    let (end, lp) = match lp {
        Some(l) => {
            let kons: Vec<&KeyOn> = apu.dsp.keyons.iter().filter(|k| k.t < end).collect();
            let ksig: Vec<u64> = kons.iter().map(|k| { let r = k.voice * 16; mix(mix(k.voice as u64, k.smp as u64), (k.regs[r + 2] as u64) << 8 | k.regs[r + 3] as u64) }).collect();
            let kps = kons.len() as f64 / (end as f64 / RATE).max(1.0);
            match crate::nsf::repeat(&ksig, (kps as usize).max(1)) {
                Some((j, p)) if j + p < kons.len() && kons[j + p].t > l && kons[j + p].t - kons[j].t >= 2 * RATE as u64 && kons[j + p].t - kons[j].t < end - l => (kons[j + p].t, Some(kons[j].t)),
                _ => (end, lp),
            }
        }
        None => (end, lp),
    };
    // A loop found partway into a note: if the key-ons just before it come round the same way at the end, it
    // starts with the key-on before it (end and loop moved back together).
    let (end, lp) = match lp {
        Some(l) => {
            let kons = &apu.dsp.keyons;
            let same = |a: &KeyOn, b: &KeyOn| { let r = a.voice * 16; a.voice == b.voice && a.smp == b.smp && a.regs[r + 2..r + 4] == b.regs[r + 2..r + 4] };
            let first = kons.iter().rev().find(|k| k.t <= l).map(|k| k.t);
            match first {
                Some(f) if l - f > 0 && l - f < RATE as u64 / 4 => {
                    let span = end - l;
                    let before: Vec<&KeyOn> = kons.iter().filter(|k| k.t >= f && k.t < l).collect();
                    let again: Vec<&KeyOn> = kons.iter().filter(|k| k.t + span >= f + span && k.t < end && k.t + 64 >= f + span).filter(|k| k.t < l + span).collect();
                    let ok = before.len() == again.len() && before.iter().zip(&again).all(|(a, b)| same(a, b) && (b.t as i64 - (a.t + span) as i64).abs() <= 64);
                    if ok { (end - (l - f), Some(f)) } else { (end, lp) }
                }
                _ => (end, lp),
            }
        }
        None => (end, lp),
    };
    let regs0 = regs;
    Ok(Played { dsp: apu.dsp, regs0, end: end.min(now), lp, tags })
}

/// A voice's envelope as the hardware runs it from a key-on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Env {
    /// ADSR: attack, decay, sustain level, sustain rate.
    Adsr(u8, u8, u8, u8),
    /// GAIN: a fixed level (0..127).
    Direct(u8),
    /// GAIN: mode (4 linear down, 5 exponential down, 6 linear up, 7 bent up) and rate.
    Slope(u8, u8),
}

impl Env {
    fn of(regs: &[u8], v: usize) -> Env {
        let (a1, a2, g) = (regs[v * 16 + 5], regs[v * 16 + 6], regs[v * 16 + 7]);
        if a1 & 0x80 != 0 { Env::Adsr(a1 & 15, a1 >> 4 & 7, a2 >> 5, a2 & 31) }
        else if g & 0x80 == 0 { Env::Direct(g & 0x7f) } else { Env::Slope(g >> 5, g & 31) }
    }
    /// Turns a sounding voice down to silence.
    fn falls(self) -> bool { matches!(self, Env::Slope(4 | 5, r) if r > 0) || self == Env::Direct(0) }
}

/// SiON's envelope rate step (`inc` in the `sion` crate): level units (1/64 octave) per 44.1 kHz frame.
fn sion_inc(r: i32) -> f64 { if r <= 0 { 0.0 } else { let r = r.min(60); 2.064e-4 * (1u32 << (r >> 2)) as f64 * (4 + (r & 3)) as f64 / 8.0 } }

/// The SiON rate (1..63) whose step is closest to `a` units per frame; 0 for none.
fn sion_rate(a: f64) -> i32 {
    if a <= 0.0 { return 0 }
    if a > sion_inc(60) * 1.5 { return 63 }
    (1..=60).min_by(|&x, &y| (sion_inc(x) / a).ln().abs().total_cmp(&(sion_inc(y) / a).ln().abs())).unwrap()
}

/// SiON rates for an exponential fall of one DSP step (-1/256 of the level) every `p` samples, a linear rise of
/// 1/64 of full scale every `p` (matched at half level: SiON's attack is a curve), and a linear fall of
/// `step` / 2048 every `p` (matched 20 dB down).
fn exp_rate(p: u32) -> i32 { if p == 0 { 0 } else { sion_rate(0.2620 / p as f64) } }
fn rise_rate(p: u32) -> i32 { if p == 0 { 0 } else if p == 1 { 63 } else { sion_rate(37.8 / (44.1 * p as f64)) } }
fn fall_rate(p: u32, step: f64) -> i32 { if p == 0 { 0 } else { sion_rate(213.0 / (1842.0 / step * p as f64 * 44100.0 / RATE)) } }

/// The release after a key-off: -8 every sample.
const KEY_OFF: (u32, f64) = (1, 8.0);

/// A note before it is written: its tone is decided once every note of the same sample and envelope is known.
struct Raw {
    voice: usize,
    start: u64,
    end: u64,
    smp: usize,
    noise: Option<f64>,
    env: Env,
    /// How it's turned down at its end: (rate period, step) of a linear fall, or the period of an exponential.
    rel: Rel,
    /// Envelope as volume steps instead (the driver changing it while the note plays): (sample, level 0..1).
    manual: Option<Vec<(u64, f32)>>,
    /// Pitch (P register) and amplitude/pan at the start, then changes: (sample, value).
    p0: u32,
    pitch: Vec<(u64, u32)>,
    vol0: (f32, i32),
    vols: Vec<(u64, (f32, i32))>,
    echo: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Rel { Linear(u32, f64), Exp(u32) }

/// VOL × MVOL as SiON's amplitude and pan (constant power: a centred note is 0.71 on each side).
fn vol_pan(regs: &[u8], v: usize) -> (f32, i32) {
    let l = regs[v * 16] as i8 as f32 * regs[MVOLL] as i8 as f32 / 16384.0;
    let r = regs[v * 16 + 1] as i8 as f32 * regs[MVOLR] as i8 as f32 / 16384.0;
    let (l, r) = (l.abs(), r.abs());
    let a = (l * l + r * r).sqrt();
    let pan = if a == 0.0 { 0 } else { (r.atan2(l) as f64 * 256.0 / PI - 64.0).round() as i32 };
    (a, pan.clamp(-64, 64))
}

/// Row length and phase (seconds): the longest row (40..400 ms) that the key-ons line up with about as well as
/// with any row (phase coherence, over 8 s windows so a slightly wrong length doesn't drift out of phase; notes
/// off the grid, like grace notes, only lower it a little), then fitted over the whole song.
fn grid(times: &[f64]) -> (f64, f64) {
    let mut t: Vec<f64> = times.to_vec();
    t.sort_by(f64::total_cmp);
    t.dedup_by(|b, a| *b - *a < 0.004);
    if t.len() < 8 { return (0.125, t.first().copied().unwrap_or(0.0)) }
    let windows: Vec<&[f64]> = {
        let (mut out, mut k) = (vec![], 0);
        while k < t.len() { let e = t[k..].partition_point(|&x| x < t[k] + 8.0) + k; out.push(&t[k..e]); k = e }
        out
    };
    let coherence = |p: f64| {
        let (mut sum, mut n) = (0.0, 0usize);
        for w in &windows {
            let (s, c) = w.iter().fold((0.0, 0.0), |a, &x| { let ph = 2.0 * PI * x / p; (a.0 + ph.sin(), a.1 + ph.cos()) });
            sum += (s * s + c * c).sqrt();
            n += w.len();
        }
        sum / n as f64
    };
    let mut cands = vec![];
    let mut p = 0.4;
    while p > 0.04 { cands.push((p, coherence(p))); p *= 0.9997 }
    let best = cands.iter().map(|c| c.1).fold(0.0, f64::max);
    let (mut per, _) = *cands.iter().find(|c| c.1 >= best * 0.9).unwrap();
    // Phase: the mean of the key-ons modulo the row.
    let phase_of = |p: f64, pts: &[f64]| { let (s, c) = pts.iter().fold((0.0, 0.0), |a, &x| { let ph = 2.0 * PI * x / p; (a.0 + ph.sin(), a.1 + ph.cos()) }); s.atan2(c) / (2.0 * PI) * p };
    let mut ph = phase_of(per, windows[0]);
    // Refine: number the key-ons along the grid and fit a line through those near it.
    for _ in 0..4 {
        let pts: Vec<(f64, f64)> = t.iter().map(|&x| (((x - ph) / per).round(), x)).filter(|&(n, x)| (x - ph - n * per).abs() <= per * 0.15).collect();
        if pts.len() < 4 { break }
        let m = pts.len() as f64;
        let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / m, pts.iter().map(|p| p.1).sum::<f64>() / m);
        let (sxy, sxx) = pts.iter().fold((0.0, 0.0), |s, p| (s.0 + (p.0 - mx) * (p.1 - my), s.1 + (p.0 - mx).powi(2)));
        if sxx <= 0.0 { break }
        let b = sxy / sxx;
        if (b / per - 1.0).abs() > 0.02 { break }
        (per, ph) = (b, my - b * mx);
    }
    // (Moved back by whole rows to the first key-on, so every note is at or after it.)
    (per, ph - per * ((ph - t[0]) / per).ceil())
}

/// The key a sample plays at P = $1000 (32 kHz): the whole part from its pitch (the period of its loop, or of its
/// start), the fraction from how the driver tunes it (the notes it plays should come out whole).
fn base_key(b: &Brr, ps: &[u32]) -> f64 {
    let (mut s, mut c) = (0f64, 0f64);
    for &p in ps { let u = 12.0 * (p.max(1) as f64 / 4096.0).log2(); s += (2.0 * PI * u).sin(); c += (2.0 * PI * u).cos() }
    let frac = if s == 0.0 && c == 0.0 { 0.0 } else { -s.atan2(c) / (2.0 * PI) };
    let mut ks: Vec<f64> = ps.iter().map(|&p| 12.0 * (p.max(1) as f64 / 4096.0).log2()).collect();
    ks.sort_by(f64::total_cmp);
    let mid = ks.get(ks.len() / 2).copied().unwrap_or(0.0);
    // (Unpitched: put its usual note at o5c.)
    let mut k = period(b).map_or(60.0 - mid, |per| hz_key(RATE / per) as f64);
    // (Pitch detection can lock onto a harmonic: keep the usual note within o3..o7, by octaves.)
    while k + mid > 96.0 { k -= 12.0 }
    while k + mid < 36.0 { k += 12.0 }
    (k - frac).round() + frac
}

/// The DSP's Gaussian interpolation, as far as it differs from SiON's linear one: a gentle low-pass in the
/// sample's own frames (`GAUSS` either side; a loop's frames wrap round to its start).
const GAUSS: f64 = 0.15;
fn soften(d: &[i16], lp: Option<usize>) -> Vec<i16> {
    let n = d.len();
    let at = |i: isize| -> f64 {
        if i < 0 { return 0.0 }
        let i = i as usize;
        match lp { _ if i < n => d[i] as f64, Some(l) if n > l => d[l + (i - n) % (n - l)] as f64, _ => 0.0 }
    };
    (0..n as isize).map(|i| (GAUSS * at(i - 1) + (1.0 - 2.0 * GAUSS) * at(i) + GAUSS * at(i + 1)).round().clamp(-32768.0, 32767.0) as i16).collect()
}

/// The fundamental period (frames at 32 kHz) of a sample's loop (or of its start, if it doesn't loop), by YIN.
fn period(b: &Brr) -> Option<f64> {
    let seg: Vec<f32> = match b.lp {
        Some(l) if b.data.len() - l >= 16 => b.data[l..].iter().cycle().take(4096.max(2 * (b.data.len() - l))).map(|&v| v as f32).collect(),
        _ => b.data.iter().skip(b.data.len().min(256)).take(4096).map(|&v| v as f32).collect(),
    };
    let w = seg.len() / 2;
    if w < 64 { return None }
    let max = w.min(2048);
    let mut d = vec![0f32; max + 1];
    for (tau, dt) in d.iter_mut().enumerate().skip(1) { *dt = (0..w).map(|j| { let x = seg[j] - seg[j + tau]; x * x }).sum() }
    let mut run = 0f32;
    let mut cm = vec![1f32; max + 1];
    for tau in 1..=max { run += d[tau]; cm[tau] = if run > 0.0 { d[tau] * tau as f32 / run } else { 1.0 } }
    let tau = (8..max).find(|&t| cm[t] < 0.15 && cm[t] <= cm[t + 1])?;
    let (a, b2, c) = (cm[tau - 1] as f64, cm[tau] as f64, cm[tau + 1] as f64);
    let den = a - 2.0 * b2 + c;
    Some(tau as f64 + if den.abs() > 1e-12 { 0.5 * (a - c) / den } else { 0.0 })
}

pub fn to_score(data: &[u8]) -> Result<(Score, Vec<String>), String> {
    let mut warnings = vec![];
    let Played { dsp, regs0, end, lp, tags } = play(data, &mut warnings)?;
    if dsp.keyons.is_empty() { return Err("the SPC plays no notes".into()) }

    // The register values as they change: replay the writes.
    let at_reg = |from: &[u8; 128], t0: u64, t1: u64| dsp.writes[dsp.writes.partition_point(|w| w.0 < t0)..dsp.writes.partition_point(|w| w.0 < t1)].to_vec().into_iter().scan(*from, |r, (t, a, v)| { r[a as usize] = v; Some((t, a, *r)) }).collect::<Vec<_>>();

    // Notes: each key-on, until the voice's next one (or it falls silent).
    let mut raws: Vec<Raw> = vec![];
    let (mut pmon, mut changed) = (false, 0usize);
    for (i, k) in dsp.keyons.iter().enumerate() {
        if k.t >= end { break }
        let v = k.voice;
        let next = dsp.keyons[i + 1..].iter().find(|n| n.voice == v).map_or(end, |n| n.t).min(end);
        // The last moment it is heard.
        let envs = &dsp.envs[v];
        let (a, b) = ((k.t / 32) as usize, ((next / 32) as usize).min(envs.len()));
        let Some(last) = (a..b).rev().find(|&j| envs[j] > 0) else { continue };
        let heard = ((last as u64 + 1) * 32).min(next);
        let off = dsp.offs.iter().find(|o| o.1 == v && o.0 > k.t && o.0 < heard).map(|o| o.0);
        let bit = 1u8 << v;
        pmon |= k.regs[PMON] & bit != 0 && v > 0;
        let env = Env::of(&k.regs, v);
        let (mut rel, mut stop, mut manual) = (Rel::Linear(KEY_OFF.0, KEY_OFF.1), off.unwrap_or(heard), false);
        let (mut pitch, mut vols) = (vec![], vec![]);
        let p_of = |r: &[u8; 128]| r[v * 16 + 2] as u32 | (r[v * 16 + 3] as u32 & 0x3f) << 8;
        let (p0, vol0) = (p_of(&k.regs), vol_pan(&k.regs, v));
        let (mut cur_env, mut cur_p, mut cur_vol) = (env, p0, vol0);
        for (t, a, r) in at_reg(&k.regs, k.t, heard) {
            let a = a as usize;
            if a >> 4 == v && (5..=7).contains(&(a & 15)) {
                let e = Env::of(&r, v);
                if e == cur_env { continue }
                // Turned down to silence: a release (or the note's end), as long as nothing changes after it.
                if e.falls() && t < stop && !manual {
                    stop = t;
                    rel = match e { Env::Slope(4, r) => Rel::Linear(RATES[r as usize], 32.0), Env::Slope(_, r) => Rel::Exp(RATES[r as usize]), _ => Rel::Linear(1, 2048.0) };
                    cur_env = e;
                    continue;
                }
                if t < stop { manual = true }
                cur_env = e;
            } else if a == v * 16 + 2 || a == v * 16 + 3 {
                let p = p_of(&r);
                if p != cur_p && t < stop { pitch.push((t, p)); cur_p = p }
            } else if a == v * 16 || a == v * 16 + 1 || a == MVOLL || a == MVOLR {
                let vp = vol_pan(&r, v);
                if vp != cur_vol && t < stop { vols.push((t, vp)); cur_vol = vp }
            }
        }
        changed += manual as usize;
        // The envelope followed step by step, as volume (every millisecond it changes).
        let manual = manual.then(|| (a..((stop / 32) as usize).min(envs.len())).map(|j| (j as u64 * 32, envs[j] as f32 / 2047.0)).collect::<Vec<_>>());
        let noise = (k.regs[NON] & bit != 0).then(|| { let r = RATES[k.regs[FLG] as usize & 31]; if r == 0 { 0.0 } else { RATE / r as f64 } });
        let echo = k.regs[EON] & bit != 0;
        raws.push(Raw { voice: v, start: k.t, end: stop.max(k.t + 1), smp: k.smp, noise, env, rel, manual, p0, pitch, vol0, vols, echo });
    }
    if pmon { warnings.push("pitch modulation (PMON) is not converted".into()) }
    if changed > 0 { warnings.push(format!("{changed} notes change their envelope as they play: written as volume steps")) }

    // Each sample's tuning, from the pitches it is played at.
    let mut usage: HashMap<usize, Vec<u32>> = HashMap::new();
    for r in raws.iter().filter(|r| r.noise.is_none()) { usage.entry(r.smp).or_default().push(r.p0) }
    let base: HashMap<usize, f64> = usage.iter().map(|(&s, ps)| (s, base_key(&dsp.samples[s], ps))).collect();
    let key_of = |r: &Raw, p: u32| match r.noise {
        Some(hz) => if hz > 0.0 { noise_key(hz) as f64 } else { 0.0 },
        None => base[&r.smp] + 12.0 * (p.max(1) as f64 / 4096.0).log2(),
    };

    // Timing: rows on the key-ons' grid, 12 ticks each (1/16 note: a quarter is 48 ticks).
    let secs = |t: u64| t as f64 / RATE;
    let (row, phase) = grid(&raws.iter().map(|r| secs(r.start)).collect::<Vec<_>>());
    let tick = |t: u64| ((secs(t) - phase) / (row / 12.0)).round().max(0.0) as u64;
    // Key-ons on the nearest whole row, half row, triplet or 1/64 note close by (drivers' timing jitters by a
    // timer tick or two).
    let on_grid = |t: u64| {
        let x = ((secs(t) - phase) / (row / 12.0)).max(0.0);
        for (unit, tol) in [(12.0, 2.0), (6.0, 1.5), (4.0, 1.0), (3.0, 1.0)] {
            let s = (x / unit).round() * unit;
            if (x - s).abs() <= tol { return s as u64 }
        }
        x.round() as u64
    };

    // Echo: the settings that hold longest, as a delay on send 1.
    let mut held: HashMap<Vec<u8>, u64> = HashMap::new();
    let echo_regs = |r: &[u8; 128]| { let mut k = vec![r[EDL] & 15, r[EFB], r[EVOLL], r[EVOLR], r[MVOLL], r[MVOLR], r[FLG] & 0x20]; k.extend((0..8).map(|i| r[i * 16 + 15])); k };
    let (mut cur, mut since) = (regs0, 0u64);
    for &(t, a, v) in dsp.writes.iter().take_while(|w| w.0 < end) {
        let before = echo_regs(&cur);
        cur[a as usize] = v;
        if echo_regs(&cur) != before { *held.entry(before).or_default() += t - since; since = t }
    }
    *held.entry(echo_regs(&cur)).or_default() += end.saturating_sub(since);
    let e = held.into_iter().max_by_key(|x| x.1).unwrap().0;
    let fir: Vec<f64> = e[7..15].iter().map(|&c| c as i8 as f64 / 128.0).collect();
    // (The FIR's gain in the midrange, where most of an echo is heard.)
    let w = 2.0 * PI * 1000.0 / RATE;
    let (re, im) = fir.iter().enumerate().fold((0.0, 0.0), |s, (k, &c)| (s.0 + c * (w * k as f64).cos(), s.1 - c * (w * k as f64).sin()));
    let g = (re * re + im * im).sqrt();
    let (evol, mvol) = (((e[2] as i8 as f64).abs() + (e[3] as i8 as f64).abs()) / 2.0, ((e[4] as i8 as f64).abs() + (e[5] as i8 as f64).abs()) / 2.0);
    let send = if e[0] > 0 && e[6] == 0 && mvol > 0.0 { (64.0 * g * evol / mvol).round() as i32 } else { 0 };
    let echo_on = send > 0 && raws.iter().any(|r| r.echo);
    let mut header = vec![];
    if echo_on {
        let fb = (-(e[1] as i8 as f64) / 128.0 * g * 100.0).clamp(-99.0, 99.0);
        header.push(format!("#EFFECT1{{delay{},{},0,100}}", e[0] as u32 * 16, fb.round()));
    }

    // Tones: the sample and its envelope; SiON's rates go up with the key (by `kc >> 5`), so each tone's are
    // taken down by that at its usual key.
    let mut defs = Defs::default();
    let mut pcm: HashMap<usize, Option<usize>> = HashMap::new();
    let mut keys: HashMap<(usize, Env, bool), Vec<f64>> = HashMap::new();
    for r in &raws { keys.entry((r.smp, r.env, r.noise.is_some())).or_default().push(key_of(r, r.p0)) }
    let ks: HashMap<(usize, Env, bool), i32> = keys.into_iter().map(|(k, mut v)| { v.sort_by(f64::total_cmp); let m = v[v.len() / 2].round() as i32; (k, ((m - 16).clamp(0, 127)) >> 5) }).collect();
    let mut notes = vec![];
    let mut full = false;
    for r in &raws {
        let instr = if r.noise.is_some() { "%2".to_string() } else {
            let Some(n) = *pcm.entry(r.smp).or_insert_with(|| {
                let b = &dsp.samples[r.smp];
                let bk = base[&r.smp];
                let key = bk.round();
                let rate = (RATE * 2f64.powf((key - bk) / 12.0) * 1000.0).round() / 1000.0;
                let bytes: Vec<u8> = soften(&b.data, b.lp).iter().flat_map(|v| v.to_le_bytes()).collect();
                defs.pcm(format!("{{{}}}{rate},{key},{},1", b64(&bytes), b.lp.map_or(-1, |l| l as i64)))
            }) else { full = true; continue };
            format!("%7@{n}")
        };
        let ks = ks[&(r.smp, r.env, r.noise.is_some())];
        let adj = |x: i32| if x == 0 || x >= 63 { x } else { (x - ks).clamp(1, 62) };
        let mut amp = r.vol0.0;
        let (ar, dr, sr, sl) = match (r.manual.is_some(), r.env) {
            (true, _) => (63, 0, 0, 0),
            (_, Env::Adsr(a, d, s, sr)) => {
                let a = if a == 15 { 63 } else { adj(rise_rate(RATES[a as usize * 2 + 1])) };
                let sl = if s == 7 { 0 } else { (-20.0 * ((s as f64 + 1.0) / 8.0).log10() / 3.0103).round() as i32 };
                (a, adj(exp_rate(RATES[d as usize * 2 + 16])), adj(exp_rate(RATES[sr as usize])), sl)
            }
            (_, Env::Direct(l)) => { amp *= l as f32 * 16.0 / 2047.0; (63, 0, 0, 0) }
            (_, Env::Slope(m, rt)) => match m {
                6 | 7 => (adj(rise_rate(RATES[rt as usize])), 0, 0, 0),
                _ => continue, // (falling from silence: never heard)
            },
        };
        let rr = adj(match r.rel { Rel::Linear(p, step) => fall_rate(p, step), Rel::Exp(p) => exp_rate(p) });
        let tone = if r.noise.is_some() { 1 } else { instr[3..].parse().unwrap() };
        let send = if echo_on { format!("@v64,{}", if r.echo { send } else { 0 }) } else { String::new() };
        // Key-offs on whole rows (the drivers' gate times are fractions of a note), unless the note is shorter.
        let (start, e) = (on_grid(r.start), tick(r.end));
        let row_end = (e + 6) / 12 * 12;
        let stop = if row_end > start { row_end } else { start + (((e as f64 - start as f64) / 3.0).round() as u64 * 3).max(3) };
        let k0 = key_of(r, r.p0);
        let key = k0.round().clamp(0.0, 127.0);
        let level = r.noise.map_or(1.0, |hz| noise_level(hz));
        let mut n = Note::new(r.voice as u32, start, stop, key as u8, amp * level, if r.noise.is_some() { "%2".into() } else { "%7".into() });
        n.env = format!("@{tone},{ar},{dr},{sr},{rr},{sl}{send}");
        n.pan = r.vol0.1;
        // (Detunes of a few 64ths of a semitone are the drivers' pitch tables rounding: inaudible.)
        let det = |k: f64| { let k = ((k - key) * 64.0).round() as i32; if k.abs() <= 3 { 0 } else { k } };
        n.k0 = det(k0);
        for &(t, p) in &r.pitch { n.bends.push((tick(t), det(key_of(r, p)))) }
        // Volume: VOL × MVOL, times the envelope where it's followed step by step.
        let env_at = |t: u64| r.manual.as_ref().map_or(1.0, |m| { let i = m.partition_point(|x| x.0 <= t); if i == 0 { 0.0 } else { m[i - 1].1 } });
        let vol_at = |t: u64| { let i = r.vols.partition_point(|x| x.0 <= t); if i == 0 { r.vol0 } else { r.vols[i - 1].1 } };
        if let Some(m) = &r.manual {
            n.amp = r.vol0.0 * level * env_at(r.start).max(m.first().map_or(0.0, |x| x.1));
            let mut times: Vec<u64> = m.iter().map(|x| x.0).chain(r.vols.iter().map(|x| x.0)).collect();
            times.sort();
            for t in times { if t > r.start { n.vols.push((tick(t), vol_at(t).0 * level * env_at(t))) } }
        } else {
            for &(t, (a, _)) in &r.vols { n.vols.push((tick(t), a * level * (amp / r.vol0.0.max(1e-9)))) }
        }
        for &(t, (_, p)) in &r.vols { n.pans.push((tick(t), p)) }
        notes.push(n);
    }
    // A voice plays one note at a time: each ends by the time its next starts.
    notes.sort_by_key(|n| (n.ch, n.start));
    for i in 1..notes.len() {
        let (ch, next) = (notes[i].ch, notes[i].start);
        let n = &mut notes[i - 1];
        if n.ch == ch && n.end > next {
            n.end = next.max(n.start + 1);
            let end = n.end;
            n.bends.retain(|b| b.0 < end);
            n.vols.retain(|v| v.0 < end);
            n.pans.retain(|v| v.0 < end);
        }
    }
    // A voice switching samples all the time is a drum kit (or several parts sharing it): one track per sample,
    // so each keeps its tone. (Channels 8.. after the voices.)
    let mut names: Vec<String> = (0..8).map(|v| format!("voice {}", v + 1)).collect();
    let mut split: HashMap<(u32, String), u32> = HashMap::new();
    for v in 0..8u32 {
        let of: Vec<usize> = (0..notes.len()).filter(|&i| notes[i].ch == v).collect();
        let smp = |n: &Note| format!("{}{}", n.instr, n.env.split(',').next().unwrap_or(""));
        let tones: Vec<String> = of.iter().map(|&i| smp(&notes[i])).collect();
        let switches = tones.windows(2).filter(|w| w[0] != w[1]).count();
        if of.len() < 8 || switches * 10 < of.len() * 3 { continue }
        for &i in &of {
            let key = (v, smp(&notes[i]));
            let ch = *split.entry(key.clone()).or_insert_with(|| { names.push(format!("voice {}, sample {}", v + 1, key.1.rsplit('@').next().unwrap_or(""))); names.len() as u32 - 1 });
            notes[i].ch = ch;
        }
    }
    if full { warnings.push("more than 256 different samples: the rest are left out".into()) }
    if notes.is_empty() { return Err("the SPC plays no sound the converter can carry over".into()) }
    header.extend(defs.header());

    let title = [tags.game.as_str(), tags.title.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" - ");
    let title = if tags.artist.is_empty() { title } else { format!("{title} ({})", tags.artist) };
    // The loop point on the next whole row, so the song's bars line up with it (the end moves with it, keeping
    // the loop's length).
    let (mut end_t, mut lp_t) = (tick(end), lp.map(tick));
    if let Some(l) = &mut lp_t {
        let d = (12 - *l % 12) % 12;
        *l += d;
        end_t += d;
    }
    let mut score = Score {
        ppq: 48, tempos: vec![(0, 15.0 / row)], header, notes, end: end_t, lp: lp_t.filter(|&l| l < end_t), db: true,
        names, title, ..Score::default()
    };
    // A loop of silence is the song having ended: it plays once, to its last sound.
    if let Some(l) = score.lp { if !score.notes.iter().any(|n| n.end > l) { score.lp = None } }
    if score.lp.is_none() { if let Some(last) = score.notes.iter().map(|n| n.end).max() { score.end = score.end.min(last) } }
    Ok((score, warnings))
}
