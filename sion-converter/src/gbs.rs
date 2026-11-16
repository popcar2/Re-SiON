//! GBS (Game Boy sound) reader: runs the file's sound driver on the Game Boy's CPU (`sm83.rs`) the way a GBS
//! player does (INIT once with the song number, then PLAY at the vertical blank's 59.7 Hz, or at the timer's
//! rate when the header asks for it), logs what it writes to the APU, and converts that log as a VGM's
//! (`vgm::render`). Like an NSF, a GBS holds no song lengths: the song ends where the machine's state repeats
//! (its loop), or where the music repeats (`nsf::ending`).

use std::collections::HashMap;

use crate::bytes::{text, u16le};
use crate::chips::{Chip, GameBoy};
use crate::cpu::Bus;
use crate::nsf::{ending, log_score, mix, pick_track, retime, MAX_SECS};
use crate::score::Score;
use crate::sm83::Sm83;
use crate::vgm::{Log, Write};

pub fn detect(d: &[u8]) -> bool { d.len() >= 0x70 && d.starts_with(b"GBS\x01") }

/// The CPU clock (and a frame: 154 lines of 456 cycles).
const CLOCK: f64 = 4_194_304.0;
const VBLANK: f64 = 70224.0;

/// Where the player's code returns to (unusable memory): a routine has returned when the CPU gets there.
const SENTINEL: u16 = 0xfeb0;

struct Gbs {
    songs: usize,
    first: usize,
    load: u16,
    init: u16,
    play: u16,
    sp: u16,
    tma: u8,
    tac: u8,
    title: String,
    author: String,
    /// The program, as banks see it: from address 0 (zeros up to the load address).
    rom: Vec<u8>,
}

fn parse(d: &[u8]) -> Result<Gbs, String> {
    if !detect(d) { return Err("not a GBS file".into()) }
    let load = u16le(d, 6);
    if load < 0x400 || load >= 0x8000 { return Err(format!("GBS load address ${load:04X} is outside $0400-$7FFF")) }
    let mut rom = vec![0; load as usize];
    rom.extend_from_slice(&d[0x70..]);
    // (Whole banks, a power of two of them: bank numbers wrap.)
    rom.resize(rom.len().div_ceil(0x4000).max(2).next_power_of_two() * 0x4000, 0);
    let unknown = |s: String| if s == "?" { String::new() } else { s };
    Ok(Gbs {
        songs: d[4].max(1) as usize,
        first: d[5].max(1) as usize - 1,
        load,
        init: u16le(d, 8),
        play: u16le(d, 0x0a),
        sp: u16le(d, 0x0c),
        tma: d[0x0e],
        tac: d[0x0f],
        title: unknown(text(d, 0x10, 32)),
        author: unknown(text(d, 0x30, 32)),
        rom,
    })
}

/// The console as the driver sees it: the cartridge (bank 0, a switchable bank 1), RAM, the I/O registers; and
/// the log of APU writes.
struct Gb {
    rom: Vec<u8>,
    bank: usize,
    vram: Vec<u8>,
    /// $A000-$DFFF: cartridge RAM and work RAM.
    ram: Vec<u8>,
    /// $FF00-$FFFF: I/O registers and high RAM.
    io: [u8; 0x100],
    /// Whether the driver has written VRAM, or the cartridge RAM ($A000-$BFFF): left out of `state` till then.
    vram_used: bool,
    cart_used: bool,
    /// The CPU clock (doubled in double speed), and the cycle now (for LY).
    clock: f64,
    now: u64,
    /// CPU cycles a display line.
    line: u64,
    writes: Vec<Write>,
    sig: u64,
}

/// What reads of the APU's registers ($FF10-$FF26) give back: the write-only bits read as 1.
const READ_MASK: [u8; 0x17] = [
    0x80, 0x3f, 0x00, 0xff, 0xbf, 0xff, 0x3f, 0x00, 0xff, 0xbf, 0x7f, 0xff, 0x9f, 0xff, 0xbf, 0xff, 0xff, 0x00, 0x00, 0xbf,
    0x00, 0x00, 0x70,
];

