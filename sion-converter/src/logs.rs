//! GYM (Mega Drive) and S98 (PC-88/PC-98, MSX...) readers. Both are register logs like VGM, so each is rewritten
//! as a VGM file, which `vgm` then reads: same chips, timing and sample handling.

use crate::bytes::u32le;

/// VGM file builder: commands, with the time they've reached (44100 Hz frames), and the YM2612 DAC values
/// written (each at a seek-and-play placeholder in `body`).
struct Vgm { fields: Vec<(usize, u32)>, body: Vec<u8>, t: u64, loop_at: Option<(usize, u64)>, dac: Vec<(usize, u8)> }

impl Vgm {
    fn new(fields: Vec<(usize, u32)>) -> Self { Vgm { fields, body: vec![], t: 0, loop_at: None, dac: vec![] } }
    /// The YM2612 DAC plays `x` now.
    fn dac(&mut self, x: u8) {
        self.dac.push((self.body.len(), x));
        self.body.extend([0xe0, 0, 0, 0, 0, 0x80]);
    }
    /// Wait until time `t`.
    fn until(&mut self, t: u64) {
        while self.t < t {
            let n = (t - self.t).min(0xffff);
            self.body.extend([0x61, n as u8, (n >> 8) as u8]);
            self.t += n;
        }
    }
    fn mark_loop(&mut self) { self.loop_at = Some((self.body.len(), self.t)) }
    fn finish(mut self) -> Vec<u8> {
        self.body.push(0x66);
        // DAC values: as reads from a sample bank made of them, where every sound heard again reads the bytes
        // it did the first time, so `pcm` can tell repeated samples apart from new ones.
        let mut pre = vec![];
        if !self.dac.is_empty() {
            let (bank, src) = bank(&self.dac.iter().map(|d| d.1).collect::<Vec<_>>());
            for (&(at, _), &s) in self.dac.iter().zip(&src) { self.body[at + 1..at + 5].copy_from_slice(&(s as u32).to_le_bytes()) }
            pre.extend([0x67, 0x66, 0x00]);
            pre.extend((bank.len() as u32).to_le_bytes());
            pre.extend(bank);
            if let Some(l) = &mut self.loop_at { l.0 += pre.len() }
        }
        self.body.splice(0..0, pre);
        let mut h = vec![0u8; 0x100];
        h[..4].copy_from_slice(b"Vgm ");
        let mut set = |o: usize, v: u32| h[o..o + 4].copy_from_slice(&v.to_le_bytes());
        set(4, (0x100 + self.body.len() - 4) as u32);
        set(8, 0x171);
        set(0x18, self.t as u32);
        set(0x34, 0x100 - 0x34);
        // (A loop that reaches no time at all is no loop.)
        if let Some((ofs, at)) = self.loop_at.filter(|l| l.1 < self.t) { set(0x1c, (0x100 + ofs - 0x1c) as u32); set(0x20, (self.t - at) as u32) }
        for &(o, v) in &self.fields { set(o, v) }
        h.extend_from_slice(&self.body);
        h
    }
}

/// A sample bank for a stream of DAC values, and where in it each value is read: stretches of the stream that
/// repeat (at least `MIN` values) read the bank where they were first stored.
fn bank(vals: &[u8]) -> (Vec<u8>, Vec<usize>) {
    const MIN: usize = 32;
    let key = |w: &[u8]| w.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    let (mut bank, mut src) = (vec![], Vec::with_capacity(vals.len()));
    let mut seen = std::collections::HashMap::<u64, usize>::new();
    let mut cur: Option<usize> = None;
    for i in 0..vals.len() {
        // (Carry on reading the bank while it matches.)
        if let Some(p) = cur.filter(|&p| bank.get(p) == Some(&vals[i])) { src.push(p); cur = Some(p + 1); continue }
        let hit = vals.get(i..i + MIN).and_then(|w| seen.get(&key(w)).copied().filter(|&p| bank[p..p + MIN] == *w));
        if let Some(p) = hit { src.push(p); cur = Some(p + 1); continue }
        bank.push(vals[i]);
        src.push(bank.len() - 1);
        cur = None;
        if bank.len() >= MIN { seen.entry(key(&bank[bank.len() - MIN..])).or_insert(bank.len() - MIN); }
    }
    (bank, src)
}

