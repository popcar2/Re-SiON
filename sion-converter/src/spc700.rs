//! The SNES sound CPU (Sony SPC700) for running SPC sound drivers: every instruction, with cycle counts (taken
//! branches included), so register writes land at the time they would on the console.

pub trait Bus {
    /// `cycle`: the CPU cycle the access happens on (the timers and DSP are caught up to it).
    fn read(&mut self, addr: u16, cycle: u64) -> u8;
    fn write(&mut self, addr: u16, val: u8, cycle: u64);
}

const N: u8 = 0x80;
const V: u8 = 0x40;
const P: u8 = 0x20;
const B: u8 = 0x10;
const H: u8 = 0x08;
const I: u8 = 0x04;
const Z: u8 = 0x02;
const C: u8 = 0x01;

/// Base cycles per opcode (a taken branch adds 2).
#[rustfmt::skip]
const CYCLES: [u8; 256] = [
    2,8,4,5,3,4,3,6,2,6,5,4,5,4,6,8, 2,8,4,5,4,5,5,6,5,5,6,5,2,2,4,6,
    2,8,4,5,3,4,3,6,2,6,5,4,5,4,5,2, 2,8,4,5,4,5,5,6,5,5,6,5,2,2,3,8,
    2,8,4,5,3,4,3,6,2,6,4,4,5,4,6,6, 2,8,4,5,4,5,5,6,5,5,4,5,2,2,4,3,
    2,8,4,5,3,4,3,6,2,6,4,4,5,4,5,5, 2,8,4,5,4,5,5,6,5,5,5,5,2,2,3,6,
    2,8,4,5,3,4,3,6,2,6,5,4,5,2,4,5, 2,8,4,5,4,5,5,6,5,5,5,5,2,2,12,5,
    3,8,4,5,3,4,3,6,2,6,4,4,5,2,4,4, 2,8,4,5,4,5,5,6,5,5,5,5,2,2,3,4,
    3,8,4,5,4,5,4,7,2,5,6,4,5,2,4,9, 2,8,4,5,5,6,6,7,4,5,5,5,2,2,6,3,
    2,8,4,5,3,4,3,6,2,4,5,3,4,3,4,3, 2,8,4,5,4,5,5,6,3,4,5,4,2,2,4,3,
];

#[derive(Default)]
pub struct Cpu { pub a: u8, pub x: u8, pub y: u8, pub sp: u8, pub psw: u8, pub pc: u16, pub cycles: u64, pub halted: bool }

impl Cpu {
    fn rd(&mut self, bus: &mut impl Bus, a: u16) -> u8 { bus.read(a, self.cycles) }
    fn wr(&mut self, bus: &mut impl Bus, a: u16, v: u8) { bus.write(a, v, self.cycles) }
    fn fetch(&mut self, bus: &mut impl Bus) -> u8 { let v = self.rd(bus, self.pc); self.pc = self.pc.wrapping_add(1); v }
    fn fetch16(&mut self, bus: &mut impl Bus) -> u16 { let lo = self.fetch(bus) as u16; lo | (self.fetch(bus) as u16) << 8 }
    /// A direct-page address (page 0, or page 1 with P set).
    fn dp(&self, o: u8) -> u16 { (if self.psw & P != 0 { 0x100 } else { 0 }) | o as u16 }
    /// A 16-bit word in the direct page (the high byte wraps within the page).
    fn rd16dp(&mut self, bus: &mut impl Bus, o: u8) -> u16 { let lo = self.rd(bus, self.dp(o)) as u16; lo | (self.rd(bus, self.dp(o.wrapping_add(1))) as u16) << 8 }
    fn wr16dp(&mut self, bus: &mut impl Bus, o: u8, v: u16) { self.wr(bus, self.dp(o), v as u8); self.wr(bus, self.dp(o.wrapping_add(1)), (v >> 8) as u8) }
    pub fn push(&mut self, bus: &mut impl Bus, v: u8) { self.wr(bus, 0x100 | self.sp as u16, v); self.sp = self.sp.wrapping_sub(1) }
    fn pop(&mut self, bus: &mut impl Bus) -> u8 { self.sp = self.sp.wrapping_add(1); self.rd(bus, 0x100 | self.sp as u16) }
    fn push16(&mut self, bus: &mut impl Bus, v: u16) { self.push(bus, (v >> 8) as u8); self.push(bus, v as u8) }
    fn pop16(&mut self, bus: &mut impl Bus) -> u16 { let lo = self.pop(bus) as u16; lo | (self.pop(bus) as u16) << 8 }
    fn nz(&mut self, v: u8) -> u8 { self.psw = self.psw & !(N | Z) | v & N | if v == 0 { Z } else { 0 }; v }
    fn nz16(&mut self, v: u16) { self.psw = self.psw & !(N | Z) | ((v >> 8) as u8 & N) | if v == 0 { Z } else { 0 } }
    fn flag(&mut self, f: u8, on: bool) { if on { self.psw |= f } else { self.psw &= !f } }

