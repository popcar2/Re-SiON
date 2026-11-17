//! NSF / NSFE (and NSF2) reader: runs the file's sound driver on a 6502 (`cpu.rs`) the way an NSF player does
//! (INIT once, then PLAY at the file's rate; NSF2: a non-returning INIT that PLAY interrupts as an NMI, and IRQs
//! from a cycle timer, the APU frame counter and the DMC), logs what it writes to the sound chips, and converts
//! that log as a VGM's (`vgm::render`). An NSF holds no song length: the song ends where the machine's state
//! repeats (its loop), or where the music repeats, or at the NSFE's track time.

use std::collections::HashMap;

use crate::chips::{Ay8910, Chip, NesApu, Vrc6};
use crate::bytes::u16le;
use crate::cpu::{Bus, Cpu, I};
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

/// NSF2 features (header byte $7C).
const IRQ: u8 = 0x10;
const NON_RETURNING: u8 = 0x20;
const NO_PLAY: u8 = 0x40;
const MANDATORY: u8 = 0x80;

/// Regions (the INIT routine's X).
const NTSC: u8 = 0;
const PAL: u8 = 1;
const DENDY: u8 = 2;

/// How long to play an NSF that neither loops nor says how long it is.
pub(crate) const MAX_SECS: f64 = 600.0;

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
    dendy_us: u16,
    /// The header's PAL/NTSC bits; an NSFE `regn` chunk's regions and preferred one.
    timing: u8,
    regn: Option<(u8, Option<u8>)>,
    chips: u8,
    /// NSF2 features.
    flags: u8,
    /// NSFE / NSF2 metadata: each track's length and fade (ms, negative: not given) and name.
    times: Vec<i32>,
    fades: Vec<i32>,
    labels: Vec<String>,
}

fn parse(d: &[u8]) -> Result<Nsf, String> {
    let mut f = Nsf::default();
    if d.starts_with(b"NESM\x1a") {
        if d.len() < 0x80 { return Err("truncated NSF header".into()) }
        (f.songs, f.first) = (d[6].max(1) as usize, d[7].max(1) as usize - 1);
        (f.load, f.init, f.play) = (u16le(d, 8), u16le(d, 0x0a), u16le(d, 0x0c));
        (f.ntsc_us, f.pal_us) = (u16le(d, 0x6e), u16le(d, 0x78));
        let banks: [u8; 8] = d[0x70..0x78].try_into().unwrap();
        f.banks = banks.iter().any(|&b| b != 0).then_some(banks);
        f.timing = d[0x7a];
        f.chips = d[0x7b];
        if d[5] >= 2 { f.flags = d[0x7c] & 0xf0 }
        // NSF2: a program length, followed by NSFE metadata chunks.
        let len = d[0x7d] as usize | (d[0x7e] as usize) << 8 | (d[0x7f] as usize) << 16;
        if d[5] >= 2 && len != 0 && 0x80 + len <= d.len() {
            f.prog = d[0x80..0x80 + len].to_vec();
            let mandatory = f.flags & MANDATORY != 0;
            chunks(&d[0x80 + len..], &mut f, true, mandatory)?;
        } else {
            f.prog = d[0x80..].to_vec();
        }
    } else {
        (f.songs, f.ntsc_us, f.pal_us) = (1, 0, 0);
        chunks(&d[4..], &mut f, false, true)?;
        if f.prog.is_empty() { return Err("NSFE without a DATA chunk".into()) }
    }
    if f.init == 0 && f.play == 0 { return Err("NSF without INIT and PLAY addresses".into()) }
    if f.flags & NO_PLAY != 0 && f.flags & (NON_RETURNING | IRQ) == 0 { return Err("NSF2 that suppresses PLAY but has neither a non-returning INIT nor IRQs".into()) }
    f.first = f.first.min(f.songs - 1);
    Ok(f)
}

