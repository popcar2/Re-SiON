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

use std::collections::{BTreeMap, HashMap, HashSet};
use std::f64::consts::PI;

use crate::bytes;
use crate::nsf::mix;
use crate::pcm::pcmb;
use crate::score::{env_inc, hz_key, noise_key, noise_level, Defs, Note, Score};
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

/// ID666 tag: names, and how long the song plays (with its fade), if given.
#[derive(Default)]
struct Tags { title: String, game: String, artist: String, secs: Option<f64> }

fn tags(d: &[u8]) -> Tags {
    if d.get(0x23) != Some(&26) || d.len() < 0x100 { return Tags::default() }
    let text = |a: usize, n: usize| bytes::text(d, a, n);
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

/// A decoded sample: its BRR blocks, their frames (16-bit), the frame its loop starts on, and where it is in RAM
/// (start and loop addresses).
struct Brr { blocks: Vec<[u8; 9]>, data: Vec<i16>, lp: Option<usize>, at: (u16, u16) }

/// A sample found this many times with different contents is a wave the driver rewrites as it plays: its later
/// key-ons play the last version decoded, and so do the earlier ones (`Dsp::canon`), so the voice keeps one tone.
const VERSIONS: usize = 4;

/// A key-on: when it started (DSP samples), the voice, its sample (index into `Dsp::samples`), and all the DSP
/// registers as they were.
struct KeyOn { t: u64, voice: usize, smp: usize, regs: [u8; 128] }

impl KeyOn {
    /// What the key-on plays: voice, sample and pitch.
    fn sig(&self) -> u64 { let r = self.voice * 16; mix(mix(self.voice as u64, self.smp as u64), (self.regs[r + 2] as u64) << 8 | self.regs[r + 3] as u64) }
}

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
    /// The versions decoded of each sample (start and loop addresses).
    versions: HashMap<(u16, u16), Vec<usize>>,
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
            KON => { self.new_kon = v; self.regs[a] = v }
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
        let vs = self.versions.entry((start, lp)).or_default();
        if vs.len() >= VERSIONS { return *vs.last().unwrap() }
        let i = self.samples.len();
        vs.push(i);
        self.samples.push(Brr { data: sion::decode_brr(blocks.as_flattened()), blocks, lp: at.map(|i| i * 16), at: (start, lp) });
        self.cache.insert((start, lp, h), i);
        i
    }

    /// The sample a key-on of sample `i` is written with: the last version of a rewritten wave.
    fn canon(&self, i: usize) -> usize {
        let vs = &self.versions[&self.samples[i].at];
        if vs.len() >= VERSIONS { *vs.last().unwrap() } else { i }
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
            if even && (koff & bit != 0 || flg & 0x80 != 0) && self.v[i].mode != RELEASE {
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
                        env = fall(env);
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
                            5 => env = fall(env),
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
    /// For skipping a driver's wait for its timer: writes and reads of anything that changes with time (the
    /// DSP, a timer that has ticked) so far, and whether the last instruction read a timer that hadn't.
    writes: u64,
    volatile: u64,
    waited: bool,
    /// The timers read (without a tick) since the last such wait.
    polled: u8,
}

/// A RAM cell's share of the RAM's hash (splitmix64 of address and value, so different changes can't cancel).
fn cell(a: u16, v: u8) -> u64 {
    let mut z = ((a as u64) << 8 | v as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Apu {
    /// The cycle at which one of these running timers' output next counts up (each counts its own clock up to
    /// its target).
    fn next_tick(&self, which: u8) -> Option<u64> {
        self.timers.iter().enumerate().filter(|&(k, t)| t.on && which >> k & 1 != 0).map(|(_, t)| {
            let target = if t.target == 0 { 256 } else { t.target as u64 };
            (t.at / t.period + target - t.div) * t.period
        }).min()
    }

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
            0xf3 => { self.volatile += 1; self.dsp.sync(cycle, &self.ram); self.dsp.read(self.addr) }
            0xf4..=0xf7 => self.ports[a as usize - 0xf4],
            0xfd..=0xff => {
                let t = &mut self.timers[a as usize - 0xfd];
                t.sync(cycle);
                let v = std::mem::take(&mut t.out);
                if v != 0 { (self.tick, self.volatile) = (true, self.volatile + 1) } else { (self.waited, self.polled) = (true, self.polled | 1 << (a - 0xfd)) }
                v
            }
            0xf0 | 0xf1 | 0xfa..=0xfc => 0,
            0xffc0..=0xffff if self.ipl => self.rom[a as usize - 0xffc0],
            _ => self.ram[a as usize],
        }
    }

    fn write(&mut self, a: u16, v: u8, cycle: u64) {
        // (Only writes that change something count: a driver's wait loop may store the same values every round.
        // The ports' writes go to the other side.)
        let changes = match a {
            0xf1 | 0xf3 => true,
            0xf2 => v != self.addr,
            0xfa..=0xfc => v != self.timers[a as usize - 0xfa].target,
            0xf0..=0xff => false,
            _ => v != self.ram[a as usize],
        };
        self.writes += changes as u64;
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
    let mut dsp = Dsp { regs, v: Default::default(), new_kon: 0, counter: 0, t: 0, writes: vec![], keyons: vec![], offs: vec![], envs: Default::default(), samples: vec![], cache: HashMap::new(), versions: HashMap::new() };
    // (The DSP's own state isn't in the file: the voices start silent, and those whose KON bit is set are keyed
    // on, as SPC players do. A snapshot taken as a song starts holds its first key-ons there.)
    for v in &mut dsp.v { v.mode = RELEASE }
    dsp.new_kon = regs[KON];
    let mut apu = Apu { ipl: ram[0xf1] & 0x80 != 0, ports: [ram[0xf4], ram[0xf5], ram[0xf6], ram[0xf7]], addr: ram[0xf2], ram, rom, timers, dsp, hash, sig: 0, tick: false, writes: 0, volatile: 0, waited: false, polled: 0 };
    let mut cpu = Cpu { pc: u16::from_le_bytes([d[0x25], d[0x26]]), a: d[0x27], x: d[0x28], y: d[0x29], psw: d[0x2a], sp: d[0x2b], ..Cpu::default() };

    // Run until the driver's state repeats (the song loops), or the time runs out. (Or until the driver's writes
    // or key-ons have been repeating for a while: the same loop found at two checks in a row.)
    let max = (tags.secs.map_or(MAX_SECS, |s| s.max(MAX_SECS)) * CLOCK) as u64;
    let (mut seen, mut ticks, mut sigs) = (HashMap::new(), vec![], vec![]);
    let mut exact = None;
    let check = (CHECK_SECS * CLOCK) as u64;
    let (mut next_check, mut found) = (2 * check, None);
    // A driver waiting for its timer goes round the same loop, reading the timer and nothing else that changes,
    // until it ticks: those rounds are skipped (all but the last), as whole rounds.
    let mut wait: Option<((u16, u8, u8, u8, u8, u8), u64, u64, u64)> = None;
    while cpu.cycles < max && !cpu.halted {
        cpu.step(&mut apu);
        if std::mem::take(&mut apu.waited) {
            let at = (cpu.pc, cpu.a, cpu.x, cpu.y, cpu.sp, cpu.psw);
            if let Some((_, c0, ..)) = wait.filter(|&(w, c0, writes, volatile)| w == at && writes == apu.writes && volatile == apu.volatile && cpu.cycles > c0) {
                let round = cpu.cycles - c0;
                if let Some(n) = apu.next_tick(apu.polled).map(|t| t.saturating_sub(cpu.cycles) / round).filter(|&n| n >= 2) { cpu.cycles += (n - 1) * round }
            }
            wait = Some((at, cpu.cycles, apu.writes, apu.volatile));
            apu.polled = 0;
        }
        if apu.tick {
            apu.tick = false;
            let k = ticks.len();
            ticks.push(cpu.cycles);
            sigs.push(std::mem::take(&mut apu.sig));
            if let Some(j) = seen.insert(apu.state(&cpu), k) { exact = Some((j, k)); break }
        }
        if cpu.cycles >= next_check {
            next_check += check;
            apu.dsp.sync(cpu.cycles, &apu.ram);
            let f = repeats(&sigs, &ticks, &apu.dsp.keyons, apu.dsp.t);
            // (A section can repeat twice and go on, and an ambient piece can repeat a phrase for two minutes
            // before it changes: only a loop that has held three times round, and for `HOLD_SECS`, is taken.)
            let held = f.is_some_and(|(e, l)| apu.dsp.t - l >= (3 * (e - l)).max((HOLD_SECS * RATE) as u64));
            if held && f == found { break }
            found = f;
        }
    }
    if cpu.halted { warnings.push("the driver stopped the CPU (SLEEP or STOP)".into()) }
    apu.dsp.sync(cpu.cycles, &apu.ram);
    let now = apu.dsp.t;
    let at = |cycles: u64| cycles / 32;
    let (mut end, mut lp) = match exact {
        Some((j, k)) => (at(ticks[k]), Some(at(ticks[j]))),
        None => match repeats(&sigs, &ticks, &apu.dsp.keyons, now) {
            Some((e, l)) => (e, Some(l)),
            // (Or the tag's play time; or all of it.)
            None => match tags.secs {
                Some(s) => ((s * RATE) as u64, None),
                None => { warnings.push(format!("no loop found in the first {} minutes: converted that much", MAX_SECS / 60.0)); (now, None) }
            },
        },
    };
    // A loop found by the driver's state (or writes) can be several times round the music (its tempo counter or
    // a vibrato coming back into phase only then): if the key-ons repeat sooner within it, from where it starts
    // (or before), loop there. (Not a figure repeated just before its end: the whole loop must repeat.)
    if let Some(l) = lp {
        let kons: Vec<&KeyOn> = apu.dsp.keyons.iter().filter(|k| k.t < end).collect();
        let ksig: Vec<u64> = kons.iter().map(|k| k.sig()).collect();
        let kps = kons.len() as f64 / (end as f64 / RATE).max(1.0);
        if let Some((j, p)) = crate::nsf::repeat(&ksig, (kps as usize).max(1))
            .filter(|&(j, p)| j + p < kons.len() && kons[j].t <= l + RATE as u64 / 8 && kons[j + p].t > l && kons[j + p].t - kons[j].t >= 2 * RATE as u64 && kons[j + p].t - kons[j].t < end - l)
        {
            (end, lp) = (kons[j + p].t, Some(kons[j].t));
        }
    }
    // A loop found partway into a note: if the key-ons just before it come round the same way at the end, it
    // starts with the key-on before it (end and loop moved back together).
    if let Some(l) = lp {
        let kons = &apu.dsp.keyons;
        let same = |a: &KeyOn, b: &KeyOn| { let r = a.voice * 16; a.voice == b.voice && a.smp == b.smp && a.regs[r + 2..r + 4] == b.regs[r + 2..r + 4] };
        if let Some(f) = kons.iter().rev().find(|k| k.t <= l).map(|k| k.t).filter(|&f| l - f > 0 && l - f < RATE as u64 / 4) {
            let span = end - l;
            let before: Vec<&KeyOn> = kons.iter().filter(|k| k.t >= f && k.t < l).collect();
            let again: Vec<&KeyOn> = kons.iter().filter(|k| k.t >= f && k.t < end && k.t + 64 >= f + span && k.t < l + span).collect();
            let ok = before.len() == again.len() && before.iter().zip(&again).all(|(a, b)| same(a, b) && (b.t as i64 - (a.t + span) as i64).abs() <= 64);
            if ok { (end, lp) = (end - (l - f), Some(f)) }
        }
    }
    Ok(Played { dsp: apu.dsp, regs0: regs, end: end.min(now), lp, tags })
}

/// How often (seconds of the song) to look for the loop in the driver's writes and key-ons while it plays.
const CHECK_SECS: f64 = 20.0;
/// How long a loop found that way must have been playing (seconds) before the run stops there.
const HOLD_SECS: f64 = 150.0;

/// Where the song loops, from the driver's writes repeating (ticks with writes, so a driver's tempo counter can
/// move them by a tick), with a loop of at least a second; failing that (drivers whose vibrato runs on through
/// the loop), from its key-ons repeating: voice, sample and pitch. (End, loop start) in DSP samples, as played up
/// to `now`.
fn repeats(sigs: &[u64], ticks: &[u64], kons: &[KeyOn], now: u64) -> Option<(u64, u64)> {
    let secs = (now as f64 / RATE).max(1.0);
    let busy: Vec<usize> = (0..sigs.len()).filter(|&k| sigs[k] != 0).collect();
    let sig: Vec<u64> = busy.iter().map(|&k| sigs[k]).collect();
    if let Some((j, p)) = crate::nsf::repeat(&sig, (busy.len() as f64 / secs) as usize).filter(|&(j, p)| j + p < busy.len()) {
        return Some((ticks[busy[j + p]] / 32, ticks[busy[j]] / 32));
    }
    let ksig: Vec<u64> = kons.iter().map(|k| k.sig()).collect();
    let (j, p) = crate::nsf::repeat(&ksig, ((kons.len() as f64 / secs) as usize).max(1)).filter(|&(j, p)| j + p < kons.len())?;
    Some((kons[j + p].t, kons[j].t))
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
    /// Its fixed GAIN level (full scale 1), or 1 for one that moves.
    fn gain(self) -> f32 { if let Env::Direct(l) = self { l as f32 * 16.0 / 2047.0 } else { 1.0 } }
}

/// The SiON rate (1..63) whose step is closest to `a` units per frame; 0 for none.
fn sion_rate(a: f64) -> i32 {
    if a <= 0.0 { return 0 }
    if a > env_inc(60) * 1.5 { return 63 }
    (1..=60).min_by(|&x, &y| (env_inc(x) / a).ln().abs().total_cmp(&(env_inc(y) / a).ln().abs())).unwrap()
}

/// SiON steps (`env_inc`'s units; 0: none, infinite: at once) for an exponential fall of one DSP step (-1/256 of
/// the level) every `p` samples, a linear rise of 1/64 of full scale every `p` (matched at half level: SiON's
/// attack is a curve), and a linear fall of `step` / 2048 every `p` (matched 20 dB down).
fn exp_inc(p: u32) -> f64 { if p == 0 { 0.0 } else { 0.2620 / p as f64 } }
fn rise_inc(p: u32) -> f64 { if p == 0 { 0.0 } else if p == 1 { f64::INFINITY } else { 37.8 / (44.1 * p as f64) } }
fn fall_inc(p: u32, step: f64) -> f64 { if p == 0 { 0.0 } else { 213.0 / (1842.0 / step * p as f64 * 44100.0 / RATE) } }

/// The DSP's exponential fall, one step: -1, then -1/256 of the level. (So it falls faster, in decibels, the lower
/// it is: below 256 it is linear.)
fn fall(e: i32) -> i32 { let e = e - 1; e - (e >> 8) }

/// An envelope level (0..2047) in decibels.
fn env_db(e: i32) -> f64 { 20.0 * (e.max(1) as f64 / 2047.0).log10() }

/// The SiON step (3/32 dB per 44.1 kHz frame) that falls `db` decibels in `n` DSP samples.
fn db_inc(db: f64, n: f64) -> f64 { if n <= 0.0 { f64::INFINITY } else { db.abs() / 0.09375 / (n * 44100.0 / RATE) } }

/// SiON steps for an ADSR envelope's decay and sustain (rates `d`, `sr`, level `s`) and SiON's sustain level (3 dB
/// steps), fitted to the DSP's curve over a note of `len` samples. The DSP's falls are faster, in decibels, the
/// lower they go, and SiON's are straight lines: the decay goes to SiON's sustain level in the time the DSP's takes
/// to reach its own, and the sustain is the least-squares line on from there until the note ends (or the DSP's is
/// 40 dB down). Without a decay stage (`s` = 7) SiON's decay is free: the sustain is fitted as two lines, with the
/// knee where they come closest (weighted by the level: what is loud counts most).
fn adsr_fit(d: u8, s: u8, sr: u8, len: u64) -> (f64, f64, i32) {
    let (pd, ps) = (RATES[d as usize * 2 + 16] as u64, RATES[sr as usize] as u64);
    let sl = if s == 7 { 0 } else { (-20.0 * ((s as f64 + 1.0) / 8.0).log10() / 3.0103).round() as i32 };
    let (mut e, mut t) = (0x7ff, 0u64);
    if s < 7 { while e >> 8 != s as i32 && e > 0 { e = fall(e); t += pd } }
    let di = if s == 7 { exp_inc(pd as u32) } else { db_inc(sl as f64 * 3.0103, t as f64) };
    // The DSP's sustain, from where the decay leaves it: (sample, dB), to the note's end, down to -40 dB.
    let mut curve = vec![(t as f64, env_db(e))];
    let mut at = t;
    while ps > 0 && at < len && e > 0 && env_db(e) > -40.0 { e = fall(e); at += ps; curve.push((at as f64, env_db(e))) }
    if curve.len() < 3 { return (di, exp_inc(ps as u32), sl) }
    let per = 0.09375 * 44100.0 / RATE; // (dB per DSP sample, per SiON step)
    // The sustain on from a knee `sl` 3 dB steps down at sample `at`, and how far the whole curve is from the DSP's.
    let fit = |sl: i32, at: f64| -> (f64, f64) {
        let knee = -(sl as f64) * 3.0103;
        let w = |db: f64| 10f64.powf(db / 20.0);
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for &(t, db) in curve.iter().filter(|c| c.0 >= at) { sxy += (knee - db) * (t - at); sxx += (t - at).powi(2) }
        let b = if sxx > 0.0 { (sxy / sxx).max(0.0) } else { 0.0 };
        let first = curve[0].0;
        let err = curve.iter().map(|&(t, db)| {
            let sion = if t < at { knee * (t - first) / (at - first) } else { knee - b * (t - at) };
            (sion - db).powi(2) * w(db)
        }).sum();
        (b, err)
    };
    if s < 7 { return (di, fit(sl, t as f64).0 / per, sl) }
    let (b, mut err) = fit(0, 0.0);
    let mut best = (di, b / per, 0);
    for k in 1..=12 {
        let knee = -(k as f64) * 3.0103;
        for &(at, _) in curve.iter().step_by((curve.len() / 64).max(1)).filter(|c| c.0 > 0.0 && (c.1 - knee).abs() < 6.0) {
            let (b, x) = fit(k, at);
            if x < err { (best, err) = ((-knee / at / per, b / per, k), x) }
        }
    }
    best
}

/// The SiON step for an exponential release (a step every `p` samples), fitted over its first 40 dB.
fn release_fit(p: u32) -> f64 {
    if p == 0 { return 0.0 }
    let (mut e, mut k) = (0x7ff, 0u64);
    while env_db(e) > -40.0 { e = fall(e); k += 1 }
    db_inc(40.0, (k * p as u64) as f64)
}

/// A rise this long (DSP samples) or longer is written as the DSP makes it (`env_table`): SiON's attack is a
/// curve, late and then sudden, against the DSP's straight line up, which is plain to hear on a slow one.
const SLOW_RISE: usize = (0.15 * RATE) as usize;

/// The DSP's envelope (0..2047) from a key-on, every sample, for up to `len` samples (or until it is 40 dB down,
/// or holds still), for an envelope that rises (an ADSR attack, or a GAIN increase; empty for others); and how
/// long the rise (and an ADSR's decay to its sustain level) takes.
fn env_curve(env: Env, len: usize) -> (Vec<i32>, usize) {
    let (mut e, mut hidden, mut mode, mut counter, mut out, mut rise) = (0i32, 0i32, ATTACK, 0u32, vec![], None);
    while out.len() < len.max(1) || rise.is_none() {
        counter = if counter == 0 { 0x77ff } else { counter - 1 };
        let mut x = e;
        let rate = match env {
            Env::Adsr(a, d, s, sr) => {
                let rate = match mode {
                    ATTACK => { x += if a < 15 { 0x20 } else { 0x400 }; a as usize * 2 + 1 }
                    DECAY => { x = fall(x); d as usize * 2 + 16 }
                    _ => { x = fall(x); sr as usize }
                };
                if x >> 8 == s as i32 && mode == DECAY { mode = SUSTAIN }
                rate
            }
            Env::Slope(m @ (6 | 7), r) => { x += if m == 7 && hidden as u32 >= 0x600 { 0x8 } else { 0x20 }; r as usize }
            _ => return (vec![], 0),
        };
        hidden = x;
        if x as u32 > 0x7ff { x = if x < 0 { 0 } else { 0x7ff }; if mode == ATTACK { mode = DECAY } }
        if RATES[rate] != 0 && (counter + OFFSETS[rate]) % RATES[rate] == 0 { e = x }
        out.push(e);
        let risen = mode == SUSTAIN || (matches!(env, Env::Slope(..)) && e == 0x7ff);
        if risen && rise.is_none() { rise = Some(out.len()) }
        // (Holding still, or nearly gone, or a rise that never ends.)
        if (risen && (RATES[rate] == 0 || e == 0x7ff && matches!(env, Env::Slope(..)) || e < 21)) || RATES[rate] == 0 || out.len() >= (60.0 * RATE) as usize { break }
    }
    let rise = rise.unwrap_or(out.len());
    (out, rise)
}

/// The DSP's envelope (`env_curve`) as the expression (`na`: x, 0..128, a value a 1/60 s frame, the middle of
/// it), for a note whose SiON envelope holds still at full.
fn env_table(curve: &[i32]) -> Vec<i32> {
    let n = (curve.len() as f64 / (RATE / 60.0)).ceil().max(1.0) as usize;
    let at = |t: f64| curve[((t * RATE) as usize).min(curve.len() - 1)] as f64 / 2047.0;
    let mut v: Vec<i32> = (0..n).map(|k| (128.0 * at((k as f64 + 0.5) / 60.0)).round() as i32).collect();
    v.push((128.0 * at(curve.len() as f64 / RATE)).round() as i32);
    trim_held(&mut v);
    v
}

/// A table's last values held: written once.
fn trim_held(v: &mut Vec<i32>) { while v.len() > 1 && v[v.len() - 2] == v[v.len() - 1] { v.pop(); } }

/// A table's values as `#TABLE` text: runs that lie on a line (to within one) as `(from,to)count`.
fn table_text(v: &[i32]) -> String {
    let (mut out, mut i) = (vec![], 0);
    while i < v.len() {
        // (The longest run from `i` that `(v[i],v[j])` with j - i values gives back.)
        let fits = |j: usize| (0..j - i).all(|k| { let x = v[i] as f64 + (v[j] - v[i]) as f64 * k as f64 / (j - i) as f64; ((x + 0.5).trunc() as i32 - v[i + k]).abs() <= 1 });
        match (i + 3..v.len()).take_while(|&j| fits(j)).last() {
            Some(j) => { out.push(format!("({},{}){}", v[i], v[j], j - i)); i = j }
            None => { out.push(v[i].to_string()); i += 1 }
        }
    }
    out.join(",")
}

/// SiON rates for envelope stages (steps as above, each with how much its accuracy counts), and the envelope clock
/// (`@clock`, percent) that brings them closest: SiON's rates are 19% apart, so a long decay could otherwise end
/// up a decibel or two off. (The clock only where some rate is more than 3% closer for it.)
fn fit_rates(stages: &[(f64, f64)]) -> (Vec<i32>, u32) {
    let pick = |c: f64| -> Vec<i32> { stages.iter().map(|&(a, _)| if a.is_infinite() { 63 } else { sion_rate(a / c) }).collect() };
    let cost = |c: f64| -> f64 {
        stages.iter().zip(pick(c)).filter(|&(&(a, _), r)| a.is_finite() && (1..=60).contains(&r)).map(|(&(a, w), r)| w * (env_inc(r) * c / a).ln().powi(2)).sum()
    };
    let (best, least) = (85..=118).map(|c| (c, cost(c as f64 / 100.0))).min_by(|x, y| x.1.total_cmp(&y.1)).unwrap();
    let c = if cost(1.0) - least > 0.03f64.powi(2) { best } else { 100 };
    (pick(c as f64 / 100.0), c)
}

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
    /// Its pitch is modulated by the previous voice (PMON).
    pmon: bool,
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
    // (Candidates run from long rows to short: the first one good enough is on its peak's slope, so climb to
    // the top of that peak.)
    let mut i = cands.iter().position(|c| c.1 >= best * 0.9).unwrap();
    while i + 1 < cands.len() && cands[i + 1].1 >= cands[i].1 { i += 1 }
    let mut per = cands[i].0;
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
    // (Moved back by whole rows to the first key-on, so every note is at or after it, give or take the timing
    // jitter the key-ons snap by: a first key-on just before a row is on it.)
    (per, ph - per * ((ph - t[0]) / per - 1.0 / 6.0).ceil())
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

/// The DSP's Gaussian interpolation, as far as it differs from SiON's linear one, for the wavetables (PMON):
/// a gentle low-pass in the sample's own frames (`GAUSS` either side). (Samples themselves are played with the
/// real thing: SiON has it for BRR.)
const GAUSS: f64 = 0.15;

/// How long an `@lfo` frame lasts (seconds) under envelope clock `clock` (`@clock`, percent): SiON's LFO counts
/// 255 steps a 1/60 s frame (256 a round), at the envelope's clock.
fn lfo_frame(clock: u32) -> f64 { 256.0 / 255.0 / 60.0 / (clock.max(1) as f64 / 100.0) }

/// A 256-step LFO table for a round of `secs`: how many rounds it holds (1 to 8: the fewest that bring the rate
/// within 0.5% with a whole number of LFO frames, else the closest) and its LFO frames.
fn lfo_fit(secs: f64, clock: u32) -> (usize, f64) {
    crate::score::lfo_rounds(secs / lfo_frame(clock))
}

/// Below this rate (Hz) a pitch-modulating voice is a vibrato, played as SiON's pitch LFO (`@lfo`, `mp`).
const LFO_HZ: f64 = 20.0;

/// SiON's pitch LFO for a modulator `m` playing sample `b` at up to `env` (envelope, full scale 1): `@lfo` with the
/// loop's shape (`#TABLE`, 256 steps: several cycles of it when that brings the rate, a whole number of LFO
/// frames (`lfo_frame`, under the note's envelope clock), within 0.5%), and its depth (`mp`, 1/64 semitones) at that envelope. The DSP's modulation is in
/// frequency, × (1 + level), SiON's in pitch: the table is the level's log (1 + level) at that envelope, so a deep
/// one rises less than it falls, as on the DSP; lower envelopes scale it down.
fn vibrato(m: &Raw, b: &Brr, env: f64, clock: u32, defs: &mut Defs) -> Option<(String, f64)> {
    let lp = loop_of(b)?;
    let hz = loop_hz(m.p0, lp.len());
    let (k, nf) = lfo_fit(1.0 / hz, clock);
    if env <= 0.0 || nf > 32767.0 { return None }
    let at = |p: f64| { let p = p * lp.len() as f64; let (i, f) = (p as usize % lp.len(), p.fract()); lp[i] as f64 * (1.0 - f) + lp[(i + 1) % lp.len()] as f64 * f };
    // (The pitch can't go below zero: down to 6 octaves.)
    let semis: Vec<f64> = (0..256).map(|i| 12.0 * (1.0 + env * at((i * k) as f64 / 256.0 % 1.0)).max(1.0 / 64.0).log2()).collect();
    let top = semis.iter().fold(0f64, |p, v| p.max(v.abs()));
    if top <= 0.0 { return None }
    Some((lfo_cmd(nf, semis.iter().map(|v| 128.0 + 127.0 * v / top), defs), top * 64.0))
}

/// `@lfo` of `nf` frames a round, its shape a table of the values (0-255, rounded).
fn lfo_cmd(nf: f64, vals: impl Iterator<Item = f64>, defs: &mut Defs) -> String {
    let body: Vec<String> = vals.map(|v| (v.round().clamp(0.0, 255.0) as i32).to_string()).collect();
    format!("@lfo{nf},{}", defs.table(&body.join(",")))
}

/// A driver's vibrato: where a note's pitch writes go round the same values, write after write (at least twice
/// round, a set time each round; the longest such run): SiON's pitch LFO there. (DSP samples it starts and
/// stops at, samples a round, and the pitch at any time in it; the writes after it, such as the next note's pitch
/// set just before its key-on, stay as they are.)
struct DriverVib { at: u64, until: u64, period: f64, seq: Vec<(u64, u32)> }

impl DriverVib {
    fn find(start: u64, end: u64, p0: u32, pitch: &[(u64, u32)]) -> Option<DriverVib> {
        // (A pitch written a byte at a time passes through a value it never meant: the one a moment later counts.)
        let mut seq = vec![(start, p0)];
        for (k, &(t, p)) in pitch.iter().enumerate() {
            if pitch.get(k + 1).is_some_and(|n| n.0 - t < 16) { continue }
            if seq.last().is_some_and(|l| l.1 == p) { continue }
            seq.push((t, p));
        }
        let n = seq.len();
        for m in 2..=48.min(n / 2) {
            // The longest run that repeats every `m` writes: writes j..e (e - m of them match the one m on).
            let (mut best, mut i) = ((0, 0), 0);
            while i + m < n {
                let j = i;
                while i + m < n && seq[i].1 == seq[i + m].1 { i += 1 }
                if i - j > best.1 - best.0 { best = (j, i) }
                i += 1;
            }
            let (mut j, e) = (best.0, best.1 + m);
            if e - j < 2 * m + 1 { continue }
            // (Each round's time, against the usual one: the vibrato starts with the first write on the driver's
            // beat, not at the key-on.)
            let all: Vec<u64> = (j..e - m).map(|i| seq[i + m].0 - seq[i].0).collect();
            let mut sorted = all.clone();
            sorted.sort_unstable();
            let usual = sorted[sorted.len() / 2];
            let j0 = j;
            let tol = (usual / 50).max(64);
            while j + 2 * m < e && seq[j + m].0 - seq[j].0 > usual + tol || seq[j + m].0 - seq[j].0 + tol < usual { j += 1 }
            if e - j < 2 * m + 1 { continue }
            let until = seq.get(e).map_or(end, |x| x.0);
            let rounds = &all[j - j0..];
            let (lo, hi) = (*rounds.iter().min()?, *rounds.iter().max()?);
            let period = rounds.iter().sum::<u64>() as f64 / rounds.len() as f64;
            let (pmin, pmax) = (seq[j..j + m].iter().map(|x| x.1).min()?, seq[j..j + m].iter().map(|x| x.1).max()?);
            if pmin == pmax || (hi - lo) as f64 > (0.02 * period).max(64.0) || period < RATE * 0.03 || period > RATE * 2.0 { continue }
            seq.truncate(e);
            return Some(DriverVib { at: seq[j].0, until, period, seq });
        }
        None
    }

    /// The pitch `t` DSP samples into the vibrato (as its first round goes).
    fn at(&self, t: f64) -> u32 {
        let t = self.at as f64 + t.rem_euclid(self.period);
        let i = self.seq.partition_point(|x| (x.0 as f64) <= t);
        self.seq[i.max(1) - 1].1
    }
}

/// A sample's loop (frames, full scale 1), if it has one.
fn loop_of(b: &Brr) -> Option<Vec<f32>> {
    let n = loop_len(b)?;
    Some(b.data[b.data.len() - n..].iter().map(|&v| v as f32 / 32768.0).collect())
}

/// How many frames a sample's loop has, if it has one.
fn loop_len(b: &Brr) -> Option<usize> { b.lp.filter(|&l| b.data.len() >= l + 2).map(|l| b.data.len() - l) }

/// How many times a second a loop of `frames` goes round at pitch `p` (P register).
fn loop_hz(p: u32, frames: usize) -> f64 { RATE * p.max(1) as f64 / 4096.0 / frames as f64 }

/// A sample's loop as a `#WAV` (see `wavetable`): (wave, level, loop frames).
fn loop_wave(b: &Brr, integrate: bool, defs: &mut Defs) -> Option<(usize, f32, usize)> {
    let lp = loop_of(b)?;
    let (t, peak) = wavetable(&lp, integrate)?;
    Some((defs.wave(&t), peak, lp.len()))
}

/// A key's detune from the note's `key` in 1/64 semitones. (Detunes of a few 64ths are the drivers' pitch tables
/// rounding: inaudible.)
fn detune(k: f64, key: f64) -> i32 { rounded_detune((k - key) * 64.0) }

/// A detune in 1/64 semitones, rounded (a few 64ths either way: none).
fn rounded_detune(d: f64) -> i32 { let d = d.round() as i32; if d.abs() <= 3 { 0 } else { d } }

/// The value at `t` of a sorted list of changes (`before` the first).
fn step_at<T: Copy>(xs: &[(u64, T)], t: u64, before: T) -> T { let i = xs.partition_point(|x| x.0 <= t); if i == 0 { before } else { xs[i - 1].1 } }

/// A loop of up to 1024 frames as a wavetable (`#WAV`: 1024 values, -128..127; SiON reads it without
/// interpolating): its harmonics as
/// the DSP plays them (its Gaussian interpolation is a gentle low-pass in the sample's own frames, like `soften`
/// on top of linear interpolation), and, with `integrate`, their integral (in frames: what a pitch modulated by
/// the loop adds up to). Also the level of full scale, in the loop's units (frames, integrated).
fn wavetable(lp: &[f32], integrate: bool) -> Option<(Vec<i32>, f32)> {
    // (A longer loop has more harmonics than 1024 values hold: a recorded sound, not one wave.)
    let n = lp.len();
    if n > 1024 { return None }
    let w = |k: usize| 2.0 * PI * k as f64 / n as f64;
    // (Linear interpolation's response, and the softening's.)
    let h = |o: f64| { let s = if o == 0.0 { 1.0 } else { (o / 2.0).sin() / (o / 2.0) }; s * s * ((1.0 - 2.0 * GAUSS) + 2.0 * GAUSS * o.cos()) };
    let harmonics: Vec<(f64, f64)> = (1..=(n - 1) / 2).map(|k| {
        let (re, im) = lp.iter().enumerate().fold((0.0, 0.0), |a, (i, &v)| (a.0 + v as f64 * (w(k) * i as f64).cos(), a.1 - v as f64 * (w(k) * i as f64).sin()));
        let g = 2.0 / n as f64 * h(w(k)) / if integrate { w(k) } else { 1.0 };
        // (Integrating turns cos into sin: a quarter cycle back.)
        if integrate { (im * g, -re * g) } else { (re * g, im * g) }
    }).collect();
    let table: Vec<f64> = (0..1024).map(|j| {
        let ph = j as f64 / 1024.0 * n as f64;
        harmonics.iter().enumerate().map(|(i, &(a, b))| a * (w(i + 1) * ph).cos() - b * (w(i + 1) * ph).sin()).sum()
    }).collect();
    let peak = table.iter().fold(0.0, |m: f64, v| m.max(v.abs()));
    (peak > 1e-6).then(|| (table.iter().map(|v| (v / peak * 127.0).round().clamp(-128.0, 127.0) as i32).collect(), peak as f32))
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
    let at_reg = |from: &[u8; 128], t0: u64, t1: u64| dsp.writes[dsp.writes.partition_point(|w| w.0 < t0)..dsp.writes.partition_point(|w| w.0 < t1)].iter().scan(*from, |r, &(t, a, v)| { r[a as usize] = v; Some((t, a, *r)) });

    // Notes: each key-on, until the voice's next one (or it falls silent).
    let mut raws: Vec<Raw> = vec![];
    let mut changed = 0usize;
    for (i, k) in dsp.keyons.iter().enumerate() {
        if k.t >= end { break }
        let v = k.voice;
        let next = dsp.keyons[i + 1..].iter().find(|n| n.voice == v).map_or(end, |n| n.t).min(end);
        // The last moment it is heard.
        let envs = &dsp.envs[v];
        let (a, b) = ((k.t / 32) as usize, ((next / 32) as usize).min(envs.len()));
        let Some(last) = (a..b).rev().find(|&j| envs[j] > 0) else { continue };
        let heard = ((last as u64 + 1) * 32).min(next);
        // (Key-offs are in time order.)
        let off = dsp.offs[dsp.offs.partition_point(|o| o.0 <= k.t)..].iter().take_while(|o| o.0 < heard).find(|o| o.1 == v).map(|o| o.0);
        let bit = 1u8 << v;
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
        raws.push(Raw { voice: v, start: k.t, end: stop.max(k.t + 1), smp: dsp.canon(k.smp), noise, env, rel, manual, p0, pitch, vol0, vols, echo, pmon: k.regs[PMON] & bit != 0 && v > 0 });
    }
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
    let at = |t: u64| ((secs(t) - phase) / (row / 12.0)).max(0.0);
    let tick = |t: u64| at(t).round() as u64;
    // (A tick position on the first of these (unit, tolerance) grids it is close to, else the nearest tick.)
    let snap = |x: f64, grids: &[(f64, f64)]| {
        grids.iter().find_map(|&(unit, tol)| { let s = (x / unit).round() * unit; ((x - s).abs() <= tol).then_some(s as u64) }).unwrap_or(x.round() as u64)
    };
    // Key-ons on the nearest whole row, half row, triplet or 1/64 note close by (drivers' timing jitters by a
    // timer tick or two).
    let on_grid = |t: u64| snap(at(t), &[(12.0, 2.0), (6.0, 1.5), (4.0, 1.0), (3.0, 1.0)]);

    // Echo: the settings that hold longest, as a delay on send 1.
    let mut held: HashMap<[u8; 15], u64> = HashMap::new();
    let echo_regs = |r: &[u8; 128]| -> [u8; 15] {
        let mut k = [r[EDL] & 15, r[EFB], r[EVOLL], r[EVOLR], r[MVOLL], r[MVOLR], r[FLG] & 0x20, 0, 0, 0, 0, 0, 0, 0, 0];
        for i in 0..8 { k[7 + i] = r[i * 16 + 15] }
        k
    };
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

    // Tones: the sample and its envelope, with its falls fitted over how long its notes usually last (the median).
    let mut defs = Defs::default();
    let mut pcm: HashMap<usize, Option<usize>> = HashMap::new();
    let tone_of = |r: &Raw| (r.smp, r.env, r.noise.is_some());
    let mut lens: HashMap<(usize, Env, bool), Vec<u64>> = HashMap::new();
    for r in &raws { lens.entry(tone_of(r)).or_default().push(r.end - r.start) }
    // (Per tone: its median length, and its longest.)
    let usual: HashMap<(usize, Env, bool), (u64, u64)> = lens.into_iter().map(|(k, mut l)| { l.sort(); (k, (l[l.len() / 2], l[l.len() - 1])) }).collect();
    // (Envelope fits, per (decay, sustain level, sustain rate, length) and release: the same for every note of a tone.)
    let mut adsr_fits: HashMap<(u8, u8, u8, u64), (f64, f64, i32)> = HashMap::new();
    let mut release_fits: HashMap<u32, f64> = HashMap::new();
    // (Per envelope: whether it rises slowly; per envelope and length, its rise as `na` with its table.)
    let mut slow_rise: HashMap<Env, bool> = HashMap::new();
    let mut rises: HashMap<(Env, usize), String> = HashMap::new();
    let mut notes = vec![];
    let mut full = false;
    // Each note's next key-on on the same voice.
    let mut next = vec![None; raws.len()];
    let mut last: [Option<usize>; 8] = [None; 8];
    for (i, r) in raws.iter().enumerate() { if let Some(p) = last[r.voice] { next[p] = Some(i) } last[r.voice] = Some(i) }
    // Each voice's usual gap before the next key-on (ticks), if it has one: gaps a tick either side of it are the
    // driver's timer against the rows, and come out as that.
    let gap: Vec<Option<u64>> = (0..8).map(|v| {
        let mut n: HashMap<u64, usize> = HashMap::new();
        for (i, r) in raws.iter().enumerate().filter(|x| x.1.voice == v) {
            if let Some(g) = next[i].map(|k| at(raws[k].start) - at(r.end)).filter(|&g| g > 0.5 && g < 12.0) { *n.entry(g.round() as u64).or_default() += 1 }
        }
        let total: usize = n.values().sum();
        n.into_iter().max_by_key(|&(g, c)| (c, std::cmp::Reverse(g))).filter(|&(_, c)| c >= 4 && c * 3 >= total).map(|x| x.0)
    }).collect();
    // Pitch-modulated (PMON) voices play their sample's loop as a wavetable, one table a loop: SiON modulates an
    // operator's phase from a pipe, not PCM's (see below). Per sample: (wave, level, loop frames).
    let mut carrier_wave: HashMap<usize, Option<(usize, f32, usize)>> = HashMap::new();
    let (mut carriers, mut note_of) = (vec![], vec![None; raws.len()]);
    // (A modulator slower than `LFO_HZ` is a vibrato: SiON's pitch LFO, on the note as it is; see below. The
    // first modulator note under each such key-on.)
    let mod_rate = |m: &Raw| -> Option<f64> { if m.noise.is_some() { return None } Some(loop_hz(m.p0, loop_len(&dsp.samples[m.smp])?)) };
    // (Each pitch-modulated note's modulators: the voice before's notes sounding with it.)
    let mut by_voice: [Vec<usize>; 8] = Default::default();
    for (i, r) in raws.iter().enumerate() { by_voice[r.voice].push(i) }
    let mods_of: Vec<Vec<usize>> = raws.iter().map(|r| {
        if !r.pmon || r.voice == 0 { return vec![] }
        by_voice[r.voice - 1].iter().copied().filter(|&m| raws[m].start < r.end && raws[m].end > r.start).collect()
    }).collect();
    let slow: Vec<Option<usize>> = raws.iter().enumerate().map(|(i, r)| {
        let ms = &mods_of[i];
        (r.noise.is_none() && !ms.is_empty() && ms.iter().all(|&m| mod_rate(&raws[m]).is_some_and(|f| f < LFO_HZ))).then(|| ms[0])
    }).collect();
    // (Each slow modulator sample's loudest envelope under the notes it modulates: its LFO table's depth.)
    let mut vib_env: HashMap<usize, f64> = HashMap::new();
    let mut vib_done = vec![false; raws.len()];
    // (Per modulator sample and pitch.)
    let mut quiet_env = vec![String::new(); raws.len()];
    let mut lfo_of: HashMap<(usize, u32, u32), Option<(String, f64)>> = HashMap::new();
    for (ri, r) in raws.iter().enumerate() {
        let Some(m) = slow[ri] else { continue };
        let e = &dsp.envs[r.voice - 1];
        let top = e.get((r.start / 32) as usize..((r.end / 32) as usize).min(e.len())).map_or(0, |s| s.iter().copied().max().unwrap_or(0));
        let x = vib_env.entry(raws[m].smp).or_default();
        *x = x.max(top as f64 / 2047.0);
    }
    for (ri, r) in raws.iter().enumerate() {
        let wav = if r.pmon && r.noise.is_none() && slow[ri].is_none() {
            *carrier_wave.entry(r.smp).or_insert_with(|| loop_wave(&dsp.samples[r.smp], false, &mut defs))
        } else { None };
        let instr = if r.noise.is_some() { "%2".to_string() } else if let Some((w, ..)) = wav { format!("%4@{w}") } else {
            let Some(n) = *pcm.entry(r.smp).or_insert_with(|| {
                let b = &dsp.samples[r.smp];
                let bk = base[&r.smp];
                let key = bk.round();
                let rate = (RATE * 2f64.powf((key - bk) / 12.0) * 1000.0).round() / 1000.0;
                // (As BRR, which SiON decodes and plays as the DSP does, with its Gaussian interpolation.)
                defs.pcm(pcmb(b.blocks.as_flattened(), rate, key, b.lp, 8))
            }) else { full = true; continue };
            format!("%7@{n}")
        };
        let mut amp = r.vol0.0;
        // Steps of attack, decay, sustain and release, and the sustain level.
        let ((ai, di, si), sl) = match (r.manual.is_some(), r.env) {
            (true, _) => ((f64::INFINITY, 0.0, 0.0), 0),
            (_, Env::Adsr(a, d, s, sr)) => {
                let a = if a == 15 { f64::INFINITY } else { rise_inc(RATES[a as usize * 2 + 1]) };
                let len = usual[&tone_of(r)].0;
                let (di, si, sl) = *adsr_fits.entry((d, s, sr, len)).or_insert_with(|| adsr_fit(d, s, sr, len));
                ((a, di, si), sl)
            }
            (_, Env::Direct(_)) => { amp *= r.env.gain(); ((f64::INFINITY, 0.0, 0.0), 0) }
            (_, Env::Slope(m, rt)) => match m {
                6 | 7 => ((rise_inc(RATES[rt as usize]), 0.0, 0.0), 0),
                _ => continue, // (falling from silence: never heard)
            },
        };
        let reli = match r.rel { Rel::Linear(p, step) => fall_inc(p, step), Rel::Exp(p) => *release_fits.entry(p).or_insert_with(|| release_fit(p)) };
        let (rates, clock) = fit_rates(&[(ai, 0.25), (di, 1.0), (si, 1.0), (reli, 0.5)]);
        // (The note's key: a wavetable plays its loop once a cycle.)
        let key_at = |p: u32| match wav { Some((_, _, n)) => hz_key(loop_hz(p, n)) as f64, None => key_of(r, p) };
        let k0 = key_at(r.p0);
        let key = k0.round().clamp(0.0, 127.0);
        // (SiON's rates go up with the key: taken down by as much at this note's.)
        let ks = ((key as i32 - 16).clamp(0, 127)) >> 5;
        if let Some((_, peak, _)) = wav { amp *= peak }
        let adj = |x: i32| if x == 0 || x >= 63 { x } else { (x - ks).clamp(1, 62) };
        let [mut ar, mut dr, mut sr, rr] = [adj(rates[0]), adj(rates[1]), adj(rates[2]), adj(rates[3])];
        let mut sl = sl;
        // A slow rise: the DSP's envelope as a table of the expression (as long as the tone's notes last), and
        // SiON's held at full.
        let mut rise = String::new();
        if r.manual.is_none() && *slow_rise.entry(r.env).or_insert_with(|| env_curve(r.env, 0).1 >= SLOW_RISE) {
            let len = usual[&tone_of(r)].1 as usize;
            rise = rises.entry((r.env, len)).or_insert_with(|| format!("na{}", defs.table(&table_text(&env_table(&env_curve(r.env, len).0))))).clone();
            (ar, dr, sr, sl) = (63, 0, 0, 0);
        }
        let tone = if r.noise.is_some() { 1 } else { instr[3..].parse().unwrap() };
        let send = if echo_on { format!("@v64,{}", if r.echo { send } else { 0 }) } else { String::new() };
        // Key-offs: a gap before the voice's next key-on (drivers key off a set time, or a fraction of the note,
        // before it) is kept as it is, counted back from where that note is written, so it comes out the same
        // every time (`@q`, `q`); otherwise on a whole or half row close by, or on the nearest tick.
        let start = on_grid(r.start);
        let xe = at(r.end);
        let stop = match next[ri] {
            Some(n) if at(raws[n].start) - xe < 12.0 => {
                let g = (at(raws[n].start) - xe).max(0.0);
                let g = match gap[r.voice] { Some(m) if (g - m as f64).abs() <= 1.0 => m, _ => g.round() as u64 };
                on_grid(raws[n].start).saturating_sub(g)
            }
            _ => snap(xe, &[(12.0, 1.0), (6.0, 0.75)]),
        }.max(start + 1);
        let level = r.noise.map_or(1.0, |hz| noise_level(hz));
        let mut n = Note::new(r.voice as u32, start, stop, key as u8, amp * level, instr[..2].to_string());
        let clk = if clock == 100 { String::new() } else { format!("@clock{clock}") };
        n.env = format!("@{tone},{ar},{dr},{sr},{rr},{sl}{send}{clk}{rise}");
        // (Without the echo send, for a copy into a pipe: it is not heard.)
        quiet_env[ri] = format!("{ar},{dr},{sr},{rr},{sl}{clk}{rise}");
        if let Some(m) = slow[ri] {
            let top = vib_env[&raws[m].smp];
            let lfo = lfo_of.entry((raws[m].smp, raws[m].p0, clock)).or_insert_with(|| vibrato(&raws[m], &dsp.samples[raws[m].smp], top, clock, &mut defs));
            if let Some((cmd, full)) = lfo.clone() {
                n.env.push_str(&cmd);
                // The depth as the modulator's envelope goes (its VOL doesn't count), on each tick it changes.
                let e = &dsp.envs[r.voice - 1];
                let depth = |t: u64| { let e = e.get((t / 32) as usize).copied().unwrap_or(0) as f64 / 2047.0; (full * e / top).round() as i32 };
                n.lfo0 = (depth(r.start), 0);
                let mut t = r.start;
                while t < r.end { let d = depth(t); if n.lfos.last().map_or(n.lfo0.0, |l| l.1.0) != d && tick(t) > n.start { n.lfos.push((tick(t), (d, 0))) } t += (RATE * row / 12.0) as u64 }
                vib_done[ri] = true;
            }
        }
        n.pan = r.vol0.1;
        // A vibrato the driver makes (and not PMON's) as SiON's LFO, in its shape: `@lfo` with a table of 256 steps
        // (one or several rounds, for a whole number of LFO frames to the table), from where it starts (the LFO
        // starts again at each key-on, and stands still while its depth is 0); `mp` its depth, from there, and `k`
        // its centre.
        let vib = if slow[ri].is_none() && r.noise.is_none() { DriverVib::find(r.start, r.end, r.p0, &r.pitch) } else { None };
        let (bent, unbent) = vib.as_ref().map_or((u64::MAX, u64::MAX), |v| (v.at, v.until));
        // The detune as it goes, exactly (1/64 semitones; DSP sample, value), from `dev0` at the key-on.
        let dev_of = |p: u32| (key_at(p) - key) * 64.0;
        let (mut dev0, mut kb, mut after) = (dev_of(r.p0), vec![], vec![]);
        for &(t, p) in &r.pitch {
            if t < bent { kb.push((t, dev_of(p))) } else if t >= unbent { after.push((t, dev_of(p))) }
        }
        if let Some(v) = &vib {
            let (_, nf) = lfo_fit(v.period / RATE, clock);
            let span = nf * lfo_frame(clock) * RATE; // (the table, DSP samples)
            let dev: Vec<f64> = (0..256).map(|i| dev_of(v.at((i as f64 + 0.5) / 256.0 * span))).collect();
            let (lo, hi) = dev.iter().fold((f64::MAX, f64::MIN), |a, &d| (a.0.min(d), a.1.max(d)));
            let (centre, depth) = ((hi + lo) / 2.0, (hi - lo) / 2.0);
            n.env.push_str(&lfo_cmd(nf, dev.iter().map(|d| 127.5 + 127.5 * (d - centre) / depth), &mut defs));
            let mp = depth.round().max(1.0) as i32;
            if tick(v.at) > n.start {
                kb.push((v.at, centre));
                n.lfos.push((tick(v.at), (mp, 0)));
            } else { (dev0, n.lfo0) = (centre, (mp, 0)) }
            if let Some(&(t, _)) = after.first() { if tick(t) > n.start { n.lfos.push((tick(t), (0, 0))) } }
            kb.extend(after);
        }
        // Written as `k` steps (a few 64ths either way are the drivers' pitch tables rounding: none)...
        n.k0 = rounded_detune(dev0);
        n.bends = kb.iter().map(|&(t, d)| (tick(t), rounded_detune(d))).collect();
        // ...or, a slide, as a pitch table (`np`, a value a 1/60 s frame from the key-on, to within a 64th, in
        // runs that lie on a line), where that is shorter: its lowest value the note's detune, so none is
        // negative. (Under the vibrato's LFO, the table moves its centre.)
        let slurs: usize = n.bends.windows(2).filter(|w| w[0].1 != w[1].1).count() + 1;
        if kb.len() >= 3 && slurs * 8 > 12 && defs.room() {
            let frame = RATE / 60.0;
            let vals: Vec<f64> = (0..((r.end - r.start) as f64 / frame).ceil().max(1.0) as usize).map(|i| step_at(&kb, (r.start as f64 + (i as f64 + 0.5) * frame) as u64, dev0)).collect();
            let base = vals.iter().fold(f64::MAX, |a, &v| a.min(v)).round();
            let top = vals.iter().fold(f64::MIN, |a, &v| a.max(v));
            let mut ints: Vec<i32> = vals.iter().map(|v| (v - base).round().max(0.0) as i32).collect();
            trim_held(&mut ints);
            let text = table_text(&ints);
            // (Within two octaves: a sweep down to nothing stays as it was.)
            if top - base <= 24.0 * 64.0 && text.len() + 12 < slurs * 8 { (n.k0, n.ptab) = (base as i32, Some(defs.table(&text) as u8)); n.bends.clear() }
        }
        // Volume: VOL × MVOL, times the envelope where it's followed step by step.
        let env_at = |t: u64| r.manual.as_ref().map_or(1.0, |m| step_at(m, t, 0.0));
        let vol_at = |t: u64| step_at(&r.vols, t, r.vol0);
        if let Some(m) = &r.manual {
            let wl = wav.map_or(1.0, |w| w.1);
            n.amp = r.vol0.0 * level * wl * env_at(r.start).max(m.first().map_or(0.0, |x| x.1));
            let mut times: Vec<u64> = m.iter().map(|x| x.0).chain(r.vols.iter().map(|x| x.0)).collect();
            times.sort();
            for t in times { if t > r.start { n.vols.push((tick(t), vol_at(t).0 * level * wl * env_at(t))) } }
        } else {
            for &(t, (a, _)) in &r.vols { n.vols.push((tick(t), a * level * (amp / r.vol0.0.max(1e-9)))) }
        }
        for &(t, (_, p)) in &r.vols { n.pans.push((tick(t), p)) }
        if wav.is_some() { carriers.push(ri) }
        note_of[ri] = Some(notes.len());
        notes.push(n);
    }
    let mut names: Vec<String> = (0..8).map(|v| format!("voice {}", v + 1)).collect();

    // Pitch modulation (PMON): the DSP bends a voice's pitch by the previous voice's output (before its VOL),
    // P × (1 + out / 32768): frequency modulation. In SiON a track's output can go to a pipe (`@o`) that
    // modulates the phase of another's operator (`@i`). The carrier plays its loop as a wavetable (above); a copy
    // of the modulator's notes plays the integral of its loop into the pipe, since modulating the phase by an
    // integral is modulating the frequency by the wave itself. The depth in cycles is the carrier's table rate ×
    // the integral's peak (frames at full scale) / the modulator's frames per second: `@i L` gives 2^(L-3) ×
    // 127/128 cycles for a full-scale pipe, and the copy's volume the rest. (Noise can't be integrated: a noise
    // modulator moves the phase by one noise step's worth, ±(carrier table rate / noise rate) cycles, which
    // sounds alike: a rough, breathy carrier.) The copy comes before the carrier, as SiON renders the tracks in
    // order a block at a time and reads a later track's pipe a block late; up to 4 pipes.
    let full_depth = |l: i32| 2f64.powi(l - 3) * sion::PIPE_DEPTH as f64;
    let mut mod_wave: HashMap<usize, Option<(usize, f32, usize)>> = HashMap::new();
    let mut pm: HashMap<usize, (usize, u32, u32)> = HashMap::new(); // carrier voice: (pipe, copy channel, carrier channel)
    let mut links: BTreeMap<usize, Vec<(usize, f64)>> = BTreeMap::new(); // modulator raw: (carrier raw, copy level)
    let (mut pm_notes, mut pm_lost) = (0usize, 0usize);
    for &ci in &carriers {
        let c = &raws[ci];
        let (Some(ni), Some((_, _, nc))) = (note_of[ci], carrier_wave[&c.smp]) else { continue };
        let ftc = loop_hz(c.p0, nc);
        let mut ds = vec![];
        for &m in mods_of[ci].iter().filter(|&&m| note_of[m].is_some()) {
            let rm = &raws[m];
            let d = match rm.noise {
                Some(hz) => (hz > 0.0).then(|| ftc / hz),
                None => mod_wave.entry(rm.smp).or_insert_with(|| loop_wave(&dsp.samples[rm.smp], true, &mut defs))
                    .map(|(_, peak, _)| ftc * peak as f64 / loop_hz(rm.p0, 1)),
            };
            // (Times its envelope's fixed GAIN level, if it has one.)
            if let Some(d) = d { ds.push((m, d * rm.env.gain() as f64)) }
        }
        let dmax = ds.iter().map(|x| x.1).fold(0.0, f64::max);
        let next = pm.len();
        if dmax <= 0.0 || (next >= 4 && !pm.contains_key(&c.voice)) { pm_lost += 1; continue }
        let &mut (pipe, _, carrier_ch) = pm.entry(c.voice).or_insert_with(|| {
            names.push(format!("voice {}, modulating voice {}", c.voice, c.voice + 1));
            names.push(format!("voice {}, pitch-modulated", c.voice + 1));
            (next, names.len() as u32 - 2, names.len() as u32 - 1)
        });
        let l = ((dmax / full_depth(3)).log2().ceil() as i32 + 3).clamp(1, 7);
        notes[ni].env.push_str(&format!("@i{l},{pipe}"));
        notes[ni].ch = carrier_ch;
        for (m, d) in ds { links.entry(m).or_default().push((ci, (d / full_depth(l)).min(1.0))) }
        pm_notes += 1;
    }
    // The modulators' copies, each at the depth of the carrier note sounding (none between them).
    for (&m, carried) in &links {
        let rm = &raws[m];
        let (pipe, copy_ch, _) = pm[&(rm.voice + 1)];
        let mut n = notes[note_of[m].unwrap()].clone();
        n.ch = copy_ch;
        let rest = &quiet_env[m];
        if rm.noise.is_none() {
            let (w, _, nm) = mod_wave[&rm.smp].unwrap();
            let key_at = |p: u32| hz_key(loop_hz(p, nm)) as f64;
            let k0 = key_at(rm.p0);
            n.key = k0.round().clamp(0.0, 127.0) as u8;
            let key = n.key as f64;
            n.k0 = detune(k0, key);
            (n.bends, n.ptab) = (rm.pitch.iter().map(|&(t, p)| (tick(t), detune(key_at(p), key))).collect(), None);
            (n.instr, n.env) = ("%4".into(), format!("@{w},{rest}@o1,{pipe}"));
        } else {
            n.env = format!("@1,{rest}@o1,{pipe}");
        }
        // Level: the envelope (its GAIN level, or the steps it is followed in), not VOL, times the depth.
        let depth: Vec<(u64, u64, f64)> = carried.iter().map(|&(ci, d)| { let c = &notes[note_of[ci].unwrap()]; (c.start, c.end, d) }).collect();
        let depth_at = |t: u64| depth.iter().find(|d| d.0 <= t && t < d.1).map_or(0.0, |d| d.2) as f32;
        let env: Vec<(u64, f32)> = rm.manual.as_ref().map_or(vec![], |e| e.iter().map(|&(t, a)| (tick(t), a)).collect());
        let env_at = |t: u64| if rm.manual.is_none() { rm.env.gain() } else { step_at(&env, t, env.first().map_or(0.0, |x| x.1)) };
        let mut times: Vec<u64> = depth.iter().flat_map(|d| [d.0, d.1]).chain(env.iter().map(|x| x.0)).filter(|&t| t > n.start && t < n.end).collect();
        times.sort();
        times.dedup();
        n.amp = env_at(n.start) * depth_at(n.start);
        n.vols = times.into_iter().map(|t| (t, env_at(t) * depth_at(t))).collect();
        n.pans.clear();
        n.pan = 0;
        notes.push(n);
    }
    let vibratos = vib_done.iter().filter(|&&d| d).count();
    // (The copies' channels.)
    let mut pipes: Vec<u32> = pm.values().map(|x| x.1).collect();
    pipes.sort();
    if vibratos > 0 { warnings.push(format!("pitch modulation (PMON) by a slow voice: {vibratos} notes, as a pitch LFO in its shape")) }
    if pm_notes > 0 {
        warnings.push(format!("pitch modulation (PMON): {pm_notes} notes, through a pipe from a copy of the voice before (a noise modulator is approximate)"));
    }
    let mut handled = vib_done;
    for &c in &carriers { handled[c] = true }
    let lost = pm_lost + raws.iter().enumerate().filter(|&(i, r)| r.pmon && r.noise.is_none() && note_of[i].is_some() && !handled[i]).count();
    if lost > 0 { warnings.push(format!("pitch modulation (PMON) on {lost} notes is not converted (a carrier or modulator sample without a loop of up to 1024 frames, or more than 4 modulated voices)")) }

    // A voice plays one note at a time.
    crate::score::monophonic(&mut notes);
    // A voice switching samples all the time is a drum kit (or several parts sharing it): one track per sample,
    // so each keeps its tone. (Channels after the voices.)
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
    // (A tone's `@n` leaves the envelope clock as it was: a track that sets it anywhere sets it with every tone.)
    let clocked: HashSet<u32> = notes.iter().filter(|n| n.env.contains("@clock")).map(|n| n.ch).collect();
    for n in notes.iter_mut().filter(|n| clocked.contains(&n.ch) && !n.env.contains("@clock")) { n.env.push_str("@clock100") }
    // (Nor does it end a rise's table, which holds the expression where it got to: on a track that has them, the
    // other tones set it back to full.)
    let risen: HashSet<u32> = notes.iter().filter(|n| n.env.contains("na")).map(|n| n.ch).collect();
    if !risen.is_empty() {
        let full = format!("na{}", defs.table("128"));
        for n in notes.iter_mut().filter(|n| risen.contains(&n.ch) && !n.env.contains("na")) { n.env.push_str(&full) }
    }
    // Each sample with its envelope and settings is an instrument (`#PCMWAVE`, `#PCMVOICE`): a note changing to it
    // chooses it by number (`@n`), not with all of it again. (Over 256 of them, the notes keep them in full.)
    let tones: HashSet<&str> = notes.iter().filter(|n| n.instr == "%7").map(|n| n.env.as_str()).collect();
    if tones.len() <= 256 {
        // (Each tone's `@n`, by its `@sample,ar,dr,sr,rr,sl` and the commands after them.)
        let mut num: HashMap<String, String> = HashMap::new();
        for n in notes.iter_mut().filter(|n| n.instr == "%7") {
            if let Some(i) = num.get(&n.env) { n.env = i.clone(); continue }
            let Some(e) = n.env.strip_prefix('@') else { continue };
            let (nums, cmds) = e.split_at(e.find(|c: char| !(c.is_ascii_digit() || c == ',' || c == '-')).unwrap_or(e.len()));
            let v: Vec<i32> = nums.split(',').filter_map(|x| x.parse().ok()).collect();
            let [smp, ar, dr, sr, rr, sl] = v[..] else { continue };
            let i = format!("@{}", defs.pcm_voice(smp as usize, [ar, dr, sr, rr, sl], cmds));
            num.insert(std::mem::replace(&mut n.env, i.clone()), i);
        }
    }
    if full { warnings.push("more than 256 different samples: the rest are left out".into()) }
    if notes.is_empty() { return Err("the SPC plays no sound the converter can carry over".into()) }
    header.extend(defs.header());

    let title = [tags.game.as_str(), tags.title.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" - ");
    let title = if tags.artist.is_empty() { title } else { format!("{title} ({})", tags.artist) };
    // The loop point moved on to the next bar line of the music (16 rows, from the row the most of the notes'
    // lengths start on), so the lines it is written in are its bars: the end moves with it, keeping the loop's
    // length, and the notes of the loop's first part are played again after the end, where the loop now runs on.
    let (mut end_t, mut lp_t) = (tick(end), lp.map(tick));
    if let Some(l) = &mut lp_t {
        const BAR: u64 = 192;
        let mut on: HashMap<u64, u64> = HashMap::new();
        for n in &notes { *on.entry(n.start % BAR).or_default() += n.end - n.start }
        let phase = (0..16).map(|r| r * 12).max_by_key(|p| (on.get(p).copied().unwrap_or(0), std::cmp::Reverse(*p))).unwrap();
        let d = (BAR + phase - *l % BAR) % BAR;
        let span = end_t - *l;
        // (A note held across the loop point is the one a loop later, cut short at the end: it goes on instead.)
        let again: Vec<Note> = notes.iter().filter(|n| n.start < *l + d && n.end > *l).map(|n| {
            let mut n = n.clone();
            n.map_times(|t| t + span);
            if n.end > end_t + d { n.clip(end_t + d) }
            n
        }).collect();
        notes.retain(|m| !again.iter().any(|n| n.start < end_t && n.ch == m.ch && n.key == m.key && n.start.abs_diff(m.start) <= 3));
        notes.extend(again);
        crate::score::monophonic(&mut notes);
        *l += d;
        end_t += d;
    }
    let mut score = Score {
        ppq: 48, tempos: vec![(0, 15.0 / row)], header, notes, end: end_t, lp: lp_t.filter(|&l| l < end_t), db: true, gates: true,
        names, title, pipes, ..Score::default()
    };
    score.drop_silent_loop();
    Ok((score, warnings))
}