    fn adc(&mut self, a: u8, b: u8) -> u8 {
        let c = (self.psw & C) as u16;
        let r = a as u16 + b as u16 + c;
        self.flag(C, r > 0xff);
        self.flag(H, (a & 15) as u16 + (b & 15) as u16 + c > 15);
        self.flag(V, !(a ^ b) & (a ^ r as u8) & 0x80 != 0);
        self.nz(r as u8)
    }
    fn sbc(&mut self, a: u8, b: u8) -> u8 { self.adc(a, !b) }
    fn cmp(&mut self, a: u8, b: u8) { let r = a as i16 - b as i16; self.flag(C, r >= 0); self.nz(r as u8); }
    fn alu(&mut self, op: u8, a: u8, b: u8) -> u8 {
        match op {
            0 => self.nz(a | b),
            1 => self.nz(a & b),
            2 => self.nz(a ^ b),
            3 => { self.cmp(a, b); a }
            4 => self.adc(a, b),
            _ => self.sbc(a, b),
        }
    }
    fn shift(&mut self, op: u8, v: u8) -> u8 {
        let c = self.psw & C;
        let (r, out) = match op { 0 => (v << 1, v >> 7), 1 => (v << 1 | c, v >> 7), 2 => (v >> 1, v & 1), _ => (v >> 1 | c << 7, v & 1) };
        self.flag(C, out != 0);
        self.nz(r)
    }
    fn branch(&mut self, bus: &mut impl Bus, take: bool) {
        let rel = self.fetch(bus) as i8;
        if take { self.pc = self.pc.wrapping_add(rel as u16); self.cycles += 2 }
    }
    /// The 13-bit address and bit number of a `mem.bit` operand.
    fn membit(&mut self, bus: &mut impl Bus) -> (u16, u8) { let w = self.fetch16(bus); (w & 0x1fff, (w >> 13) as u8) }

    /// The address of an ALU-group operand by the opcode's low bits (x4..x7, xx4..xx7 rows), or None for the
    /// immediate.
    fn operand_addr(&mut self, bus: &mut impl Bus, op: u8) -> u16 {
        match (op >> 4 & 1, op & 15) {
            (0, 4) => { let o = self.fetch(bus); self.dp(o) }
            (0, 5) => self.fetch16(bus),
            (0, 6) => self.dp(self.x),
            (0, 7) => { let o = self.fetch(bus).wrapping_add(self.x); self.rd16dp(bus, o) }
            (1, 4) => { let o = self.fetch(bus).wrapping_add(self.x); self.dp(o) }
            (1, 5) => self.fetch16(bus).wrapping_add(self.x as u16),
            (1, 6) => self.fetch16(bus).wrapping_add(self.y as u16),
            _ => { let o = self.fetch(bus); self.rd16dp(bus, o).wrapping_add(self.y as u16) }
        }
    }

