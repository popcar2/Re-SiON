//! The Game Boy's CPU (Sharp SM83: a Z80-like 8080 descendant) for running GBS sound drivers: every
//! instruction, with cycle counts (T-states, taken branches included), so register writes land at the time they
//! would on the console. Interrupts aren't modelled (the GBS player calls the driver itself), so EI, DI, HALT
//! and STOP do nothing; RST calls the vector relative to the file's load address, as GBS players do.

use crate::cpu::Bus;

const Z: u8 = 0x80;
const N: u8 = 0x40;
const H: u8 = 0x20;
const C: u8 = 0x10;

#[derive(Default)]
pub struct Sm83 {
    pub a: u8, pub f: u8, pub b: u8, pub c: u8, pub d: u8, pub e: u8, pub h: u8, pub l: u8,
    pub sp: u16, pub pc: u16,
    pub cycles: u64,
    /// Where RST vectors point (the GBS load address).
    pub rst_base: u16,
    /// An undefined opcode was run.
    pub jammed: bool,
}

impl Sm83 {
    fn fetch(&mut self, bus: &mut impl Bus) -> u8 { let v = bus.read(self.pc); self.pc = self.pc.wrapping_add(1); v }
    fn fetch16(&mut self, bus: &mut impl Bus) -> u16 { let lo = self.fetch(bus) as u16; lo | (self.fetch(bus) as u16) << 8 }
    fn wr(&mut self, bus: &mut impl Bus, a: u16, v: u8) { bus.write(a, v, self.cycles) }
    pub fn push(&mut self, bus: &mut impl Bus, v: u16) {
        self.sp = self.sp.wrapping_sub(1);
        self.wr(bus, self.sp, (v >> 8) as u8);
        self.sp = self.sp.wrapping_sub(1);
        self.wr(bus, self.sp, v as u8);
    }
    fn pop(&mut self, bus: &mut impl Bus) -> u16 {
        let lo = bus.read(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        let hi = bus.read(self.sp) as u16;
        self.sp = self.sp.wrapping_add(1);
        lo | hi << 8
    }

    fn hl(&self) -> u16 { (self.h as u16) << 8 | self.l as u16 }
    fn set_hl(&mut self, v: u16) { (self.h, self.l) = ((v >> 8) as u8, v as u8) }
    /// Register pair `i` (BC, DE, HL, SP).
    fn rr(&self, i: u8) -> u16 {
        match i { 0 => (self.b as u16) << 8 | self.c as u16, 1 => (self.d as u16) << 8 | self.e as u16, 2 => self.hl(), _ => self.sp }
    }
    fn set_rr(&mut self, i: u8, v: u16) {
        let (hi, lo) = ((v >> 8) as u8, v as u8);
        match i { 0 => (self.b, self.c) = (hi, lo), 1 => (self.d, self.e) = (hi, lo), 2 => (self.h, self.l) = (hi, lo), _ => self.sp = v }
    }
    /// Register `i` (B, C, D, E, H, L, (HL), A).
    fn r(&mut self, bus: &mut impl Bus, i: u8) -> u8 {
        match i { 0 => self.b, 1 => self.c, 2 => self.d, 3 => self.e, 4 => self.h, 5 => self.l, 6 => bus.read(self.hl()), _ => self.a }
    }
    fn set_r(&mut self, bus: &mut impl Bus, i: u8, v: u8) {
        match i { 0 => self.b = v, 1 => self.c = v, 2 => self.d = v, 3 => self.e = v, 4 => self.h = v, 5 => self.l = v, 6 => self.wr(bus, self.hl(), v), _ => self.a = v }
    }
    fn flags(&mut self, z: bool, n: bool, h: bool, c: bool) { self.f = (z as u8) << 7 | (n as u8) << 6 | (h as u8) << 5 | (c as u8) << 4 }
    fn carry(&self) -> u8 { (self.f & C != 0) as u8 }
    /// Condition `i` (NZ, Z, NC, C).
    fn cond(&self, i: u8) -> bool { match i { 0 => self.f & Z == 0, 1 => self.f & Z != 0, 2 => self.f & C == 0, _ => self.f & C != 0 } }

    /// ALU operation `op` (ADD, ADC, SUB, SBC, AND, XOR, OR, CP) on A and `v`.
    fn alu(&mut self, op: u8, v: u8) {
        let (a, c) = (self.a, self.carry());
        match op {
            0 | 1 => {
                let c = if op == 1 { c } else { 0 };
                let r = a as u16 + v as u16 + c as u16;
                self.flags(r as u8 == 0, false, (a & 15) + (v & 15) + c > 15, r > 0xff);
                self.a = r as u8;
            }
            2 | 3 | 7 => {
                let c = if op == 3 { c } else { 0 };
                let r = a.wrapping_sub(v).wrapping_sub(c);
                self.flags(r == 0, true, (a & 15) < (v & 15) + c, (a as u16) < v as u16 + c as u16);
                if op != 7 { self.a = r }
            }
            4 => { self.a &= v; self.flags(self.a == 0, false, true, false) }
            5 => { self.a ^= v; self.flags(self.a == 0, false, false, false) }
            _ => { self.a |= v; self.flags(self.a == 0, false, false, false) }
        }
    }

    /// The CB-prefixed rotate/shift `op` (RLC, RRC, RL, RR, SLA, SRA, SWAP, SRL) of `v`, flags set.
    fn shift(&mut self, op: u8, v: u8) -> u8 {
        let c = self.carry();
        let (r, out) = match op {
            0 => (v.rotate_left(1), v >> 7),
            1 => (v.rotate_right(1), v & 1),
            2 => (v << 1 | c, v >> 7),
            3 => (v >> 1 | c << 7, v & 1),
            4 => (v << 1, v >> 7),
            5 => ((v as i8 >> 1) as u8, v & 1),
            6 => (v.rotate_left(4), 0),
            _ => (v >> 1, v & 1),
        };
        self.flags(r == 0, false, false, out != 0);
        r
    }

    /// SP plus a signed byte, with the flags ADD SP,e and LD HL,SP+e set (from the low byte's unsigned add).
    fn sp_plus(&mut self, bus: &mut impl Bus) -> u16 {
        let e = self.fetch(bus);
        let sp = self.sp;
        self.flags(false, false, (sp & 15) + (e as u16 & 15) > 15, (sp & 0xff) + e as u16 > 0xff);
        sp.wrapping_add(e as i8 as u16)
    }

    /// Run one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) {
        let op = self.fetch(bus);
        let (y, z) = (op >> 3 & 7, op & 7);
        let mut t = 4u64;
        match op {
            0x00 | 0x10 | 0x76 | 0xf3 | 0xfb => { if op == 0x10 { self.fetch(bus); } }
            0x01 | 0x11 | 0x21 | 0x31 => { let v = self.fetch16(bus); self.set_rr(y >> 1, v); t = 12 }
            0x02 | 0x12 | 0x22 | 0x32 => {
                let a = match op { 0x02 => self.rr(0), 0x12 => self.rr(1), _ => self.hl() };
                self.wr(bus, a, self.a);
                if op == 0x22 { self.set_hl(a.wrapping_add(1)) } else if op == 0x32 { self.set_hl(a.wrapping_sub(1)) }
                t = 8;
            }
            0x0a | 0x1a | 0x2a | 0x3a => {
                let a = match op { 0x0a => self.rr(0), 0x1a => self.rr(1), _ => self.hl() };
                self.a = bus.read(a);
                if op == 0x2a { self.set_hl(a.wrapping_add(1)) } else if op == 0x3a { self.set_hl(a.wrapping_sub(1)) }
                t = 8;
            }
            0x03 | 0x13 | 0x23 | 0x33 => { let v = self.rr(y >> 1).wrapping_add(1); self.set_rr(y >> 1, v); t = 8 }
            0x0b | 0x1b | 0x2b | 0x3b => { let v = self.rr(y >> 1).wrapping_sub(1); self.set_rr(y >> 1, v); t = 8 }
            0x04 | 0x0c | 0x14 | 0x1c | 0x24 | 0x2c | 0x34 | 0x3c => {
                let v = self.r(bus, y);
                let r = v.wrapping_add(1);
                self.f = self.f & C | if r == 0 { Z } else { 0 } | if v & 15 == 15 { H } else { 0 };
                self.set_r(bus, y, r);
                if y == 6 { t = 12 }
            }
            0x05 | 0x0d | 0x15 | 0x1d | 0x25 | 0x2d | 0x35 | 0x3d => {
                let v = self.r(bus, y);
                let r = v.wrapping_sub(1);
                self.f = self.f & C | N | if r == 0 { Z } else { 0 } | if v & 15 == 0 { H } else { 0 };
                self.set_r(bus, y, r);
                if y == 6 { t = 12 }
            }
            0x06 | 0x0e | 0x16 | 0x1e | 0x26 | 0x2e | 0x36 | 0x3e => { let v = self.fetch(bus); self.set_r(bus, y, v); t = if y == 6 { 12 } else { 8 } }
            0x07 | 0x0f | 0x17 | 0x1f => { let v = self.shift(y, self.a); self.a = v; self.f &= C }
            0x08 => { let a = self.fetch16(bus); self.wr(bus, a, self.sp as u8); self.wr(bus, a.wrapping_add(1), (self.sp >> 8) as u8); t = 20 }
            0x09 | 0x19 | 0x29 | 0x39 => {
                let (hl, v) = (self.hl(), self.rr(y >> 1));
                self.f = self.f & Z | if (hl & 0xfff) + (v & 0xfff) > 0xfff { H } else { 0 } | if hl as u32 + v as u32 > 0xffff { C } else { 0 };
                self.set_hl(hl.wrapping_add(v));
                t = 8;
            }
            0x18 | 0x20 | 0x28 | 0x30 | 0x38 => {
                let e = self.fetch(bus) as i8;
                t = 8;
                if op == 0x18 || self.cond(y - 4) { self.pc = self.pc.wrapping_add(e as u16); t = 12 }
            }
            0x27 => {
                let (mut a, mut c) = (self.a, self.f & C != 0);
                if self.f & N == 0 {
                    if c || a > 0x99 { a = a.wrapping_add(0x60); c = true }
                    if self.f & H != 0 || a & 15 > 9 { a = a.wrapping_add(6) }
                } else {
                    if c { a = a.wrapping_sub(0x60) }
                    if self.f & H != 0 { a = a.wrapping_sub(6) }
                }
                self.a = a;
                self.f = self.f & N | if a == 0 { Z } else { 0 } | if c { C } else { 0 };
            }
            0x2f => { self.a = !self.a; self.f |= N | H }
            0x37 => self.f = self.f & Z | C,
            0x3f => self.f = self.f & (Z | C) ^ C,
            0x40..=0x7f => { let v = self.r(bus, z); self.set_r(bus, y, v); if y == 6 || z == 6 { t = 8 } }
            0x80..=0xbf => { let v = self.r(bus, z); self.alu(y, v); if z == 6 { t = 8 } }
            0xc6 | 0xce | 0xd6 | 0xde | 0xe6 | 0xee | 0xf6 | 0xfe => { let v = self.fetch(bus); self.alu(y, v); t = 8 }
            0xc0 | 0xc8 | 0xd0 | 0xd8 => { t = 8; if self.cond(y) { self.pc = self.pop(bus); t = 20 } }
            0xc9 | 0xd9 => { self.pc = self.pop(bus); t = 16 }
            0xc1 | 0xd1 | 0xe1 | 0xf1 => {
                let v = self.pop(bus);
                if op == 0xf1 { (self.a, self.f) = ((v >> 8) as u8, v as u8 & 0xf0) } else { self.set_rr(y >> 1, v) }
                t = 12;
            }
            0xc5 | 0xd5 | 0xe5 | 0xf5 => {
                let v = if op == 0xf5 { (self.a as u16) << 8 | self.f as u16 } else { self.rr(y >> 1) };
                self.push(bus, v);
                t = 16;
            }
            0xc2 | 0xca | 0xd2 | 0xda | 0xc3 => {
                let a = self.fetch16(bus);
                t = 12;
                if op == 0xc3 || self.cond(y) { self.pc = a; t = 16 }
            }
            0xe9 => self.pc = self.hl(),
            0xc4 | 0xcc | 0xd4 | 0xdc | 0xcd => {
                let a = self.fetch16(bus);
                t = 12;
                if op == 0xcd || self.cond(y) { self.push(bus, self.pc); self.pc = a; t = 24 }
            }
            0xc7 | 0xcf | 0xd7 | 0xdf | 0xe7 | 0xef | 0xf7 | 0xff => { self.push(bus, self.pc); self.pc = self.rst_base.wrapping_add(y as u16 * 8); t = 16 }
            0xe0 => { let a = 0xff00 | self.fetch(bus) as u16; self.wr(bus, a, self.a); t = 12 }
            0xf0 => { let a = 0xff00 | self.fetch(bus) as u16; self.a = bus.read(a); t = 12 }
            0xe2 => { self.wr(bus, 0xff00 | self.c as u16, self.a); t = 8 }
            0xf2 => { self.a = bus.read(0xff00 | self.c as u16); t = 8 }
            0xea => { let a = self.fetch16(bus); self.wr(bus, a, self.a); t = 16 }
            0xfa => { let a = self.fetch16(bus); self.a = bus.read(a); t = 16 }
            0xe8 => { self.sp = self.sp_plus(bus); t = 16 }
            0xf8 => { let v = self.sp_plus(bus); self.set_hl(v); t = 12 }
            0xf9 => { self.sp = self.hl(); t = 8 }
            0xcb => {
                let op = self.fetch(bus);
                let (x, y, z) = (op >> 6, op >> 3 & 7, op & 7);
                let v = self.r(bus, z);
                t = if z == 6 { if x == 1 { 12 } else { 16 } } else { 8 };
                match x {
                    0 => { let r = self.shift(y, v); self.set_r(bus, z, r) }
                    1 => self.f = self.f & C | H | if v >> y & 1 == 0 { Z } else { 0 },
                    2 => self.set_r(bus, z, v & !(1 << y)),
                    _ => self.set_r(bus, z, v | 1 << y),
                }
            }
            // D3 DB DD E3 E4 EB EC ED F4 FC FD: undefined (the CPU locks up).
            _ => { self.jammed = true; self.pc = self.pc.wrapping_sub(1) }
        }
        self.cycles += t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cpu::Ram;

    fn run(code: &[u8], steps: usize) -> (Sm83, Ram) {
        let mut ram = Ram(vec![0; 0x10000]);
        ram.0[0x100..0x100 + code.len()].copy_from_slice(code);
        let mut cpu = Sm83 { pc: 0x100, sp: 0xfffe, ..Sm83::default() };
        for _ in 0..steps { cpu.step(&mut ram) }
        (cpu, ram)
    }

    #[test]
    fn arithmetic_and_flags() {
        // LD A,$3A; ADD A,$C6 → 0, Z H C.
        let (c, _) = run(&[0x3e, 0x3a, 0xc6, 0xc6], 2);
        assert_eq!((c.a, c.f), (0, Z | H | C));
        // LD A,$15; LD B,$27; ADD A,B; DAA → $42 (BCD 15 + 27).
        let (c, _) = run(&[0x3e, 0x15, 0x06, 0x27, 0x80, 0x27], 4);
        assert_eq!(c.a, 0x42);
        // LD A,$10; SUB $01 → $0F, N H.
        let (c, _) = run(&[0x3e, 0x10, 0xd6, 0x01], 2);
        assert_eq!((c.a, c.f), (0x0f, N | H));
        // LD SP,$FFF8; LD HL,SP+2 → $FFFA, no flags ($8 + $2: no half carry; $F8 + $02: no carry).
        let (c, _) = run(&[0x31, 0xf8, 0xff, 0xf8, 0x02], 2);
        assert_eq!((c.hl(), c.f), (0xfffa, 0));
        // LD A,$80; RLCA → $01, C (Z clear); SWAP via CB: LD B,$F1; SWAP B → $1F.
        let (c, _) = run(&[0x3e, 0x80, 0x07, 0x06, 0xf1, 0xcb, 0x30], 4);
        assert_eq!((c.a, c.b, c.f), (0x01, 0x1f, 0));
        // BIT 7,H with H = 0 → Z H.
        let (c, _) = run(&[0x26, 0x00, 0xcb, 0x7c], 2);
        assert_eq!(c.f & (Z | H | N), Z | H);
    }

    #[test]
    fn calls_loads_and_cycles() {
        // LD HL,$C000; LD (HL+),A ×2 with A = 7; CALL $0200 (RET there); LDH ($80),A.
        let mut code = vec![0x3e, 0x07, 0x21, 0x00, 0xc0, 0x22, 0x22, 0xcd, 0x00, 0x02, 0xe0, 0x80];
        code.resize(0x100, 0);
        code.push(0xc9); // $0200: RET
        let (c, ram) = run(&code, 7);
        assert_eq!((ram.0[0xc000], ram.0[0xc001], ram.0[0xff80]), (7, 7, 7));
        assert_eq!((c.hl(), c.sp, c.pc), (0xc002, 0xfffe, 0x10c));
        assert_eq!(c.cycles, 8 + 12 + 8 + 8 + 24 + 16 + 12);
        // DEC B; JR NZ: a loop counting B down.
        let (c, _) = run(&[0x06, 0x03, 0x05, 0x20, 0xfd], 1 + 2 * 3);
        assert_eq!((c.b, c.pc), (0, 0x105));
    }

    /// Blargg's `cpu_instrs` test ROMs (the individual ones, not included): `BLARGG=dir cargo test --release --lib
    /// -- --ignored blargg`. Each prints its result on the serial port; 02 (interrupts) is skipped, as interrupts
    /// aren't modelled.
    #[test]
    #[ignore]
    fn blargg() {
        struct Gb { mem: Vec<u8>, out: String, cycles: u64 }
        impl Bus for Gb {
            fn read(&mut self, a: u16) -> u8 { if a == 0xff44 { (self.cycles / 456 % 154) as u8 } else { self.mem[a as usize] } }
            fn write(&mut self, a: u16, v: u8, _: u64) {
                if a < 0x8000 { return }
                if a == 0xff02 && v == 0x81 { self.out.push(self.mem[0xff01] as char) }
                self.mem[a as usize] = v;
            }
        }
        let dir = std::env::var("BLARGG").expect("BLARGG=directory of the cpu_instrs individual ROMs");
        let mut roms: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "gb")).collect();
        roms.sort();
        let mut failed = vec![];
        for rom in roms {
            let name = rom.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("02") { continue }
            let mut gb = Gb { mem: vec![0; 0x10000], out: String::new(), cycles: 0 };
            let data = std::fs::read(&rom).unwrap();
            gb.mem[..data.len().min(0x8000)].copy_from_slice(&data[..data.len().min(0x8000)]);
            let mut cpu = Sm83 { pc: 0x100, sp: 0xfffe, a: 0x01, ..Sm83::default() };
            while cpu.cycles < 4_194_304 * 120 && !gb.out.contains("Passed") && !gb.out.contains("Failed") && !cpu.jammed {
                gb.cycles = cpu.cycles;
                cpu.step(&mut gb);
            }
            println!("{name}: {}", gb.out.trim().replace('\n', " | "));
            if !gb.out.contains("Passed") { failed.push(name) }
        }
        assert!(failed.is_empty(), "failed: {failed:?}");
    }
}
