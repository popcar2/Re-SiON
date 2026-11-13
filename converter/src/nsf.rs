//! NSF / NSFE (and NSF2) reader: runs the file's sound driver on a 6502 (`cpu.rs`) the way an NSF player does
//! (INIT once, then PLAY at the file's rate), logs what it writes to the sound chips, and converts that log as a
//! VGM's (`vgm::render`). An NSF holds no song length: the song ends where the machine's state repeats (its
//! loop), or where the music repeats, or at the NSFE's track time.

use std::collections::HashMap;

use crate::chips::{Ay8910, Chip, NesApu, Vrc6};
use crate::cpu::{Bus, Cpu};
use crate::score::Score;
use crate::vgm::{Log, Write};

pub fn detect(d: &[u8]) -> bool { d.starts_with(b"NESM\x1a") || d.starts_with(b"NSFE") }

/// Expansion sound chips (header byte $7B).
const VRC6: u8 = 1;
const VRC7: u8 = 2;
const FDS: u8 = 4;
const MMC5: u8 = 8;
const N163: u8 = 16;
const S5B: u8 = 32;

/// How long to play an NSF that neither loops nor says how long it is.
const MAX_SECS: f64 = 600.0;

#[derive(Default)]
struct Nsf {
    load: u16,
    init: u16,
    play: u16,
    songs: usize,
    /// The song an NSF player starts with (from 0).
    first: usize,
    banks: Option<[u8; 8]>,
    prog: Vec<u8>,
    /// PLAY periods, microseconds.
    ntsc_us: u16,
    pal_us: u16,
    pal: bool,
    chips: u8,
    /// NSFE / NSF2 metadata: each track's length and fade (ms, negative: not given) and name.
    times: Vec<i32>,
    fades: Vec<i32>,
    labels: Vec<String>,
}

fn u16at(d: &[u8], o: usize) -> u16 { d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) }

fn parse(d: &[u8]) -> Result<Nsf, String> {
    let mut f = Nsf::default();
    if d.starts_with(b"NESM\x1a") {
        if d.len() < 0x80 { return Err("truncated NSF header".into()) }
        (f.songs, f.first) = (d[6].max(1) as usize, d[7].max(1) as usize - 1);
        (f.load, f.init, f.play) = (u16at(d, 8), u16at(d, 0x0a), u16at(d, 0x0c));
        (f.ntsc_us, f.pal_us) = (u16at(d, 0x6e), u16at(d, 0x78));
        let banks: [u8; 8] = d[0x70..0x78].try_into().unwrap();
        f.banks = banks.iter().any(|&b| b != 0).then_some(banks);
        f.pal = d[0x7a] & 3 == 1;
        f.chips = d[0x7b];
        // NSF2: a program length, followed by NSFE metadata chunks.
        let len = d[0x7d] as usize | (d[0x7e] as usize) << 8 | (d[0x7f] as usize) << 16;
        if d[5] >= 2 && len != 0 && 0x80 + len <= d.len() {
            f.prog = d[0x80..0x80 + len].to_vec();
            chunks(&d[0x80 + len..], &mut f, true)?;
        } else {
            f.prog = d[0x80..].to_vec();
        }
    } else {
        (f.songs, f.ntsc_us, f.pal_us) = (1, 0, 0);
        chunks(&d[4..], &mut f, false)?;
        if f.prog.is_empty() { return Err("NSFE without a DATA chunk".into()) }
    }
    if f.init == 0 && f.play == 0 { return Err("NSF without INIT and PLAY addresses".into()) }
    f.first = f.first.min(f.songs - 1);
    Ok(f)
}

