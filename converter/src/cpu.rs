//! A 6502 (the NES's 2A03 core, without decimal mode) for running NSF sound drivers: every documented
//! instruction and the stable undocumented ones, with cycle counts (page crossings and taken branches included),
//! so register writes land at the time they would on the console.

pub trait Bus {
    fn read(&mut self, addr: u16) -> u8;
    /// `cycle`: the CPU cycle the write happens on.
    fn write(&mut self, addr: u16, val: u8, cycle: u64);
}

#[derive(Default)]
pub struct Cpu { pub a: u8, pub x: u8, pub y: u8, pub s: u8, pub p: u8, pub pc: u16, pub cycles: u64, pub jammed: bool }

#[derive(Clone, Copy, PartialEq)]
enum M { Imp, Acc, Imm, Zp, Zpx, Zpy, Abs, Abx, Aby, Izx, Izy, Ind, Rel }

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Adc, And, Asl, Bcc, Bcs, Beq, Bit, Bmi, Bne, Bpl, Brk, Bvc, Bvs, Clc, Cld, Cli, Clv, Cmp, Cpx, Cpy, Dec, Dex, Dey,
    Eor, Inc, Inx, Iny, Jmp, Jsr, Lda, Ldx, Ldy, Lsr, Nop, Ora, Pha, Php, Pla, Plp, Rol, Ror, Rti, Rts, Sbc, Sec,
    Sed, Sei, Sta, Stx, Sty, Tax, Tay, Tsx, Txa, Txs, Tya,
    // Undocumented.
    Slo, Rla, Sre, Rra, Sax, Lax, Dcp, Isc, Anc, Alr, Arr, Ane, Lxa, Sbx, Sha, Shx, Shy, Tas, Las, Jam,
}

use M::*;
use Op::*;