/// A GYM command: (command, register, value).
type Cmd = (u8, u8, u8);

/// The frames of GYM commands (each ended by a 0 byte), or None if the data isn't a GYM command stream.
fn gym_frames(d: &[u8]) -> Option<Vec<Vec<Cmd>>> {
    let (mut frames, mut i) = (vec![vec![]], 0);
    while i < d.len() {
        let arg = |k: usize| d.get(i + k).copied();
        match d[i] {
            0 => { frames.push(vec![]); i += 1 }
            c @ (1 | 2) => { frames.last_mut()?.push((c, arg(1)?, arg(2)?)); i += 3 }
            3 => { frames.last_mut()?.push((3, 0, arg(1)?)); i += 2 }
            // (libgme skips a stray byte; many GYMs have them.)
            _ => i += 1,
        }
    }
    // (The frame the trailing 0 starts holds nothing.)
    if frames.last().is_some_and(|f| f.is_empty()) { frames.pop(); }
    Some(frames)
}

/// A GYM file without the GYMX header is only commands: recognized by being well formed throughout, with its
/// FM writes to registers the YM2612 has.
fn bare_gym(d: &[u8]) -> bool {
    let (mut i, mut frames, mut writes) = (0, 0usize, 0usize);
    while i < d.len() {
        match d[i] {
            0 => { frames += 1; i += 1 }
            1 | 2 => {
                let Some(&r) = d.get(i + 1) else { return false };
                if d.get(i + 2).is_none() || !(0x21..=0xb7).contains(&r) || (d[i] == 2 && r < 0x30) { return false }
                writes += 1;
                i += 3;
            }
            3 => { if d.get(i + 1).is_none() { return false } writes += 1; i += 2 }
            _ => return false,
        }
    }
    frames >= 16 && writes >= 8
}

pub fn detect_gym(d: &[u8]) -> bool { d.starts_with(b"GYMX") || bare_gym(d) }

/// A GYM file as VGM. Frames are 1/60 s; the YM2612 DAC samples a frame writes are spread over it as libgme does.
pub fn gym_to_vgm(d: &[u8]) -> Result<(Vec<u8>, Vec<String>), String> {
    let (body, loop_frame) = if d.starts_with(b"GYMX") {
        if d.len() < 0x1ac { return Err("truncated GYM header".into()) }
        let packed = u32le(d, 0x1a8);
        let body = if packed != 0 {
            miniz_oxide::inflate::decompress_to_vec_zlib(&d[0x1ac..]).map_err(|e| format!("bad packed GYM data: {e:?}"))?
        } else { d[0x1ac..].to_vec() };
        // (libgme starts the loop on the frame the loop start counts to, from 1.)
        (body, u32le(d, 0x1a4).checked_sub(1).map(|f| f as usize))
    } else { (d.to_vec(), None) };
    let frames = gym_frames(&body).ok_or("not a GYM file")?;
    if frames.iter().all(|f| f.is_empty()) { return Err("the GYM file writes nothing".into()) }

    let mut v = Vgm::new(vec![(0x0c, 3579545), (0x2c, 7670453), (0x24, 60)]);
    let (mut dac_on, mut prev) = (false, 0usize);
    for (k, f) in frames.iter().enumerate() {
        v.until(k as u64 * 735);
        if loop_frame == Some(k) { v.mark_loop() }
        let mut dac = vec![];
        for &(c, r, x) in f {
            match c {
                1 if r == 0x2a => { if dac_on { dac.push(x) } }
                1 | 2 => { if c == 1 && r == 0x2b { dac_on = x & 0x80 != 0 } v.body.extend([0x51 + c, r, x]) }
                _ => v.body.extend([0x50, x]),
            }
        }
        if !dac.is_empty() {
            // libgme's guess at where a sample starts or ends within its first and last frames: a frame with fewer
            // samples than the one after (and none before) ends with them, one with fewer than before starts with
            // them; the rest are spread evenly.
            let next = frames.get(k + 1).map_or(0, |f| f.iter().filter(|w| w.0 == 1 && w.1 == 0x2a).count());
            let (mut rate, mut first) = (dac.len(), 0);
            if prev == 0 && next > dac.len() { (rate, first) = (next, next - dac.len()) } else if prev > dac.len() && next == 0 { rate = prev }
            let step = 735.0 / rate as f64;
            for (j, &x) in dac.iter().enumerate() {
                v.until(k as u64 * 735 + ((first + j) as f64 * step + step / 2.0) as u64);
                v.dac(x);
            }
        }
        prev = dac.len();
    }
    v.until(frames.len() as u64 * 735);
    Ok((v.finish(), vec![]))
}