/// NSFE chunks (`meta`: only the metadata ones, after an NSF2's program).
fn chunks(mut d: &[u8], f: &mut Nsf, meta: bool) -> Result<(), String> {
    let strings = |b: &[u8]| -> Vec<String> { b.split(|&c| c == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect() };
    while d.len() >= 8 {
        let len = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
        let (id, body) = (&d[4..8], d.get(8..8 + len).ok_or("truncated NSFE chunk")?);
        match id {
            b"INFO" if !meta => {
                if body.len() < 8 { return Err("NSFE INFO chunk too short".into()) }
                (f.load, f.init, f.play) = (u16at(body, 0), u16at(body, 2), u16at(body, 4));
                (f.pal, f.chips) = (body[6] & 3 == 1, body[7]);
                f.songs = body.get(8).map_or(1, |&n| n.max(1) as usize);
                f.first = body.get(9).copied().unwrap_or(0) as usize;
            }
            b"DATA" if !meta => f.prog = body.to_vec(),
            b"BANK" if !meta => { let mut b = [0u8; 8]; b[..body.len().min(8)].copy_from_slice(&body[..body.len().min(8)]); f.banks = Some(b) }
            b"RATE" if !meta => (f.ntsc_us, f.pal_us) = (u16at(body, 0), u16at(body, 2)),
            b"time" => f.times = body.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
            b"fade" => f.fades = body.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
            b"tlbl" => f.labels = strings(body),
            b"NEND" => break,
            _ => {}
        }
        d = &d[8 + len..];
    }
    Ok(())
}

/// The console as the driver sees it: RAM, the cartridge's memory and bank registers, the expansion chips' few
/// readable registers; and the log of sound writes.
struct Nes {
    ram: [u8; 0x800],
    /// $5C00-$FFFF (MMC5 ExRAM, work RAM, the program; FDS RAM up to $DFFF).
    mem: Vec<u8>,
    /// The program in 4 KB banks, when bankswitched.
    rom: Option<Vec<u8>>,
    chips: u8,
    mul: (u8, u8),
    n163: ([u8; 128], u8),
    ay_reg: u8,
    /// The last $4012 and $4013 (DPCM address and length).
    dmc: (u8, u8),
    clock: f64,
    /// Chip numbers in the log.
    mmc5: Option<usize>,
    vrc6: Option<usize>,
    ay: Option<usize>,
    writes: Vec<Write>,
    blocks: Vec<(u8, usize, Vec<u8>)>,
    /// DPCM samples already passed to the APU model: (address, length) → contents.
    sent: HashMap<(usize, usize), Vec<u8>>,
    /// What the music wrote this frame (a hash), and which unconverted chips it used.
    sig: u64,
    used: u8,
    mmc5_pcm: bool,
}

const SENTINEL: u16 = 0x4f00;

fn mix(h: u64, v: u64) -> u64 { (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95) }

impl Nes {
    fn bank(&mut self, slot: usize, b: u8) {
        let Some(rom) = &self.rom else { return };
        let from = b as usize * 0x1000;
        let at = 0x6000 + slot * 0x1000 - 0x5c00;
        for k in 0..0x1000 { self.mem[at + k] = rom.get(from + k).copied().unwrap_or(0) }
    }
    fn log(&mut self, cycle: u64, chip: usize, reg: u8, val: u8) {
        self.sig = mix(mix(self.sig, (chip as u64) << 8 | reg as u64), val as u64);
        self.writes.push(Write::reg((cycle as f64 / self.clock * 44100.0).round() as u64, chip, 0, reg, val));
    }
    /// The machine state the driver's future depends on.
    fn state(&self, cpu: &Cpu) -> u64 {
        let mut h = mix(cpu.s as u64, self.mul.0 as u64 | (self.mul.1 as u64) << 8 | (self.n163.1 as u64) << 16 | (self.ay_reg as u64) << 24);
        let top = if self.chips & FDS != 0 { 0xe000 } else { 0x8000 };
        for w in self.ram.chunks_exact(8).chain(self.mem[..top - 0x5c00].chunks_exact(8)).chain(self.n163.0.chunks_exact(8)) {
            h = mix(h, u64::from_le_bytes(w.try_into().unwrap()));
        }
        // (Bankswitched: which banks are in, as their first bytes.)
        if self.rom.is_some() { for s in 0..10 { h = mix(h, u64::from_le_bytes(self.mem[0x6000 - 0x5c00 + s * 0x1000..][..8].try_into().unwrap())) } }
        h
    }
}

impl Bus for Nes {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0..=0x1fff => self.ram[a as usize & 0x7ff],
            0x4800 if self.chips & N163 != 0 => {
                let (ram, addr) = &mut self.n163;
                let v = ram[*addr as usize & 0x7f];
                if *addr & 0x80 != 0 { *addr = 0x80 | (addr.wrapping_add(1) & 0x7f) }
                v
            }
            0x5205 if self.chips & MMC5 != 0 => (self.mul.0 as u16 * self.mul.1 as u16) as u8,
            0x5206 if self.chips & MMC5 != 0 => ((self.mul.0 as u16 * self.mul.1 as u16) >> 8) as u8,
            0x5c00..=0xffff => self.mem[a as usize - 0x5c00],
            _ => 0,
        }
    }

    fn write(&mut self, a: u16, v: u8, cycle: u64) {
        let has = |c: u8| self.chips & c != 0;
        match a {
            0..=0x1fff => self.ram[a as usize & 0x7ff] = v,
            0x4000..=0x4013 | 0x4015 | 0x4017 => {
                // A DPCM sample starting: hand the APU model its bytes as the program's memory has them now.
                if a == 0x4015 && v & 0x10 != 0 {
                    let (addr, len) = (0xc000 + self.dmc.0 as usize * 64, self.dmc.1 as usize * 16 + 1);
                    let bytes: Vec<u8> = (0..len).map(|k| self.mem[((addr + k) & 0xffff | 0x8000) - 0x5c00]).collect();
                    if self.sent.get(&(addr, len)) != Some(&bytes) {
                        self.sent.insert((addr, len), bytes.clone());
                        self.blocks.push((0xc2, addr, bytes));
                        let at = (cycle as f64 / self.clock * 44100.0).round() as u64;
                        self.writes.push(Write { at, chip: 0, port: 0, reg: 0, val: 0, block: Some(self.blocks.len() - 1), src: None });
                    }
                }
                if a == 0x4012 { self.dmc.0 = v }
                if a == 0x4013 { self.dmc.1 = v }
                self.log(cycle, 0, (a - 0x4000) as u8, v)
            }
            0x4040..=0x4092 if has(FDS) => self.used |= FDS,
            0x4800 if has(N163) => {
                self.used |= N163;
                let (ram, addr) = &mut self.n163;
                ram[*addr as usize & 0x7f] = v;
                if *addr & 0x80 != 0 { *addr = 0x80 | (addr.wrapping_add(1) & 0x7f) }
            }
            0xf800..=0xffff if has(N163) => self.n163.1 = v,
            0x5000..=0x5015 if has(MMC5) => match a {
                // (The MMC5's pulses have no sweep units.)
                0x5000 | 0x5002 | 0x5003 | 0x5004 | 0x5006 | 0x5007 => { let c = self.mmc5.unwrap(); self.log(cycle, c, (a - 0x5000) as u8, v) }
                0x5015 => { let c = self.mmc5.unwrap(); self.log(cycle, c, 0x15, v & 3) }
                0x5011 => self.mmc5_pcm |= v != 0,
                _ => {}
            },
            0x5205 if has(MMC5) => self.mul.0 = v,
            0x5206 if has(MMC5) => self.mul.1 = v,
            0x5c00..=0x5ff5 if has(MMC5) => self.mem[a as usize - 0x5c00] = v,
            0x5ff6..=0x5fff if self.rom.is_some() => {
                let slot = a as usize - 0x5ff6;
                if slot >= 2 || has(FDS) { self.bank(slot, v) }
            }
            0x6000..=0x7fff => self.mem[a as usize - 0x5c00] = v,
            0x9000..=0x9003 | 0xa000..=0xa002 | 0xb000..=0xb002 if has(VRC6) => {
                let c = self.vrc6.unwrap();
                self.log(cycle, c, ((a >> 12) - 9) as u8 * 4 + (a & 3) as u8, v)
            }
            0x9010 | 0x9030 if has(VRC7) => self.used |= VRC7,
            0xc000..=0xdfff if has(S5B) => self.ay_reg = v & 15,
            0xe000..=0xffff if has(S5B) => { let (c, r) = (self.ay.unwrap(), self.ay_reg); self.log(cycle, c, r, v) }
            0x8000..=0xdfff if has(FDS) => self.mem[a as usize - 0x5c00] = v,
            _ => {}
        }
    }
}