#[rustfmt::skip]
const TABLE: [(Op, M); 256] = [
    (Brk,Imp),(Ora,Izx),(Jam,Imp),(Slo,Izx),(Nop,Zp ),(Ora,Zp ),(Asl,Zp ),(Slo,Zp ),(Php,Imp),(Ora,Imm),(Asl,Acc),(Anc,Imm),(Nop,Abs),(Ora,Abs),(Asl,Abs),(Slo,Abs),
    (Bpl,Rel),(Ora,Izy),(Jam,Imp),(Slo,Izy),(Nop,Zpx),(Ora,Zpx),(Asl,Zpx),(Slo,Zpx),(Clc,Imp),(Ora,Aby),(Nop,Imp),(Slo,Aby),(Nop,Abx),(Ora,Abx),(Asl,Abx),(Slo,Abx),
    (Jsr,Abs),(And,Izx),(Jam,Imp),(Rla,Izx),(Bit,Zp ),(And,Zp ),(Rol,Zp ),(Rla,Zp ),(Plp,Imp),(And,Imm),(Rol,Acc),(Anc,Imm),(Bit,Abs),(And,Abs),(Rol,Abs),(Rla,Abs),
    (Bmi,Rel),(And,Izy),(Jam,Imp),(Rla,Izy),(Nop,Zpx),(And,Zpx),(Rol,Zpx),(Rla,Zpx),(Sec,Imp),(And,Aby),(Nop,Imp),(Rla,Aby),(Nop,Abx),(And,Abx),(Rol,Abx),(Rla,Abx),
    (Rti,Imp),(Eor,Izx),(Jam,Imp),(Sre,Izx),(Nop,Zp ),(Eor,Zp ),(Lsr,Zp ),(Sre,Zp ),(Pha,Imp),(Eor,Imm),(Lsr,Acc),(Alr,Imm),(Jmp,Abs),(Eor,Abs),(Lsr,Abs),(Sre,Abs),
    (Bvc,Rel),(Eor,Izy),(Jam,Imp),(Sre,Izy),(Nop,Zpx),(Eor,Zpx),(Lsr,Zpx),(Sre,Zpx),(Cli,Imp),(Eor,Aby),(Nop,Imp),(Sre,Aby),(Nop,Abx),(Eor,Abx),(Lsr,Abx),(Sre,Abx),
    (Rts,Imp),(Adc,Izx),(Jam,Imp),(Rra,Izx),(Nop,Zp ),(Adc,Zp ),(Ror,Zp ),(Rra,Zp ),(Pla,Imp),(Adc,Imm),(Ror,Acc),(Arr,Imm),(Jmp,Ind),(Adc,Abs),(Ror,Abs),(Rra,Abs),
    (Bvs,Rel),(Adc,Izy),(Jam,Imp),(Rra,Izy),(Nop,Zpx),(Adc,Zpx),(Ror,Zpx),(Rra,Zpx),(Sei,Imp),(Adc,Aby),(Nop,Imp),(Rra,Aby),(Nop,Abx),(Adc,Abx),(Ror,Abx),(Rra,Abx),
    (Nop,Imm),(Sta,Izx),(Nop,Imm),(Sax,Izx),(Sty,Zp ),(Sta,Zp ),(Stx,Zp ),(Sax,Zp ),(Dey,Imp),(Nop,Imm),(Txa,Imp),(Ane,Imm),(Sty,Abs),(Sta,Abs),(Stx,Abs),(Sax,Abs),
    (Bcc,Rel),(Sta,Izy),(Jam,Imp),(Sha,Izy),(Sty,Zpx),(Sta,Zpx),(Stx,Zpy),(Sax,Zpy),(Tya,Imp),(Sta,Aby),(Txs,Imp),(Tas,Aby),(Shy,Abx),(Sta,Abx),(Shx,Aby),(Sha,Aby),
    (Ldy,Imm),(Lda,Izx),(Ldx,Imm),(Lax,Izx),(Ldy,Zp ),(Lda,Zp ),(Ldx,Zp ),(Lax,Zp ),(Tay,Imp),(Lda,Imm),(Tax,Imp),(Lxa,Imm),(Ldy,Abs),(Lda,Abs),(Ldx,Abs),(Lax,Abs),
    (Bcs,Rel),(Lda,Izy),(Jam,Imp),(Lax,Izy),(Ldy,Zpx),(Lda,Zpx),(Ldx,Zpy),(Lax,Zpy),(Clv,Imp),(Lda,Aby),(Tsx,Imp),(Las,Aby),(Ldy,Abx),(Lda,Abx),(Ldx,Aby),(Lax,Aby),
    (Cpy,Imm),(Cmp,Izx),(Nop,Imm),(Dcp,Izx),(Cpy,Zp ),(Cmp,Zp ),(Dec,Zp ),(Dcp,Zp ),(Iny,Imp),(Cmp,Imm),(Dex,Imp),(Sbx,Imm),(Cpy,Abs),(Cmp,Abs),(Dec,Abs),(Dcp,Abs),
    (Bne,Rel),(Cmp,Izy),(Jam,Imp),(Dcp,Izy),(Nop,Zpx),(Cmp,Zpx),(Dec,Zpx),(Dcp,Zpx),(Cld,Imp),(Cmp,Aby),(Nop,Imp),(Dcp,Aby),(Nop,Abx),(Cmp,Abx),(Dec,Abx),(Dcp,Abx),
    (Cpx,Imm),(Sbc,Izx),(Nop,Imm),(Isc,Izx),(Cpx,Zp ),(Sbc,Zp ),(Inc,Zp ),(Isc,Zp ),(Inx,Imp),(Sbc,Imm),(Nop,Imp),(Sbc,Imm),(Cpx,Abs),(Sbc,Abs),(Inc,Abs),(Isc,Abs),
    (Beq,Rel),(Sbc,Izy),(Jam,Imp),(Isc,Izy),(Nop,Zpx),(Sbc,Zpx),(Inc,Zpx),(Isc,Zpx),(Sed,Imp),(Sbc,Aby),(Nop,Imp),(Isc,Aby),(Nop,Abx),(Sbc,Abx),(Inc,Abx),(Isc,Abx),
];