impl Gb {
    fn log(&mut self, cycle: u64, reg: u8, val: u8) {
        self.sig = mix(mix(self.sig, reg as u64), val as u64);
        self.writes.push(Write::reg((cycle as f64 / self.clock * 44100.0).round() as u64, 0, 0, reg, val));
    }
    fn banked(&self, a: u16) -> u8 { self.rom[(self.bank * 0x4000 + (a as usize - 0x4000)) & (self.rom.len() - 1)] }
    /// The machine state the driver's future depends on.
    fn state(&self, cpu: &Sm83) -> u64 {
        let mut h = mix(cpu.sp as u64, self.bank as u64 | (self.io[6] as u64) << 16 | (self.io[7] as u64) << 24);
        let vram: &[u8] = if self.vram_used { &self.vram } else { &[] };
        let ram = if self.cart_used { &self.ram[..] } else { &self.ram[0x2000..] };
        for w in ram.chunks_exact(8).chain(self.io[0x80..].chunks_exact(8)).chain(vram.chunks_exact(8)) {
            h = mix(h, u64::from_le_bytes(w.try_into().unwrap()));
        }
        h
    }
}

impl Bus for Gb {
    fn read(&mut self, a: u16) -> u8 {
        match a {
            0..=0x3fff => self.rom[a as usize],
            0x4000..=0x7fff => self.banked(a),
            0x8000..=0x9fff => self.vram[a as usize - 0x8000],
            0xa000..=0xdfff => self.ram[a as usize - 0xa000],
            0xe000..=0xfdff => self.ram[a as usize - 0x2000 - 0xa000],
            0xfe00..=0xfeff => 0xff,
            // (LY: the line the display is on, which drivers may wait on.)
            0xff44 => (self.now / self.line % 154) as u8,
            0xff10..=0xff26 => self.io[a as usize & 0xff] | READ_MASK[a as usize - 0xff10],
            0xff27..=0xff2f => 0xff,
            _ => self.io[a as usize & 0xff],
        }
    }

    fn write(&mut self, a: u16, v: u8, cycle: u64) {
        match a {
            // (Bank 0 can't be selected in bank 1: ignored, as GBS players do.)
            0x2000..=0x3fff => if v != 0 { self.bank = v as usize },
            0x8000..=0x9fff => { self.vram[a as usize - 0x8000] = v; self.vram_used = true }
            0xa000..=0xbfff => { self.ram[a as usize - 0xa000] = v; self.cart_used = true }
            0xc000..=0xdfff => self.ram[a as usize - 0xa000] = v,
            0xe000..=0xfdff => self.ram[a as usize - 0x2000 - 0xa000] = v,
            0xff10..=0xff3f => { self.io[a as usize & 0xff] = v; self.log(cycle, (a - 0xff10) as u8, v) }
            0xff00..=0xffff => self.io[a as usize & 0xff] = v,
            _ => {}
        }
    }
}

/// Call a driver routine, as CALL from the player: until it returns, or `budget` cycles. Whether it returned.
fn call(cpu: &mut Sm83, gb: &mut Gb, addr: u16, budget: u64) -> bool {
    let sp = cpu.sp;
    cpu.push(gb, SENTINEL);
    cpu.pc = addr;
    let limit = cpu.cycles + budget;
    while cpu.pc != SENTINEL && !cpu.jammed && cpu.cycles < limit {
        gb.now = cpu.cycles;
        cpu.step(gb);
    }
    let done = cpu.pc == SENTINEL;
    if !done { cpu.sp = sp }
    cpu.jammed = false;
    done
}