/// NSFE chunks (`meta`: only the metadata ones, after an NSF2's program). A chunk whose name starts with a
/// capital is needed to play the file: one the converter doesn't know is an error (`mandatory`: an NSFE, or an
/// NSF2 that says it has one).
fn chunks(mut d: &[u8], f: &mut Nsf, meta: bool, mandatory: bool) -> Result<(), String> {
    let strings = |b: &[u8]| -> Vec<String> { b.split(|&c| c == 0).map(|s| String::from_utf8_lossy(s).into_owned()).collect() };
    while d.len() >= 8 {
        let len = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
        let (id, body) = (&d[4..8], d.get(8..8 + len).ok_or("truncated NSFE chunk")?);
        match id {
            b"INFO" if !meta => {
                if body.len() < 8 { return Err("NSFE INFO chunk too short".into()) }
                (f.load, f.init, f.play) = (u16le(body, 0), u16le(body, 2), u16le(body, 4));
                (f.timing, f.chips) = (body[6], body[7]);
                f.songs = body.get(8).map_or(1, |&n| n.max(1) as usize);
                f.first = body.get(9).copied().unwrap_or(0) as usize;
            }
            b"DATA" if !meta => f.prog = body.to_vec(),
            b"BANK" if !meta => { let mut b = [0u8; 8]; b[..body.len().min(8)].copy_from_slice(&body[..body.len().min(8)]); f.banks = Some(b) }
            b"RATE" => {
                (f.ntsc_us, f.pal_us, f.dendy_us) = (u16le(body, 0), u16le(body, 2), u16le(body, 4));
            }
            b"NSF2" if !meta => f.flags = body.first().copied().unwrap_or(0) & 0xf0,
            b"regn" if !body.is_empty() => f.regn = Some((body[0], body.get(1).copied())),
            // (The VRC7 chunk says whether the chip is a VRC7 or a YM2413, and its voices: neither converted.)
            b"VRC7" | b"INFO" | b"DATA" | b"BANK" | b"NSF2" => {}
            b"time" => f.times = body.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
            b"fade" => f.fades = body.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect(),
            b"tlbl" => f.labels = strings(body),
            b"NEND" => break,
            id if mandatory && id[0].is_ascii_uppercase() => {
                return Err(format!("the file needs its '{}' chunk to play, which the converter doesn't know", String::from_utf8_lossy(id)))
            }
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
    /// NSF2: the IRQ vector's RAM at $FFFE (the player's overlay on the program), and the IRQ sources (when the
    /// file asks for them).
    overlay: bool,
    vector: [u8; 2],
    irqs: Option<Irqs>,
    /// The CPU cycle now, for the IRQ sources.
    now: u64,
    /// How finely the IRQ sources' timing counts in the state (CPU cycles: 1/64 of a frame), so a source that
    /// drifts a few cycles a loop against the frames doesn't keep it from coming round.
    quantum: u64,
}

/// NSF2 IRQ sources: the player's cycle timer ($401B-$401D), the APU frame counter and the DMC.
#[derive(Default)]
struct Irqs {
    reload: u16,
    /// When the timer next underflows, while it runs.
    timer: Option<u64>,
    timer_flag: bool,
    /// The frame counter's period (CPU cycles) and its next IRQ, while it raises them.
    frame_period: u64,
    frame: Option<u64>,
    frame_flag: bool,
    /// $4010, and when the playing DPCM sample ends.
    dmc_ctl: u8,
    dmc_end: Option<u64>,
    dmc_flag: bool,
    pal: bool,
}

impl Irqs {
    /// Bring the sources up to cycle `now` (`len`: the DMC's sample length, $4013).
    fn tick(&mut self, now: u64, len: u8) {
        let dmc = self.dmc_cycles(len);
        if let Some(t) = &mut self.timer {
            while *t <= now { self.timer_flag = true; *t += self.reload as u64 + 1 }
        }
        if let Some(t) = &mut self.frame {
            while *t <= now { self.frame_flag = true; *t += self.frame_period }
        }
        if let Some(t) = self.dmc_end.filter(|&t| t <= now) {
            // (Looping: the sample starts over, with no IRQ.)
            if self.dmc_ctl & 0x40 != 0 { self.dmc_end = Some(t + dmc.max(1)) } else {
                self.dmc_end = None;
                if self.dmc_ctl & 0x80 != 0 { self.dmc_flag = true }
            }
        }
    }
    fn line(&self) -> bool { self.timer_flag || self.frame_flag || self.dmc_flag }
    /// The next cycle a source may raise its IRQ.
    fn next(&self) -> u64 { [self.timer, self.frame, self.dmc_end].iter().flatten().copied().min().unwrap_or(u64::MAX) }
    /// How long the DMC takes to play its sample (CPU cycles), by $4010 and $4013.
    fn dmc_cycles(&self, len: u8) -> u64 {
        let rate = crate::chips::DMC_PERIODS[self.pal as usize][self.dmc_ctl as usize & 15] as u64;
        (len as u64 * 16 + 1) * 8 * rate
    }
}

const SENTINEL: u16 = 0x4f00;

pub(crate) fn mix(h: u64, v: u64) -> u64 { (h.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95) }

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
    /// The IRQ line, at cycle `now`.
    fn irq(&mut self) -> bool {
        let Some(q) = &mut self.irqs else { return false };
        q.tick(self.now, self.dmc.1);
        q.line()
    }
    /// The machine state the driver's future depends on (`full`: the CPU's registers and the IRQ sources too,
    /// for a driver that is interrupted rather than called).
    fn state(&self, cpu: &Cpu, full: bool) -> u64 {
        let mut h = mix(cpu.s as u64, self.mul.0 as u64 | (self.mul.1 as u64) << 8 | (self.n163.1 as u64) << 16 | (self.ay_reg as u64) << 24);
        if full {
            h = mix(h, cpu.a as u64 | (cpu.x as u64) << 8 | (cpu.y as u64) << 16 | (cpu.p as u64) << 24 | (cpu.pc as u64) << 32);
            h = mix(h, u16::from_le_bytes(self.vector) as u64 | (self.dmc.0 as u64) << 16 | (self.dmc.1 as u64) << 24);
            if let Some(q) = &self.irqs {
                let rel = |t: Option<u64>| t.map_or(u64::MAX, |t| t.saturating_sub(self.now) / self.quantum);
                h = mix(h, q.reload as u64 | (q.dmc_ctl as u64) << 16 | (q.timer_flag as u64) << 24 | (q.frame_flag as u64) << 25 | (q.dmc_flag as u64) << 26);
                for t in [q.timer, q.frame, q.dmc_end] { h = mix(h, rel(t)) }
            }
        }
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
            0xfffe | 0xffff if self.overlay => self.vector[a as usize - 0xfffe],
            0x4015 | 0x401b..=0x401d if self.irqs.is_some() => {
                let q = self.irqs.as_mut().unwrap();
                q.tick(self.now, self.dmc.1);
                match a {
                    0x4015 => {
                        let v = (q.dmc_flag as u8) << 7 | (q.frame_flag as u8) << 6 | (q.dmc_end.is_some() as u8) << 4;
                        q.frame_flag = false;
                        v
                    }
                    0x401b => q.reload as u8,
                    0x401c => (q.reload >> 8) as u8,
                    _ => { let v = (q.timer_flag as u8) << 7 | q.timer.is_some() as u8; q.timer_flag = false; v }
                }
            }
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
        if let Some(q) = &mut self.irqs {
            let (now, len) = (cycle, self.dmc.1);
            q.tick(now, len);
            match a {
                0x4010 => { q.dmc_ctl = v; if v & 0x80 == 0 { q.dmc_flag = false } }
                0x4015 => {
                    q.dmc_flag = false;
                    if v & 0x10 == 0 { q.dmc_end = None } else if q.dmc_end.is_none() { q.dmc_end = Some(now + q.dmc_cycles(len)) }
                }
                0x4017 => {
                    if v & 0x40 != 0 { q.frame_flag = false }
                    q.frame = (v & 0xc0 == 0).then_some(now + q.frame_period);
                }
                0x401b => q.reload = q.reload & 0xff00 | v as u16,
                0x401c => q.reload = q.reload & 0xff | (v as u16) << 8,
                0x401d => {
                    if v & 1 == 0 { q.timer = None } else if q.timer.is_none() { q.timer = Some(now + q.reload as u64 + 1) }
                }
                _ => {}
            }
        }
        match a {
            0..=0x1fff => self.ram[a as usize & 0x7ff] = v,
            0xfffe | 0xffff if self.overlay => self.vector[a as usize - 0xfffe] = v,
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

/// Take the IRQ, if it is raised and not masked. Whether it was taken.
fn take_irq(cpu: &mut Cpu, nes: &mut Nes) -> bool {
    nes.now = cpu.cycles;
    let take = !cpu.masked() && nes.irq();
    if take { cpu.interrupt(nes, 0xfffe) }
    take
}

/// One instruction, or the IRQ the CPU takes instead.
fn step(cpu: &mut Cpu, nes: &mut Nes) { if !take_irq(cpu, nes) { cpu.step(nes) } }

/// Push the address the player's code returns to (the sentinel, its idle loop).
fn push_return(cpu: &mut Cpu, nes: &mut Nes) {
    let r = SENTINEL - 1;
    cpu.push(nes, (r >> 8) as u8);
    cpu.push(nes, r as u8);
}

/// Call a driver routine, as JSR from the player: until it returns, or `budget` cycles. Whether it returned.
fn call(cpu: &mut Cpu, nes: &mut Nes, addr: u16, budget: u64) -> bool {
    push_return(cpu, nes);
    let (s, limit) = (cpu.s, cpu.cycles + budget);
    cpu.pc = addr;
    while cpu.pc != SENTINEL && !cpu.jammed && cpu.cycles < limit { step(cpu, nes) }
    let done = cpu.pc == SENTINEL;
    // (Abandoned: back to the player's idle loop.)
    if !done { (cpu.s, cpu.pc) = (s.wrapping_add(2), SENTINEL) }
    cpu.jammed = false;
    done
}

/// NSF2: PLAY as the player's NMI handler calls it, interrupting whatever runs (a non-returning INIT, an IRQ
/// handler, the idle loop), IRQs masked, the registers restored after. Whether it returned within `budget`.
fn nmi_play(cpu: &mut Cpu, nes: &mut Nes, play: u16, budget: u64) -> bool {
    let saved = (cpu.a, cpu.x, cpu.y, cpu.p, cpu.pc, cpu.s);
    // (The NMI pushes the return address and flags: 3 bytes of stack below what runs.)
    cpu.s = cpu.s.wrapping_sub(3);
    cpu.p |= I;
    let done = call(cpu, nes, play, budget);
    (cpu.a, cpu.x, cpu.y, cpu.p, cpu.pc, cpu.s) = saved;
    done
}

/// Let the CPU run until cycle `until`: the code it is in (at the sentinel: the player's idle loop, which only
/// waits), taking IRQs. A CPU that jams goes back to idling; how many times, in `jams`.
fn run(cpu: &mut Cpu, nes: &mut Nes, until: u64, jams: &mut usize) {
    while cpu.cycles < until {
        if take_irq(cpu, nes) { continue }
        if cpu.pc == SENTINEL {
            let next = nes.irqs.as_ref().map_or(u64::MAX, Irqs::next);
            cpu.cycles = if next > cpu.cycles { next.min(until) } else { until };
            continue;
        }
        cpu.step(nes);
        if cpu.jammed { (cpu.jammed, cpu.pc) = (false, SENTINEL); *jams += 1 }
    }
}

/// The earliest point from which the per-frame write hashes repeat to the end, at least twice, with a period of
/// at least `min` frames: (start, period).
pub(crate) fn repeat(sig: &[u64], min: usize) -> Option<(usize, usize)> {
    let n = sig.len();
    // How far back from the end the hashes match those `p` frames later: the longest common suffix of `sig` and
    // `sig[..n - p]`, for every `p` at once (the Z-function of `sig` read backwards), in linear time.
    let r = |k: usize| sig[n - 1 - k];
    let mut z = vec![0usize; n];
    let (mut lo, mut hi) = (0, 0);
    for k in 1..n {
        let mut m = if k < hi { z[k - lo].min(hi - k) } else { 0 };
        while k + m < n && r(m) == r(k + m) { m += 1 }
        z[k] = m;
        if k + m > hi { (lo, hi) = (k, k + m) }
    }
    let mut best: Option<(usize, usize)> = None;
    for p in min.max(1)..=n / 2 {
        let i = n - p - z[p];
        if n - i >= 2 * p && best.is_none_or(|(j, _)| i < j) { best = Some((i, p)) }
        if best.is_some_and(|(j, _)| j == 0) { break }
    }
    best
}

/// Where a played song ends (a frame) and where it loops, from each frame's writes (`sig`, hashes) and where
/// the machine's state first came round (`exact`: from frame j it is the same at frame k):
/// - A state that comes round loops there, unless every frame of the loop writes the same (the song has stopped,
///   or holds its last chord: it ends). The state can come round only after many times through the music (a
///   counter the music doesn't hear): when the writes repeat with a period that divides the state's, from no
///   later, that loop is exact too.
/// - Otherwise the writes repeating to the end, at least twice, with a loop of at least `min` frames; or the same
///   writes every frame to the end, for `min` frames or more (it ends); or the frames played (`given`: as long
///   as the file says the track is, otherwise with a note).
pub(crate) fn ending(sig: &[u64], exact: Option<(usize, usize)>, min: usize, given: bool, warnings: &mut Vec<String>) -> (usize, Option<usize>) {
    let frames = sig.len();
    match exact {
        Some((j, k)) if sig[j..k].iter().all(|&h| h == sig[j]) => (k, None),
        Some((j, k)) => match repeat(sig, min) {
            Some((i, p)) if p < k - j && (k - j) % p == 0 && i <= j => (i + p, Some(i)),
            _ => (k, Some(j)),
        },
        None => match (repeat(sig, min), sig.iter().rposition(|&h| Some(&h) != sig.last()).map_or(0, |i| i + 1)) {
            (Some((j, p)), still) if j < still => (j + p, Some(j)),
            (_, still) if frames - still >= min => (still + 1, None),
            _ if given => (frames, None),
            _ => { warnings.push(format!("no loop found in the first {} minutes: converted that much", MAX_SECS / 60.0)); (frames, None) }
        },
    }
}

/// The song (from 0) a driver file plays: `track` (from 1), or the file's `first`.
pub(crate) fn pick_track(track: Option<usize>, songs: usize, first: usize) -> Result<usize, String> {
    match track {
        Some(t) if t == 0 || t > songs => Err(format!("no track {t}: the file has {songs} (1-{songs})")),
        Some(t) => Ok(t - 1),
        None => Ok(first),
    }
}

/// A played driver's log as a score, its silent loop dropped; an error if it plays nothing.
/// The frame as a whole number of samples (the NES's 60.1 Hz as 60, PAL's 50.007 as 50, the Game Boy's 59.73 as
/// 60), the song played that much off speed (0.16%, 0.45%): SiON's envelope tables step on whole samples, so the
/// driver's per-frame volume and pitch changes can be tables instead of a slur each frame (`Arranged::envelopes`).
/// Returns the new end and loop point. (A rate not within 0.5% of one that divides 44100 is kept.)
pub(crate) fn retime(writes: &mut [Write], end: u64, loop_at: Option<u64>, period: &mut Option<f64>) -> (u64, Option<u64>) {
    let whole = |p: f64| { let w = 44100.0 / (44100.0 / p).round(); (44100.0 % w == 0.0 && (p / w - 1.0).abs() < 0.005).then_some((p, w)) };
    let Some((p, w)) = period.and_then(whole) else { return (end, loop_at) };
    let r = w / p;
    for w in writes.iter_mut() { w.at = (w.at as f64 * r).round() as u64 }
    *period = Some(w);
    ((end as f64 * r).round() as u64, loop_at.map(|l| (l as f64 * r).round() as u64))
}

pub(crate) fn log_score(log: Log, warnings: Vec<String>, song: usize) -> Result<(Score, Vec<String>), String> {
    let (mut score, w) = crate::vgm::render(log, warnings)?;
    score.drop_silent_loop();
    if score.notes.is_empty() { return Err(format!("track {} plays no sound the converter can carry over", song + 1)) }
    Ok((score, w))
}

/// `track`: which song of the file (from 1; default: the file's own first song).
pub fn to_score(data: &[u8], exact: bool, exact_rate: bool, track: Option<usize>) -> Result<(Score, Vec<String>), String> {
    let f = parse(data)?;
    let mut warnings = vec![];
    let song = pick_track(track, f.songs, f.first)?;
    if f.songs > 1 {
        let name = f.labels.get(song).filter(|l| !l.is_empty()).map_or(String::new(), |l| format!(" ({l})"));
        warnings.push(format!("track {} of {}{name}; choose another with --track N", song + 1, f.songs));
    }
    // The region: a `regn` chunk's preferred one (or the first it lists), otherwise NTSC unless PAL-only.
    let region = match f.regn {
        Some((bits, pref)) => pref.filter(|&p| p < 3 && bits >> p & 1 != 0).or((0..3).find(|&r| bits >> r & 1 != 0)).unwrap_or(NTSC),
        None if f.timing & 3 == 1 => PAL,
        None => NTSC,
    };
    let clock = [1_789_773.0, 1_662_607.0, 1_773_448.0][region as usize];
    let us = match region {
        NTSC => if f.ntsc_us == 0 { 16639 } else { f.ntsc_us },
        PAL => if f.pal_us == 0 { 19997 } else { f.pal_us },
        _ => if f.dendy_us != 0 { f.dendy_us } else if f.pal_us != 0 { f.pal_us } else { 19997 },
    } as f64;
    if region == DENDY { warnings.push("played as on a Dendy (the file's preferred region)".into()) }
    let nsf2 = f.flags & (IRQ | NON_RETURNING) != 0;

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
        overlay: nsf2, vector: [0; 2], irqs: None, now: 0, quantum: (clock * us / 1e6 / 64.0).max(1.0) as u64,
    };
    if f.flags & IRQ != 0 {
        // (The frame counter's 4-step sequence: 29830 CPU cycles, 33254 on a PAL console.)
        nes.irqs = Some(Irqs { frame_period: if region == PAL { 33254 } else { 29830 }, pal: region == PAL, ..Irqs::default() });
    }
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
    // (The IRQ vector's RAM starts as the program has it.)
    nes.vector = [nes.mem[0xfffe - 0x5c00], nes.mem[0xffff - 0x5c00]];

    // The player's setup (the APU model starts cleared): channels enabled, the frame counter in 4-step mode.
    nes.log(0, 0, 0x15, 0x0f);
    nes.log(0, 0, 0x17, 0x40);
    let mut cpu = Cpu::new();
    // (Y: $80 tells a non-returning INIT that this first call is to return; otherwise 0.)
    let nonret = f.flags & NON_RETURNING != 0;
    (cpu.a, cpu.x, cpu.y, cpu.s) = (song as u8, region, if nonret { 0x80 } else { 0 }, 0xff);
    if !call(&mut cpu, &mut nes, f.init, clock as u64 * 10) { warnings.push("the INIT routine did not return (within 10 s)".into()) }
    if nonret {
        // INIT again (Y = $81), this time to run for good: PLAY interrupts it (if it returns, the player idles).
        (cpu.a, cpu.x, cpu.y) = (song as u8, region, 0x81);
        push_return(&mut cpu, &mut nes);
        cpu.pc = f.init;
    }
    let play = f.flags & NO_PLAY == 0;
    let mut jams = 0;

    // PLAY once per period, until the machine's state repeats (the song loops) or the time runs out.
    let period = clock * us / 1e6;
    let given = f.times.get(song).filter(|&&t| t > 0).map(|&t| (t + f.fades.get(song).copied().unwrap_or(0).max(0)) as f64 / 1000.0);
    let max = (given.unwrap_or(MAX_SECS) * 1e6 / us).ceil() as usize;
    // (Frames on a grid from time 0: the first after INIT has returned.)
    let t0 = ((cpu.cycles as f64 / period).ceil() * period) as u64;
    // (A frame's writes: from its PLAY to the next frame's, IRQs and a running INIT included.)
    let (mut seen, mut sig, mut starts, mut late) = (HashMap::new(), vec![], vec![], 0usize);
    let mut exact_loop = None;
    let at = |k: usize| (t0 as f64 + k as f64 * period).round() as u64;
    for k in 0..max {
        run(&mut cpu, &mut nes, at(k), &mut jams);
        if k > 0 { sig.push(nes.sig) }
        nes.now = cpu.cycles;
        if let Some(j) = seen.insert(nes.state(&cpu, nsf2), k) { exact_loop = Some((j, k)); break }
        starts.push(cpu.cycles);
        nes.sig = 0;
        let budget = period as u64 * 4;
        let done = match (play, nsf2) {
            (false, _) => true,
            (true, false) => call(&mut cpu, &mut nes, f.play, budget),
            (true, true) => nmi_play(&mut cpu, &mut nes, f.play, budget),
        };
        if !done { late += 1 }
    }
    if sig.len() < starts.len() { run(&mut cpu, &mut nes, at(starts.len()), &mut jams); sig.push(nes.sig) }
    if late > 0 { warnings.push(format!("the PLAY routine did not return within 4 frames {late} times")) }
    if jams > 0 { warnings.push(format!("the CPU jammed {jams} times (an unknown instruction)")) }
    let frames = starts.len();
    let (end_frame, lp) = ending(&sig, exact_loop, (1e6 / us) as usize, given.is_some(), &mut warnings);
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
    // (Without PLAY, the header's rate doesn't time the music: the driver's frames are found from the writes.)
    let (mut period, phase) = if play { (Some(44100.0 * us / 1e6), Some(0.0)) } else { (None, None) };
    let (end, loop_at) = if exact_rate { (end, loop_at) } else { retime(&mut writes, end, loop_at, &mut period) };
    let log = Log { chips, level, writes, blocks: nes.blocks, end, loop_at, period, phase, dac: None, shelf: false };
    log_score(log, warnings, song)
}