/// Base cycles per opcode (page crossings and branches add theirs).
#[rustfmt::skip]
const CYCLES: [u8; 256] = [
    7,6,2,8,3,3,5,5,3,2,2,2,4,4,6,6, 2,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    6,6,2,8,3,3,5,5,4,2,2,2,4,4,6,6, 2,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    6,6,2,8,3,3,5,5,3,2,2,2,3,4,6,6, 2,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    6,6,2,8,3,3,5,5,4,2,2,2,5,4,6,6, 2,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    2,6,2,6,3,3,3,3,2,2,2,2,4,4,4,4, 2,6,2,6,4,4,4,4,2,5,2,5,5,5,5,5,
    2,6,2,6,3,3,3,3,2,2,2,2,4,4,4,4, 2,5,2,5,4,4,4,4,2,4,2,4,4,4,4,4,
    2,6,2,8,3,3,5,5,2,2,2,2,4,4,6,6, 2,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
    2,6,2,8,3,3,5,5,2,2,2,2,4,4,6,6, 2,5,2,8,4,4,6,6,2,4,2,7,4,4,7,7,
];

const C: u8 = 1;
const Z: u8 = 2;
const I: u8 = 4;
const D: u8 = 8;
const B: u8 = 0x10;
const U: u8 = 0x20;
const V: u8 = 0x40;
const N: u8 = 0x80;

impl Cpu {
    pub fn new() -> Self { Cpu { s: 0xfd, p: I | U, ..Cpu::default() } }

    fn fetch(&mut self, bus: &mut impl Bus) -> u8 { let v = bus.read(self.pc); self.pc = self.pc.wrapping_add(1); v }
    fn fetch16(&mut self, bus: &mut impl Bus) -> u16 { let lo = self.fetch(bus) as u16; lo | (self.fetch(bus) as u16) << 8 }
    pub fn push(&mut self, bus: &mut impl Bus, v: u8) { bus.write(0x100 | self.s as u16, v, self.cycles); self.s = self.s.wrapping_sub(1) }
    fn pull(&mut self, bus: &mut impl Bus) -> u8 { self.s = self.s.wrapping_add(1); bus.read(0x100 | self.s as u16) }
    fn nz(&mut self, v: u8) -> u8 { self.p = (self.p & !(N | Z)) | (v & N) | if v == 0 { Z } else { 0 }; v }
    fn flag(&mut self, f: u8, on: bool) { if on { self.p |= f } else { self.p &= !f } }
    /// A zero-page pointer (wrapping within the page).
    fn ptr(bus: &mut impl Bus, zp: u8) -> u16 { bus.read(zp as u16) as u16 | (bus.read(zp.wrapping_add(1) as u16) as u16) << 8 }

    /// The operand's address, and whether indexing crossed a page.
    fn addr(&mut self, bus: &mut impl Bus, m: M) -> (u16, bool) {
        let idx = |base: u16, i: u8| { let a = base.wrapping_add(i as u16); (a, a & 0xff00 != base & 0xff00) };
        match m {
            Imm => { let a = self.pc; self.pc = self.pc.wrapping_add(1); (a, false) }
            Zp => (self.fetch(bus) as u16, false),
            Zpx => (self.fetch(bus).wrapping_add(self.x) as u16, false),
            Zpy => (self.fetch(bus).wrapping_add(self.y) as u16, false),
            Abs => (self.fetch16(bus), false),
            Abx => { let b = self.fetch16(bus); idx(b, self.x) }
            Aby => { let b = self.fetch16(bus); idx(b, self.y) }
            Izx => { let z = self.fetch(bus).wrapping_add(self.x); (Self::ptr(bus, z), false) }
            Izy => { let z = self.fetch(bus); let b = Self::ptr(bus, z); idx(b, self.y) }
            // (The 6502's JMP ($xxFF) reads its high byte from $xx00.)
            Ind => { let p = self.fetch16(bus); (bus.read(p) as u16 | (bus.read((p & 0xff00) | (p.wrapping_add(1) & 0xff)) as u16) << 8, false) }
            Rel => { let o = self.fetch(bus) as i8; (self.pc.wrapping_add(o as u16), false) }
            Imp | Acc => (0, false),
        }
    }