/// Call a driver routine, as JSR from the player: until it returns, or `budget` cycles. Whether it returned.
fn call(cpu: &mut Cpu, nes: &mut Nes, addr: u16, budget: u64) -> bool {
    let r = SENTINEL - 1;
    cpu.push(nes, (r >> 8) as u8);
    cpu.push(nes, r as u8);
    let (s, limit) = (cpu.s, cpu.cycles + budget);
    cpu.pc = addr;
    while cpu.pc != SENTINEL && !cpu.jammed && cpu.cycles < limit { cpu.step(nes) }
    let done = cpu.pc == SENTINEL;
    if !done { cpu.s = s.wrapping_add(2) }
    cpu.jammed = false;
    done
}

/// The earliest point from which the per-frame write hashes repeat to the end, at least twice, with a period of
/// at least `min` frames: (start, period).
pub(crate) fn repeat(sig: &[u64], min: usize) -> Option<(usize, usize)> {
    let n = sig.len();
    let mut best: Option<(usize, usize)> = None;
    for p in min.max(1)..=n / 2 {
        let mut i = n - p;
        while i > 0 && sig[i - 1] == sig[i - 1 + p] { i -= 1 }
        if n - i >= 2 * p && best.is_none_or(|(j, _)| i < j) { best = Some((i, p)) }
        if best.is_some_and(|(j, _)| j == 0) { break }
    }
    best
}