/// `track`: which song of the file (from 1; default: the file's own first song).
pub fn to_score(data: &[u8], exact_rate: bool, track: Option<usize>) -> Result<(Score, Vec<String>), String> {
    let f = parse(data)?;
    let mut warnings = vec![];
    let song = pick_track(track, f.songs, f.first.min(f.songs - 1))?;
    if f.songs > 1 { warnings.push(format!("track {} of {}; choose another with --track N", song + 1, f.songs)) }

    // (TAC bit 7: the CPU in double speed, and the timer with it.)
    let double = f.tac & 0x80 != 0;
    let clock = if double { 2.0 * CLOCK } else { CLOCK };
    let mut gb = Gb {
        rom: f.rom, bank: 1, vram: vec![0; 0x2000], ram: vec![0; 0x4000], io: [0; 0x100], vram_used: false, cart_used: false,
        clock, now: 0, line: if double { 912 } else { 456 }, writes: vec![], sig: 0,
    };
    (gb.io[6], gb.io[7]) = (f.tma, f.tac);
    // PLAY's period (seconds): the timer's when the header asks for it (at the rate TAC and TMA set now: drivers
    // change TMA for their tempo), otherwise the vertical blank's.
    let period = |gb: &Gb| -> f64 {
        if f.tac & 4 == 0 { return VBLANK / CLOCK }
        let shift = [10, 4, 6, 8][gb.io[7] as usize & 3] - double as u32;
        ((256 - gb.io[6] as u32) << shift) as f64 / CLOCK
    };

    // The APU as a GBS player leaves it before INIT (powered on, the boot ROM's registers and wave RAM).
    const BOOT: [u8; 0x30] = [
        0x80, 0xbf, 0x00, 0x00, 0xbf, 0x00, 0x3f, 0x00, 0x00, 0xbf, 0x7f, 0xff, 0x9f, 0x00, 0xbf, 0x00, 0xff, 0x00, 0x00, 0xbf,
        0x77, 0xf3, 0xf1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0xac, 0xdd, 0xda, 0x48, 0x36, 0x02, 0xcf, 0x16, 0x2c, 0x04, 0xe5, 0x2c, 0xac, 0xdd, 0xda, 0x48,
    ];
    gb.write(0xff26, BOOT[0x16], 0);
    for (r, &v) in BOOT.iter().enumerate() { if r != 0x16 && !(0x17..0x20).contains(&r) { gb.write(0xff10 + r as u16, v, 0) } }

    let mut cpu = Sm83 { a: song as u8, sp: f.sp, rst_base: f.load, ..Sm83::default() };
    if !call(&mut cpu, &mut gb, f.init, clock as u64 * 10) { warnings.push("the INIT routine did not return (within 10 s)".into()) }

    // PLAY once per period, until the machine's state repeats (the song loops) or the time runs out.
    let p0 = period(&gb);
    let max = (MAX_SECS / p0).ceil() as usize;
    // (Frames on a grid from time 0: the first after INIT has returned.)
    let mut t = (cpu.cycles as f64 / clock / p0).ceil() * p0;
    let (mut seen, mut sig, mut starts, mut late, mut steady) = (HashMap::new(), vec![], vec![], 0usize, true);
    let mut exact_loop = None;
    for k in 0..max {
        if let Some(j) = seen.insert(gb.state(&cpu), k) { exact_loop = Some((j, k)); break }
        cpu.cycles = cpu.cycles.max((t * clock).round() as u64);
        starts.push(cpu.cycles);
        gb.sig = 0;
        if !call(&mut cpu, &mut gb, f.play, (clock * p0) as u64 * 4) { late += 1 }
        sig.push(gb.sig);
        let p = period(&gb);
        steady &= p == p0;
        t += p;
    }
    if late > 0 { warnings.push(format!("the PLAY routine did not return within 4 frames {late} times")) }
    let frames = starts.len();
    let (end_frame, lp) = ending(&sig, exact_loop, (1.0 / p0) as usize, false, &mut warnings);
    let end = ((if end_frame < frames { starts[end_frame] as f64 } else { t * clock }) / clock * 44100.0) as u64;
    let loop_at = lp.map(|j| (starts[j] as f64 / clock * 44100.0).round() as u64);

    let mut writes = std::mem::take(&mut gb.writes);
    writes.retain(|w| w.at < end);
    let chips: Vec<Box<dyn Chip>> = vec![Box::new(GameBoy::new())];
    // (A driver that changes its timer's rate has no one frame length: the frames are found from the writes.)
    let (mut period, phase) = if steady { (Some(44100.0 * p0), Some(0.0)) } else { (None, None) };
    let (end, loop_at) = if exact_rate { (end, loop_at) } else { retime(&mut writes, end, loop_at, &mut period) };
    let log = Log { chips, level: vec![1.0], writes, blocks: vec![], end, loop_at, period, phase, dac: None, shelf: false };
    let (mut score, w) = log_score(log, warnings, song)?;
    if score.title.is_empty() {
        score.title = if f.author.is_empty() { f.title } else { format!("{} ({})", f.title, f.author) };
    }
    Ok((score, w))
}