    /// Run one instruction.
    pub fn step(&mut self, bus: &mut impl Bus) {
        if self.halted { self.cycles += 2; return }
        let op = self.fetch(bus);
        self.cycles += CYCLES[op as usize] as u64;
        let (hi, lo) = (op >> 4, op & 15);
        match op {
            // ALU group: OR AND EOR CMP ADC SBC on A (rows 0,2,4,6,8,A and the following ones).
            _ if alu_op(op) => {
                let k = hi >> 1;
                match (hi & 1, lo) {
                    (_, 4..=7) => { let a = self.operand_addr(bus, op); let b = self.rd(bus, a); self.a = self.alu(k, self.a, b) }
                    (0, 8) => { let b = self.fetch(bus); self.a = self.alu(k, self.a, b) }
                    (0, 9) => {
                        // dp, dp: source first, then destination.
                        let s = self.fetch(bus);
                        let b = self.rd(bus, self.dp(s));
                        let d = self.fetch(bus);
                        let a = self.rd(bus, self.dp(d));
                        let r = self.alu(k, a, b);
                        if k != 3 { self.wr(bus, self.dp(d), r) }
                    }
                    (1, 8) => {
                        // dp, #imm
                        let b = self.fetch(bus);
                        let d = self.fetch(bus);
                        let a = self.rd(bus, self.dp(d));
                        let r = self.alu(k, a, b);
                        if k != 3 { self.wr(bus, self.dp(d), r) }
                    }
                    _ => {
                        // (X), (Y)
                        let b = self.rd(bus, self.dp(self.y));
                        let a = self.rd(bus, self.dp(self.x));
                        let r = self.alu(k, a, b);
                        if k != 3 { self.wr(bus, self.dp(self.x), r) }
                    }
                }
            }
            0x00 => {}
            // TCALL n
            _ if lo == 1 => { let n = hi as u16; let pc = self.pc; self.push16(bus, pc); let a = 0xffde - 2 * n; self.pc = self.rd(bus, a) as u16 | (self.rd(bus, a + 1) as u16) << 8 }
            // SET1 / CLR1 dp.bit
            _ if lo == 2 => { let o = self.fetch(bus); let a = self.dp(o); let v = self.rd(bus, a); let m = 1 << (hi >> 1); self.wr(bus, a, if hi & 1 == 0 { v | m } else { v & !m }) }
            // BBS / BBC dp.bit, rel
            _ if lo == 3 => { let o = self.fetch(bus); let v = self.rd(bus, self.dp(o)); let set = v >> (hi >> 1) & 1 != 0; self.branch(bus, set == (hi & 1 == 0)) }
            // Branches on flags.
            0x10 => { let t = self.psw & N == 0; self.branch(bus, t) }
            0x30 => { let t = self.psw & N != 0; self.branch(bus, t) }
            0x50 => { let t = self.psw & V == 0; self.branch(bus, t) }
            0x70 => { let t = self.psw & V != 0; self.branch(bus, t) }
            0x90 => { let t = self.psw & C == 0; self.branch(bus, t) }
            0xb0 => { let t = self.psw & C != 0; self.branch(bus, t) }
            0xd0 => { let t = self.psw & Z == 0; self.branch(bus, t) }
            0xf0 => { let t = self.psw & Z != 0; self.branch(bus, t) }
            0x2f => self.branch(bus, true),
            0x20 => self.psw &= !P,
            0x40 => self.psw |= P,
            0x60 => self.psw &= !C,
            0x80 => self.psw |= C,
            0xa0 => self.psw |= I,
            0xc0 => self.psw &= !I,
            0xe0 => self.psw &= !(V | H),
            0xed => self.psw ^= C,
            // Bit operations on mem.bit.
            0x0a | 0x2a | 0x4a | 0x6a | 0x8a | 0xaa => {
                let (a, b) = self.membit(bus);
                let bit = self.rd(bus, a) >> b & 1 != 0;
                let c = self.psw & C != 0;
                let r = match op { 0x0a => c | bit, 0x2a => c | !bit, 0x4a => c & bit, 0x6a => c & !bit, 0x8a => c ^ bit, _ => bit };
                self.flag(C, r);
            }
            0xca => { let (a, b) = self.membit(bus); let v = self.rd(bus, a); let c = self.psw & C; self.wr(bus, a, v & !(1 << b) | c << b) }
            0xea => { let (a, b) = self.membit(bus); let v = self.rd(bus, a); self.wr(bus, a, v ^ 1 << b) }
            // Shifts and rotates: ASL ROL LSR ROR on dp, !abs, dp+X, A.
            0x0b | 0x2b | 0x4b | 0x6b => { let o = self.fetch(bus); let a = self.dp(o); let v = self.rd(bus, a); let r = self.shift(hi >> 1, v); self.wr(bus, a, r) }
            0x0c | 0x2c | 0x4c | 0x6c => { let a = self.fetch16(bus); let v = self.rd(bus, a); let r = self.shift(hi >> 1, v); self.wr(bus, a, r) }
            0x1b | 0x3b | 0x5b | 0x7b => { let o = self.fetch(bus).wrapping_add(self.x); let a = self.dp(o); let v = self.rd(bus, a); let r = self.shift(hi >> 1, v); self.wr(bus, a, r) }
            0x1c | 0x3c | 0x5c | 0x7c => self.a = self.shift(hi >> 1, self.a),
            // INC / DEC on dp, !abs, dp+X.
            0x8b | 0xab => { let o = self.fetch(bus); let a = self.dp(o); let v = self.rd(bus, a); let r = if op == 0xab { v.wrapping_add(1) } else { v.wrapping_sub(1) }; let r = self.nz(r); self.wr(bus, a, r) }
            0x8c | 0xac => { let a = self.fetch16(bus); let v = self.rd(bus, a); let r = if op == 0xac { v.wrapping_add(1) } else { v.wrapping_sub(1) }; let r = self.nz(r); self.wr(bus, a, r) }
            0x9b | 0xbb => { let o = self.fetch(bus).wrapping_add(self.x); let a = self.dp(o); let v = self.rd(bus, a); let r = if op == 0xbb { v.wrapping_add(1) } else { v.wrapping_sub(1) }; let r = self.nz(r); self.wr(bus, a, r) }
            0x9c => self.a = self.nz(self.a.wrapping_sub(1)),
            0xbc => self.a = self.nz(self.a.wrapping_add(1)),
            0x1d => self.x = self.nz(self.x.wrapping_sub(1)),
            0x3d => self.x = self.nz(self.x.wrapping_add(1)),
            0xdc => self.y = self.nz(self.y.wrapping_sub(1)),
            0xfc => self.y = self.nz(self.y.wrapping_add(1)),
            // Stack.
            0x0d => { let p = self.psw; self.push(bus, p) }
            0x2d => { let a = self.a; self.push(bus, a) }
            0x4d => { let x = self.x; self.push(bus, x) }
            0x6d => { let y = self.y; self.push(bus, y) }
            0x8e => self.psw = self.pop(bus),
            0xae => self.a = self.pop(bus),
            0xce => self.x = self.pop(bus),
            0xee => self.y = self.pop(bus),
            // TSET1 / TCLR1 !abs
            0x0e | 0x4e => {
                let a = self.fetch16(bus);
                let v = self.rd(bus, a);
                self.nz(self.a.wrapping_sub(v));
                self.wr(bus, a, if op == 0x0e { v | self.a } else { v & !self.a });
            }
            0x0f => {
                let pc = self.pc;
                self.push16(bus, pc);
                let p = self.psw;
                self.push(bus, p);
                self.psw = (self.psw | B) & !I;
                self.pc = self.rd(bus, 0xffde) as u16 | (self.rd(bus, 0xffdf) as u16) << 8;
            }
            0x1a | 0x3a => { let o = self.fetch(bus); let v = self.rd16dp(bus, o); let r = if op == 0x3a { v.wrapping_add(1) } else { v.wrapping_sub(1) }; self.wr16dp(bus, o, r); self.nz16(r) }
            // CMP X / Y with memory.
            0x1e => { let a = self.fetch16(bus); let v = self.rd(bus, a); self.cmp(self.x, v) }
            0x3e => { let o = self.fetch(bus); let v = self.rd(bus, self.dp(o)); self.cmp(self.x, v) }
            0x5e => { let a = self.fetch16(bus); let v = self.rd(bus, a); self.cmp(self.y, v) }
            0x7e => { let o = self.fetch(bus); let v = self.rd(bus, self.dp(o)); self.cmp(self.y, v) }
            0xc8 => { let v = self.fetch(bus); self.cmp(self.x, v) }
            0xad => { let v = self.fetch(bus); self.cmp(self.y, v) }
            0x1f => { let a = self.fetch16(bus).wrapping_add(self.x as u16); self.pc = self.rd(bus, a) as u16 | (self.rd(bus, a.wrapping_add(1)) as u16) << 8 }
            0x5f => self.pc = self.fetch16(bus),
            0x3f => { let a = self.fetch16(bus); let pc = self.pc; self.push16(bus, pc); self.pc = a }
            0x4f => { let u = self.fetch(bus); let pc = self.pc; self.push16(bus, pc); self.pc = 0xff00 | u as u16 }
            0x6f => self.pc = self.pop16(bus),
            0x7f => { self.psw = self.pop(bus); self.pc = self.pop16(bus) }
            // CBNE dp / dp+X, DBNZ dp / Y.
            0x2e => { let o = self.fetch(bus); let v = self.rd(bus, self.dp(o)); let t = self.a != v; self.branch(bus, t) }
            0xde => { let o = self.fetch(bus).wrapping_add(self.x); let v = self.rd(bus, self.dp(o)); let t = self.a != v; self.branch(bus, t) }
            0x6e => { let o = self.fetch(bus); let a = self.dp(o); let v = self.rd(bus, a).wrapping_sub(1); self.wr(bus, a, v); self.branch(bus, v != 0) }
            0xfe => { self.y = self.y.wrapping_sub(1); let t = self.y != 0; self.branch(bus, t) }
            // 16-bit YA arithmetic.
            0x5a => { let o = self.fetch(bus); let w = self.rd16dp(bus, o); let ya = self.ya(); let r = ya.wrapping_sub(w); self.flag(C, ya >= w); self.nz16(r) }
            0x7a | 0x9a => {
                let o = self.fetch(bus);
                let w = self.rd16dp(bus, o);
                let (ya, w, c) = if op == 0x7a { (self.ya(), w, 0u32) } else { (self.ya(), !w, 1u32) };
                let r = ya as u32 + w as u32 + c;
                self.flag(C, r > 0xffff);
                self.flag(H, (ya & 0xfff) as u32 + (w & 0xfff) as u32 + c > 0xfff);
                self.flag(V, !(ya ^ w) & (ya ^ r as u16) & 0x8000 != 0);
                self.set_ya(r as u16);
                self.nz16(r as u16);
            }
            0xba => { let o = self.fetch(bus); let w = self.rd16dp(bus, o); self.set_ya(w); self.nz16(w) }
            0xda => { let o = self.fetch(bus); let ya = self.ya(); self.wr16dp(bus, o, ya) }
            0xcf => { let r = self.y as u16 * self.a as u16; self.set_ya(r); self.nz(self.y); }
            0x9e => {
                let (ya, x) = (self.ya() as u32, self.x as u32);
                self.flag(H, (self.y & 15) >= (self.x & 15));
                self.flag(V, self.y as u32 >= x);
                if (self.y as u32) < x << 1 {
                    (self.a, self.y) = ((ya / x) as u8, (ya % x) as u8);
                } else {
                    let r = ya - (x << 9);
                    (self.a, self.y) = ((255 - r / (256 - x)) as u8, (x + r % (256 - x)) as u8);
                }
                self.nz(self.a);
            }
            0x9f => self.a = self.nz(self.a >> 4 | self.a << 4),
            0xdf => {
                if self.psw & C != 0 || self.a > 0x99 { self.a = self.a.wrapping_add(0x60); self.psw |= C }
                if self.psw & H != 0 || self.a & 15 > 9 { self.a = self.a.wrapping_add(6) }
                self.nz(self.a);
            }
            0xbe => {
                if self.psw & C == 0 || self.a > 0x99 { self.a = self.a.wrapping_sub(0x60); self.psw &= !C }
                if self.psw & H == 0 || self.a & 15 > 9 { self.a = self.a.wrapping_sub(6) }
                self.nz(self.a);
            }
            // Moves.
            0x5d => self.x = self.nz(self.a),
            0x7d => self.a = self.nz(self.x),
            0x9d => self.x = self.nz(self.sp),
            0xbd => self.sp = self.x,
            0xdd => self.a = self.nz(self.y),
            0xfd => self.y = self.nz(self.a),
            0x8d => { let v = self.fetch(bus); self.y = self.nz(v) }
            0xcd => { let v = self.fetch(bus); self.x = self.nz(v) }
            0xe8 => { let v = self.fetch(bus); self.a = self.nz(v) }
            0x8f => { let v = self.fetch(bus); let o = self.fetch(bus); self.wr(bus, self.dp(o), v) }
            0xfa => { let s = self.fetch(bus); let v = self.rd(bus, self.dp(s)); let d = self.fetch(bus); self.wr(bus, self.dp(d), v) }
            0xaf => { let a = self.a; self.wr(bus, self.dp(self.x), a); self.x = self.x.wrapping_add(1) }
            0xbf => { let v = self.rd(bus, self.dp(self.x)); self.x = self.x.wrapping_add(1); self.a = self.nz(v) }
            // MOV A, mem (E4..E7, F4..F7).
            0xe4..=0xe7 | 0xf4..=0xf7 => { let a = self.operand_addr(bus, op); let v = self.rd(bus, a); self.a = self.nz(v) }
            // MOV mem, A (C4..C7, D4..D7).
            0xc4..=0xc7 | 0xd4..=0xd7 => { let a = self.operand_addr(bus, op - 0x20); let v = self.a; self.wr(bus, a, v) }
            0xe9 => { let a = self.fetch16(bus); let v = self.rd(bus, a); self.x = self.nz(v) }
            0xf8 => { let o = self.fetch(bus); let v = self.rd(bus, self.dp(o)); self.x = self.nz(v) }
            0xf9 => { let o = self.fetch(bus).wrapping_add(self.y); let v = self.rd(bus, self.dp(o)); self.x = self.nz(v) }
            0xeb => { let o = self.fetch(bus); let v = self.rd(bus, self.dp(o)); self.y = self.nz(v) }
            0xec => { let a = self.fetch16(bus); let v = self.rd(bus, a); self.y = self.nz(v) }
            0xfb => { let o = self.fetch(bus).wrapping_add(self.x); let v = self.rd(bus, self.dp(o)); self.y = self.nz(v) }
            0xc9 => { let a = self.fetch16(bus); let v = self.x; self.wr(bus, a, v) }
            0xd8 => { let o = self.fetch(bus); let v = self.x; self.wr(bus, self.dp(o), v) }
            0xd9 => { let o = self.fetch(bus).wrapping_add(self.y); let v = self.x; self.wr(bus, self.dp(o), v) }
            0xcb => { let o = self.fetch(bus); let v = self.y; self.wr(bus, self.dp(o), v) }
            0xcc => { let a = self.fetch16(bus); let v = self.y; self.wr(bus, a, v) }
            0xdb => { let o = self.fetch(bus).wrapping_add(self.x); let v = self.y; self.wr(bus, self.dp(o), v) }
            // SLEEP, STOP.
            0xef | 0xff => { self.halted = true; self.pc = self.pc.wrapping_sub(1) }
            _ => {}
        }
    }

    fn ya(&self) -> u16 { (self.y as u16) << 8 | self.a as u16 }
    fn set_ya(&mut self, v: u16) { (self.y, self.a) = ((v >> 8) as u8, v as u8) }
}

/// Opcodes of the ALU group (OR, AND, EOR, CMP, ADC, SBC on A; dp,dp; dp,#imm; (X),(Y)): rows 0-B, columns 4-9.
fn alu_op(op: u8) -> bool { let (hi, lo) = (op >> 4, op & 15); hi < 12 && (4..=9).contains(&lo) }