    fn adc(&mut self, v: u8) {
        let s = self.a as u16 + v as u16 + (self.p & C) as u16;
        self.flag(V, (!(self.a ^ v) & (self.a ^ s as u8)) & 0x80 != 0);
        self.flag(C, s > 0xff);
        self.a = self.nz(s as u8);
    }
    fn cmp(&mut self, r: u8, v: u8) { self.flag(C, r >= v); self.nz(r.wrapping_sub(v)); }
    fn asl(&mut self, v: u8) -> u8 { self.flag(C, v & 0x80 != 0); self.nz(v << 1) }
    fn lsr(&mut self, v: u8) -> u8 { self.flag(C, v & 1 != 0); self.nz(v >> 1) }
    fn rol(&mut self, v: u8) -> u8 { let c = self.p & C; self.flag(C, v & 0x80 != 0); self.nz(v << 1 | c) }
    fn ror(&mut self, v: u8) -> u8 { let c = self.p & C; self.flag(C, v & 1 != 0); self.nz(v >> 1 | c << 7) }

    /// Run one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) {
        let code = self.fetch(bus);
        let (op, m) = TABLE[code as usize];
        self.cycles += CYCLES[code as usize] as u64;
        let (ea, crossed) = self.addr(bus, m);
        // Reads that cross a page take a cycle more (stores and read-modify-writes always take it).
        if crossed && matches!(op, Lda | Ldx | Ldy | Eor | And | Ora | Adc | Sbc | Cmp | Lax | Las | Nop) { self.cycles += 1 }
        let at = self.cycles;
        macro_rules! val { () => { if m == Acc { self.a } else { bus.read(ea) } } }
        macro_rules! put { ($v:expr) => {{ let v = $v; if m == Acc { self.a = v } else { bus.write(ea, v, at) } }} }
        macro_rules! branch { ($cond:expr) => { if $cond { self.cycles += 1 + ((ea & 0xff00) != (self.pc & 0xff00)) as u64; self.pc = ea } } }
        match op {
            Lda => { let v = val!(); self.a = self.nz(v) }
            Ldx => { let v = val!(); self.x = self.nz(v) }
            Ldy => { let v = val!(); self.y = self.nz(v) }
            Sta => bus.write(ea, self.a, at),
            Stx => bus.write(ea, self.x, at),
            Sty => bus.write(ea, self.y, at),
            Tax => self.x = self.nz(self.a),
            Tay => self.y = self.nz(self.a),
            Txa => self.a = self.nz(self.x),
            Tya => self.a = self.nz(self.y),
            Tsx => self.x = self.nz(self.s),
            Txs => self.s = self.x,
            Pha => self.push(bus, self.a),
            Php => self.push(bus, self.p | B | U),
            Pla => { let v = self.pull(bus); self.a = self.nz(v) }
            Plp => self.p = self.pull(bus) & !B | U,
            And => { let v = val!(); self.a = self.nz(self.a & v) }
            Ora => { let v = val!(); self.a = self.nz(self.a | v) }
            Eor => { let v = val!(); self.a = self.nz(self.a ^ v) }
            Adc => { let v = val!(); self.adc(v) }
            Sbc => { let v = val!(); self.adc(!v) }
            Cmp => { let v = val!(); self.cmp(self.a, v) }
            Cpx => { let v = val!(); self.cmp(self.x, v) }
            Cpy => { let v = val!(); self.cmp(self.y, v) }
            Bit => { let v = val!(); self.flag(Z, self.a & v == 0); self.p = (self.p & !(N | V)) | (v & (N | V)) }
            Inc => { let v = val!().wrapping_add(1); put!(self.nz(v)) }
            Dec => { let v = val!().wrapping_sub(1); put!(self.nz(v)) }
            Inx => self.x = self.nz(self.x.wrapping_add(1)),
            Iny => self.y = self.nz(self.y.wrapping_add(1)),
            Dex => self.x = self.nz(self.x.wrapping_sub(1)),
            Dey => self.y = self.nz(self.y.wrapping_sub(1)),
            Asl => { let v = val!(); put!(self.asl(v)) }
            Lsr => { let v = val!(); put!(self.lsr(v)) }
            Rol => { let v = val!(); put!(self.rol(v)) }
            Ror => { let v = val!(); put!(self.ror(v)) }
            Jmp => self.pc = ea,
            Jsr => { let r = self.pc.wrapping_sub(1); self.push(bus, (r >> 8) as u8); self.push(bus, r as u8); self.pc = ea }
            Rts => { let lo = self.pull(bus) as u16; let hi = self.pull(bus) as u16; self.pc = (lo | hi << 8).wrapping_add(1) }
            Rti => { self.p = self.pull(bus) & !B | U; let lo = self.pull(bus) as u16; let hi = self.pull(bus) as u16; self.pc = lo | hi << 8 }
            Brk => {
                let r = self.pc.wrapping_add(1);
                self.push(bus, (r >> 8) as u8);
                self.push(bus, r as u8);
                self.push(bus, self.p | B | U);
                self.p |= I;
                self.pc = bus.read(0xfffe) as u16 | (bus.read(0xffff) as u16) << 8;
            }
            Bcc => branch!(self.p & C == 0),
            Bcs => branch!(self.p & C != 0),
            Bne => branch!(self.p & Z == 0),
            Beq => branch!(self.p & Z != 0),
            Bpl => branch!(self.p & N == 0),
            Bmi => branch!(self.p & N != 0),
            Bvc => branch!(self.p & V == 0),
            Bvs => branch!(self.p & V != 0),
            Clc => self.p &= !C,
            Sec => self.p |= C,
            Cli => self.p &= !I,
            Sei => self.p |= I,
            Clv => self.p &= !V,
            Cld => self.p &= !D,
            Sed => self.p |= D,
            Nop => { if !matches!(m, Imp | Imm) { bus.read(ea); } }
            // Undocumented: read-modify-write combinations.
            Slo => { let v = val!(); let r = self.asl(v); put!(r); self.a = self.nz(self.a | r) }
            Rla => { let v = val!(); let r = self.rol(v); put!(r); self.a = self.nz(self.a & r) }
            Sre => { let v = val!(); let r = self.lsr(v); put!(r); self.a = self.nz(self.a ^ r) }
            Rra => { let v = val!(); let r = self.ror(v); put!(r); self.adc(r) }
            Dcp => { let v = val!().wrapping_sub(1); put!(v); self.cmp(self.a, v) }
            Isc => { let v = val!().wrapping_add(1); put!(v); self.adc(!v) }
            Sax => bus.write(ea, self.a & self.x, at),
            Lax => { let v = val!(); self.a = self.nz(v); self.x = v }
            Anc => { let v = val!(); self.a = self.nz(self.a & v); self.flag(C, self.a & 0x80 != 0) }
            Alr => { let v = val!(); let r = self.a & v; self.a = self.lsr(r) }
            Arr => {
                let v = val!();
                let r = (self.a & v) >> 1 | (self.p & C) << 7;
                self.a = self.nz(r);
                self.flag(C, r & 0x40 != 0);
                self.flag(V, ((r >> 6) ^ (r >> 5)) & 1 != 0);
            }
            Sbx => { let v = val!(); let ax = self.a & self.x; self.flag(C, ax >= v); self.x = self.nz(ax.wrapping_sub(v)) }
            // (Unstable on the chip; the usual approximations.)
            Ane => { let v = val!(); self.a = self.nz((self.a | 0xee) & self.x & v) }
            Lxa => { let v = val!(); let r = (self.a | 0xee) & v; self.a = self.nz(r); self.x = r }
            Las => { let v = val!() & self.s; self.s = v; self.x = v; self.a = self.nz(v) }
            Sha => bus.write(ea, self.a & self.x & ((ea >> 8) as u8).wrapping_add(1), at),
            Shx => bus.write(ea, self.x & ((ea >> 8) as u8).wrapping_add(1), at),
            Shy => bus.write(ea, self.y & ((ea >> 8) as u8).wrapping_add(1), at),
            Tas => { self.s = self.a & self.x; bus.write(ea, self.s & ((ea >> 8) as u8).wrapping_add(1), at) }
            Jam => { self.jammed = true; self.pc = self.pc.wrapping_sub(1) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u8>);
    impl Bus for Ram {
        fn read(&mut self, a: u16) -> u8 { self.0[a as usize] }
        fn write(&mut self, a: u16, v: u8, _: u64) { self.0[a as usize] = v }
    }

    /// Run code at $0200 until it reaches `stop`.
    fn run(code: &[u8], stop: u16) -> (Cpu, Ram) {
        let mut ram = Ram(vec![0; 0x10000]);
        ram.0[0x200..0x200 + code.len()].copy_from_slice(code);
        let mut cpu = Cpu::new();
        cpu.pc = 0x200;
        while cpu.pc != stop && cpu.cycles < 100_000 { cpu.step(&mut ram) }
        (cpu, ram)
    }

    #[test]
    fn arithmetic_and_flags() {
        // LDA #$50; CLC; ADC #$50 (signed overflow) -> $A0, V set, C clear.
        let (c, _) = run(&[0xa9, 0x50, 0x18, 0x69, 0x50], 0x205);
        assert_eq!((c.a, c.p & (V | C | N)), (0xa0, V | N));
        // SEC; LDA #$00; SBC #$01 -> $FF, borrow (C clear).
        let (c, _) = run(&[0x38, 0xa9, 0x00, 0xe9, 0x01], 0x205);
        assert_eq!((c.a, c.p & C), (0xff, 0));
    }

    #[test]
    fn loop_subroutine_and_cycles() {
        // $0200: LDX #5; loop: JSR $0210; DEX; BNE loop; ...  $0210: INC $10; RTS.
        let mut code = vec![0xa2, 0x05, 0x20, 0x10, 0x02, 0xca, 0xd0, 0xfa];
        code.resize(0x10, 0xea);
        code.extend([0xe6, 0x10, 0x60]);
        let (c, ram) = run(&code, 0x208);
        assert_eq!(ram.0[0x10], 5);
        // LDX 2, then 5 x (JSR 6 + INC 5 + RTS 6 + DEX 2) + 4 taken BNE (3) + the last one (2).
        assert_eq!(c.cycles, 2 + 5 * 19 + 4 * 3 + 2);
    }

    #[test]
    fn indirect_indexed_and_undocumented() {
        // ($10),Y with $10 = $02FF, Y = 1 -> $0300 (a page crossing); LAX; DCP.
        let code = [0xa9, 0xff, 0x85, 0x10, 0xa9, 0x02, 0x85, 0x11, 0xa0, 0x01, 0xb3, 0x10, 0xc7, 0x20];
        let (mut cpu, mut ram) = (Cpu::new(), Ram(vec![0; 0x10000]));
        ram.0[0x200..0x200 + code.len()].copy_from_slice(&code);
        ram.0[0x300] = 0x42;
        ram.0[0x20] = 0x43;
        cpu.pc = 0x200;
        while cpu.pc < 0x20e { cpu.step(&mut ram) }
        assert_eq!((cpu.a, cpu.x), (0x42, 0x42));
        assert_eq!(ram.0[0x20], 0x42);
        assert!(cpu.p & Z != 0 && cpu.p & C != 0, "DCP compares A with the decremented value");
    }
}

/// Klaus Dormann's 6502 functional test (`6502_functional_test.bin`, not included): `KLAUS=path cargo test
/// --release -- --ignored functional`. It reports where it trapped: the binary-mode tests pass when that is in
/// the decimal-mode ADC/SBC check (`$3477`, decimal flag set), the last test, since the 2A03 has no decimal mode.
#[cfg(test)]
mod klaus {
    use super::*;
    struct Ram(Vec<u8>);
    impl Bus for Ram {
        fn read(&mut self, a: u16) -> u8 { self.0[a as usize] }
        fn write(&mut self, a: u16, v: u8, _: u64) { self.0[a as usize] = v }
    }
    #[test]
    #[ignore]
    fn functional() {
        let mut ram = Ram(std::fs::read(std::env::var("KLAUS").unwrap()).unwrap());
        let mut cpu = Cpu::new();
        cpu.pc = 0x400;
        let mut last = 0;
        for _ in 0..200_000_000u64 {
            last = cpu.pc;
            cpu.step(&mut ram);
            if cpu.pc == last { break }
        }
        assert!(last == 0x3477 && cpu.p & D != 0, "trapped at {last:04x} (a {:02x} x {:02x} y {:02x} p {:02x} s {:02x}) after {} cycles", cpu.a, cpu.x, cpu.y, cpu.p, cpu.s, cpu.cycles);
    }
}