pub fn detect_s98(d: &[u8]) -> bool { d.len() >= 0x20 && d.starts_with(b"S98") && (b'0'..=b'3').contains(&d[3]) }

/// An S98 file as VGM. Devices map to the VGM chips the converter reads (one of each); YM2608 ADPCM data, which
/// S98 writes through the chip's memory port, becomes VGM data blocks.
pub fn s98_to_vgm(d: &[u8]) -> Result<(Vec<u8>, Vec<String>), String> {
    if !detect_s98(d) { return Err("not an S98 file".into()) }
    let version = d[3] - b'0';
    let num = if version == 0 { 0 } else { u32le(d, 4) };
    let den = if version <= 1 { 0 } else { u32le(d, 8) };
    let (num, den) = (if num == 0 { 10 } else { num } as f64, if den == 0 { 1000 } else { den } as f64);
    if u32le(d, 0x0c) != 0 { return Err("compressed S98 files are not supported".into()) }
    let (data, lp) = (u32le(d, 0x14) as usize, u32le(d, 0x18) as usize);
    if data >= d.len() { return Err("truncated S98 file".into()) }
    let lp = (lp >= data && lp < d.len()).then_some(lp);

    // Devices: (type, clock), listed after the header; none means one YM2608.
    let mut devs = vec![];
    let entry = |k: usize| (0x20 + 16 * k + 16 <= d.len()).then(|| (u32le(d, 0x20 + 16 * k), u32le(d, 0x24 + 16 * k)));
    match version {
        2 => { for k in 0.. { match entry(k) { Some((t, c)) if t != 0 => devs.push((t, c)), _ => break } } }
        // (Up to the end of the file, whatever count the header gives.)
        3 => { for k in 0..u32le(d, 0x1c) as usize { match entry(k) { Some(e) => devs.push(e), None => break } } }
        _ => {}
    }
    if devs.is_empty() { devs.push((4, 7987200)) }

    let mut warnings = vec![];
    let mut fields = vec![];
    // Each device: its VGM commands (port 0, port 1), or None if it isn't converted.
    let mut cmds: Vec<Option<(u8, u8)>> = vec![];
    let mut used = std::collections::HashSet::new();
    for &(ty, clock) in &devs {
        let (name, c, field) = match ty {
            1 => ("YM2149", (0xa0, 0xa0), 0x74),
            15 => ("AY-3-8910", (0xa0, 0xa0), 0x74),
            2 => ("YM2203", (0x55, 0x55), 0x44),
            3 => ("YM2612", (0x52, 0x53), 0x2c),
            4 => ("YM2608", (0x56, 0x57), 0x48),
            5 => ("YM2151", (0x54, 0x54), 0x30),
            16 => ("SN76489", (0x50, 0x4f), 0x0c),
            6 => { warnings.push("not converted (unsupported chip): YM2413".into()); cmds.push(None); continue }
            7..=9 => { warnings.push(format!("not converted (unsupported chip): {}", ["YM3526", "YM3812", "YMF262"][ty as usize - 7])); cmds.push(None); continue }
            _ => { warnings.push(format!("not converted: unknown S98 device type {ty}")); cmds.push(None); continue }
        };
        if !used.insert(field) { warnings.push(format!("a second {name} is not converted")); cmds.push(None); continue }
        // (libvgm plays both PSGs at half the listed clock: the YM2149 with its divider pin set.)
        fields.push((field, if field == 0x74 { clock / 2 } else { clock }));
        cmds.push(Some(c));
    }

    let mut v = Vgm::new(fields);
    let (mut tick, mut i) = (0u64, data);
    // YM2608 ADPCM memory: per device, its registers and the bytes written since the address was set.
    let mut adpcm: Vec<([u8; 16], Option<usize>, Vec<u8>)> = vec![([0; 16], None, vec![]); devs.len()];
    let flush = |v: &mut Vgm, a: &mut ([u8; 16], Option<usize>, Vec<u8>)| {
        let (Some(at), false) = (a.1, a.2.is_empty()) else { return };
        let n = a.2.len();
        v.body.extend([0x67, 0x66, 0x81]);
        v.body.extend(((n + 8) as u32).to_le_bytes());
        v.body.extend(0x40000u32.to_le_bytes());
        v.body.extend(((at - n) as u32).to_le_bytes());
        v.body.append(&mut a.2);
    };
    let mut ended = false;
    while i < d.len() {
        let at = |tick: u64| (tick as f64 * num * 44100.0 / den).round() as u64;
        if lp == Some(i) { v.until(at(tick)); v.mark_loop() }
        let c = d[i];
        match c {
            0xfd => { ended = true; break }
            0xff => { tick += 1; i += 1 }
            0xfe => {
                let (mut n, mut shift) = (0u64, 0);
                i += 1;
                while let Some(&b) = d.get(i) {
                    n |= ((b & 0x7f) as u64) << shift;
                    (i, shift) = (i + 1, shift + 7);
                    if b & 0x80 == 0 || shift > 56 { break }
                }
                tick += n + 2;
            }
            _ => {
                let (Some(&reg), Some(&val)) = (d.get(i + 1), d.get(i + 2)) else { break };
                i += 3;
                let dev = (c >> 1) as usize;
                let Some(Some((p0, p1))) = cmds.get(dev).copied() else { continue };
                v.until(at(tick));
                let port = c & 1;
                match p0 {
                    0x50 => v.body.extend(if reg == 1 { [0x4f, val] } else { [0x50, val] }),
                    0x56 if port == 1 && reg < 0x10 => {
                        let a = &mut adpcm[dev];
                        // Memory writes (record + external memory) go to the address the start registers give,
                        // in 32-byte units with 8-bit RAM, else 4 (as `fm` reads them).
                        if reg == 0x08 && a.0[0] & 0x60 == 0x60 {
                            let shift = if a.0[1] & 3 != 0 { 5 } else { 2 };
                            let at = a.1.get_or_insert((a.0[2] as usize | (a.0[3] as usize) << 8) << shift);
                            *at += 1;
                            a.2.push(val);
                            continue;
                        }
                        if matches!(reg, 0x00..=0x03) { flush(&mut v, a); a.1 = None }
                        a.0[reg as usize] = val;
                        v.body.extend([p1, reg, val]);
                    }
                    0x52 if port == 0 && reg == 0x2a => v.dac(val),
                    _ => v.body.extend([if port == 0 { p0 } else { p1 }, reg, val]),
                }
            }
        }
    }
    for a in &mut adpcm { flush(&mut v, a) }
    v.until((tick as f64 * num * 44100.0 / den).round() as u64);
    if !ended { warnings.push("the S98 file ends early (no end command)".into()) }
    if lp.is_some() && !ended { v.loop_at = None }
    Ok((v.finish(), warnings))
}