/// `track`: which song of the file (from 1; default: the file's own first song).
pub fn to_score(data: &[u8], exact: bool, track: Option<usize>) -> Result<(Score, Vec<String>), String> {
    let f = parse(data)?;
    let mut warnings = vec![];
    let song = match track {
        Some(t) if t == 0 || t > f.songs => return Err(format!("no track {t}: the file has {} (1-{})", f.songs, f.songs)),
        Some(t) => t - 1,
        None => f.first,
    };
    if f.songs > 1 {
        let name = f.labels.get(song).filter(|l| !l.is_empty()).map_or(String::new(), |l| format!(" ({l})"));
        warnings.push(format!("track {} of {}{name}; choose another with --track N", song + 1, f.songs));
    }
    let clock = if f.pal { 1_662_607.0 } else { 1_789_773.0 };
    let us = match (f.pal, f.ntsc_us, f.pal_us) { (false, 0, _) => 16639, (false, n, _) => n, (true, _, 0) => 19997, (true, _, p) => p } as f64;

    // Chips: the APU, then the expansion chips that are converted.
    let mut chips: Vec<Box<dyn Chip>> = vec![Box::new(NesApu::power_on(clock as u32))];
    let mmc5 = (f.chips & MMC5 != 0).then(|| { chips.push(Box::new(NesApu::mmc5(clock as u32))); chips.len() - 1 });
    // (The Sunsoft 5B is a YM2149 that halves the CPU clock.)
    let vrc6 = (f.chips & VRC6 != 0).then(|| { chips.push(Box::new(Vrc6::new(clock))); chips.len() - 1 });
    let ay = (f.chips & S5B != 0).then(|| { chips.push(Box::new(Ay8910::new(clock / 2.0))); chips.len() - 1 });
    for c in &mut chips { c.set_exact(exact) }

    let mut nes = Nes {
        ram: [0; 0x800], mem: vec![0; 0x10000 - 0x5c00], rom: None, chips: f.chips, mul: (0xff, 0xff), n163: ([0; 128], 0), ay_reg: 0, dmc: (0, 0), clock,
        mmc5, vrc6, ay, writes: vec![], blocks: vec![], sent: HashMap::new(), sig: 0, used: 0, mmc5_pcm: false,
    };
    match f.banks {
        Some(banks) => {
            let mut rom = vec![0; f.load as usize & 0xfff];
            rom.extend(&f.prog);
            nes.rom = Some(rom);
            for (k, &b) in banks.iter().enumerate() { nes.bank(k + 2, b) }
            if f.chips & FDS != 0 { nes.bank(0, banks[6]); nes.bank(1, banks[7]) }
        }
        None => {
            let at = (f.load as usize).max(0x5c00) - 0x5c00;
            let n = f.prog.len().min(nes.mem.len().saturating_sub(at));
            nes.mem[at..at + n].copy_from_slice(&f.prog[..n]);
        }
    }

    // The player's setup (the APU model starts cleared): channels enabled, the frame counter in 4-step mode.
    nes.log(0, 0, 0x15, 0x0f);
    nes.log(0, 0, 0x17, 0x40);
    let mut cpu = Cpu::new();
    (cpu.a, cpu.x, cpu.s) = (song as u8, f.pal as u8, 0xff);
    if !call(&mut cpu, &mut nes, f.init, clock as u64 * 10) { warnings.push("the INIT routine did not return (within 10 s)".into()) }

    // PLAY once per period, until the machine's state repeats (the song loops) or the time runs out.
    let period = clock * us / 1e6;
    let given = f.times.get(song).filter(|&&t| t > 0).map(|&t| (t + f.fades.get(song).copied().unwrap_or(0).max(0)) as f64 / 1000.0);
    let max = (given.unwrap_or(MAX_SECS) * 1e6 / us).ceil() as usize;
    // (Frames on a grid from time 0: the first after INIT has returned.)
    let t0 = ((cpu.cycles as f64 / period).ceil() * period) as u64;
    let (mut seen, mut sig, mut starts, mut late) = (HashMap::new(), vec![], vec![], 0usize);
    let mut exact_loop = None;
    for k in 0..max {
        let at = (t0 as f64 + k as f64 * period).round() as u64;
        if let Some(j) = seen.insert(nes.state(&cpu), k) { exact_loop = Some((j, k)); break }
        cpu.cycles = cpu.cycles.max(at);
        starts.push(cpu.cycles);
        nes.sig = 0;
        if !call(&mut cpu, &mut nes, f.play, period as u64 * 4) { late += 1 }
        sig.push(nes.sig);
    }
    if late > 0 { warnings.push(format!("the PLAY routine did not return within 4 frames {late} times")) }
    let frames = starts.len();
    let (end_frame, lp) = match exact_loop {
        Some((j, k)) => (k, Some(j)),
        // The music repeating (a loop of at least a second), or the same writes every frame to the end (the
        // song has stopped, or holds its last chord: it ends there).
        None => match (repeat(&sig, (1e6 / us) as usize), sig.iter().rposition(|&h| Some(&h) != sig.last()).map_or(0, |i| i + 1)) {
            (Some((j, p)), still) if j < still => (j + p, Some(j)),
            (_, still) if (frames - still) as f64 * us >= 1e6 => (still + 1, None),
            _ if given.is_some() => (frames, None),
            _ => { warnings.push(format!("no loop found in the first {} minutes: converted that much", MAX_SECS / 60.0)); (frames, None) }
        },
    };
    // (The end rounded down, so it doesn't reach into another frame; the loop point is at a frame's start.)
    let end = ((if end_frame < frames { starts[end_frame] as f64 } else { t0 as f64 + frames as f64 * period }) / clock * 44100.0) as u64;
    let loop_at = lp.map(|j| (starts[j] as f64 / clock * 44100.0).round() as u64);

    for (bit, name) in [(VRC7, "Konami VRC7"), (FDS, "Famicom Disk System"), (N163, "Namco 163")] {
        if nes.used & bit != 0 { warnings.push(format!("{name} sound is not converted")) }
    }
    if nes.mmc5_pcm { warnings.push("MMC5 PCM is not converted".into()) }
    let mut writes = std::mem::take(&mut nes.writes);
    writes.retain(|w| w.at < end);
    let level = vec![1.0; chips.len()];
    let log = Log { chips, level, writes, blocks: nes.blocks, end, loop_at, period: Some(44100.0 * us / 1e6), phase: Some(0.0), dac: None, shelf: false };
    let (mut score, w) = crate::vgm::render(log, warnings)?;
    // A loop of silence is the song having ended: it plays once, to its last sound.
    if let Some(l) = score.lp { if !score.notes.iter().any(|n| n.end > l) { score.lp = None } }
    if score.lp.is_none() { if let Some(last) = score.notes.iter().map(|n| n.end).max() { score.end = score.end.min(last) } }
    if score.notes.is_empty() { return Err(format!("track {} plays no sound the converter can carry over", song + 1)) }
    Ok((score, w))
}
