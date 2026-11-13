//! VGM / VGZ reader: replays the logged register writes into chip models (`chips.rs`, `fm.rs`), samples every
//! channel once per sound-driver frame, and turns those frames into notes. The tempo is chosen so that the
//! song's most common row length becomes a 1/16 note. Samples the chips play (`pcm.rs`) become notes of their
//! own, each starting at its exact time.

use crate::chips::{Ay8910, Chip, GameBoy, NesApu, Sn76489};
use crate::fm::{Kind, Opm, Opn};
use crate::pcm::Hit;
use crate::score::{Defs, Note, Score, Stream};

/// Decompress a gzip file (VGZ).
pub fn gunzip(d: &[u8]) -> Result<Vec<u8>, String> {
    if d.len() < 18 || d[0] != 0x1f || d[1] != 0x8b || d[2] != 8 { return Err("not a gzip file".into()) }
    let flags = d[3];
    let mut i = 10;
    if flags & 4 != 0 { i += 2 + (d[i] as usize | (d.get(i + 1).copied().unwrap_or(0) as usize) << 8) }
    for bit in [8, 16] { if flags & bit != 0 { while d.get(i).is_some_and(|&c| c != 0) { i += 1 } i += 1 } }
    if flags & 2 != 0 { i += 2 }
    miniz_oxide::inflate::decompress_to_vec(d.get(i..).ok_or("truncated gzip file")?).map_err(|e| format!("bad gzip data: {e:?}"))
}

/// Chips that may appear in a VGM header but are not converted: (header offset, name).
const OTHER: [(usize, &str); 27] = [
    (0x10, "YM2413"), (0x50, "YM3812"), (0x54, "YM3526"), (0x58, "Y8950"), (0x5c, "YMF262"), (0x60, "YMF278B"), (0x64, "YMF271"),
    (0x68, "YMZ280B"), (0x6c, "RF5C164"), (0x70, "PWM"), (0x40, "RF5C68"), (0x38, "SegaPCM"), (0x88, "MultiPCM"), (0x8c, "uPD7759"),
    (0x90, "OKIM6258"), (0x98, "OKIM6295"), (0x9c, "K051649"), (0xa0, "K054539"), (0xa4, "HuC6280"), (0xa8, "C140"), (0xac, "K053260"),
    (0xb0, "Pokey"), (0xb4, "QSound"), (0xb8, "SCSP"), (0xc0, "WonderSwan"), (0xc4, "VSU"), (0xc8, "SAA1099"),
];

/// A register write at a sample time (44100 Hz), to chip number `chip` (`src`: of that byte of the sample bank);
/// or (`block`) a data block for it.
pub(crate) struct Write { pub at: u64, pub chip: usize, pub port: u8, pub reg: u8, pub val: u8, pub block: Option<usize>, pub src: Option<usize> }

impl Write {
    pub fn reg(at: u64, chip: usize, port: u8, reg: u8, val: u8) -> Self { Write { at, chip, port, reg, val, block: None, src: None } }
}

/// A chip log, from a VGM file or a played NSF: what `render` turns into a score.
pub(crate) struct Log {
    pub chips: Vec<Box<dyn Chip>>,
    /// Mixing level of each chip.
    pub level: Vec<f32>,
    /// In time order.
    pub writes: Vec<Write>,
    /// Data blocks the writes refer to: (type, start in the chip's memory, bytes).
    pub blocks: Vec<(u8, usize, Vec<u8>)>,
    /// The song's end and loop point (44100 Hz frames).
    pub end: u64,
    pub loop_at: Option<u64>,
    /// The driver's frame length (44100 Hz frames), if known; otherwise it is detected from the writes.
    pub period: Option<f64>,
    /// Where in the period the driver's frames start (44100 Hz frames), if known; otherwise detected.
    pub phase: Option<f64>,
    /// The chip whose writes to register 0x2a (port 0) are DAC samples, not the driver's frame.
    pub dac: Option<usize>,
    /// Mega Drive chips: match libvgm's output with a high shelf.
    pub shelf: bool,
}

/// A DAC stream (commands 0x90-0x95): bytes from a data bank written to a chip register at a fixed rate.
#[derive(Default, Clone)]
struct DacStream {
    target: Option<(usize, u8, u8)>,
    bank: u8,
    step: usize,
    base: usize,
    freq: f64,
    /// Playing: start time, first byte, byte count, looping.
    run: Option<(u64, usize, usize, bool)>,
}

impl DacStream {
    /// The writes of the current run up to time `until`; the run then ends.
    fn flush(&mut self, until: u64, banks: &[Vec<u8>], out: &mut Vec<Write>) {
        let (Some((start, first, count, lp)), Some((chip, port, reg))) = (self.run.take(), self.target) else { return };
        let Some(bank) = banks.get(self.bank as usize) else { return };
        if self.freq <= 0.0 || count == 0 { return }
        for k in 0.. {
            if !lp && k >= count { break }
            let at = start + (k as f64 * 44100.0 / self.freq) as u64;
            if at >= until { break }
            let i = first + self.base + (k % count) * self.step.max(1);
            let Some(&val) = bank.get(i) else { break };
            // (Bytes of bank 0, the YM2612's, can be stored as the bank itself.)
            out.push(Write { at, chip, port, reg, val, block: None, src: (self.bank == 0).then_some(i) });
        }
    }
}

/// `exact`: capture samples frame by frame as the chips output them (`pcm.rs`).
pub fn to_score(data: &[u8], exact: bool) -> Result<(Score, Vec<String>), String> {
    let gz;
    let d = if data.starts_with(&[0x1f, 0x8b]) { gz = gunzip(data)?; &gz[..] } else { data };
    if d.len() < 0x40 || &d[..4] != b"Vgm " { return Err("not a VGM file".into()) }
    let u32at = |o: usize| d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let version = u32at(8);
    let start = if version >= 0x150 && u32at(0x34) != 0 { 0x34 + u32at(0x34) as usize } else { 0x40 };
    // Header fields past the header's own size are not there (they overlap the data).
    let hdr = |o: usize| if o + 4 <= start { u32at(o) } else { 0 };
    let mut warnings = vec![];

    // Chips by command byte.
    let mut chips: Vec<Box<dyn Chip>> = vec![];
    let mut slot = [usize::MAX; 256];
    let mut add = |chips: &mut Vec<Box<dyn Chip>>, cmds: &[u8], c: Box<dyn Chip>| { for &k in cmds { slot[k as usize] = chips.len() } chips.push(c) };
    let (sn, ym2612, ym2151) = (hdr(0x0c), if version < 0x110 { hdr(0x10) } else { hdr(0x2c) }, if version < 0x110 { hdr(0x10) } else { hdr(0x30) });
    if sn != 0 { add(&mut chips, &[0x50, 0x4f], Box::new(Sn76489::new(sn & 0x3fff_ffff))) }
    if ym2612 != 0 { add(&mut chips, &[0x52, 0x53], Box::new(Opn::new(Kind::Ym2612, ym2612, 144.0, 6, None))) }
    if ym2151 != 0 { add(&mut chips, &[0x54], Box::new(Opm::new(ym2151))) }
    let ym2203 = hdr(0x44);
    if ym2203 != 0 { add(&mut chips, &[0x55], Box::new(Opn::new(Kind::Ym2203, ym2203, 72.0, 3, Some((ym2203 & 0x3fff_ffff) as f64 / 2.0)))) }
    let ym2608 = hdr(0x48);
    if ym2608 != 0 { add(&mut chips, &[0x56, 0x57], Box::new(Opn::new(Kind::Ym2608, ym2608, 144.0, 6, Some((ym2608 & 0x3fff_ffff) as f64 / 4.0)))) }
    let ym2610 = hdr(0x4c);
    if ym2610 != 0 { add(&mut chips, &[0x58, 0x59], Box::new(Opn::new(Kind::Ym2610, ym2610, 144.0, 6, Some((ym2610 & 0x3fff_ffff) as f64 / 4.0)))) }
    let ay = hdr(0x74);
    if ay != 0 {
        let (ty, flags) = (d.get(0x78).copied().unwrap_or(0), d.get(0x79).copied().unwrap_or(0));
        let clock = (ay & 0x3fff_ffff) as f64 / if ty & 0x10 != 0 && flags & 0x10 != 0 { 2.0 } else { 1.0 };
        add(&mut chips, &[0xa0], Box::new(Ay8910::new(clock)));
    }
    if hdr(0x80) != 0 { add(&mut chips, &[0xb3], Box::new(GameBoy::new())) }
    let nes = hdr(0x84);
    if nes != 0 { add(&mut chips, &[0xb4], Box::new(NesApu::new(nes))) }
    // Mixing level of each chip against the others: the Mega Drive mixes its PSG 12 dB below the YM2612 (a
    // full-volume square at a quarter of a full-scale FM channel, as libvgm plays it).
    let mut level = vec![1f32; chips.len()];
    if sn != 0 && ym2612 != 0 { level[slot[0x50]] = 0.25 }
    // YM2612 and YM2151 shared one clock field before VGM 1.10; keep whichever the song writes to.
    let others: Vec<&str> = OTHER.iter().filter(|(o, _)| hdr(*o) != 0 && !(*o == 0x10 && version < 0x110)).map(|x| x.1).collect();
    if !others.is_empty() { warnings.push(format!("not converted (unsupported chips): {}", others.join(", "))) }
    for c in &mut chips { c.set_exact(exact) }
    if chips.is_empty() { return Err("the VGM uses no supported chip (supported: SN76489, AY-3-8910/YM2149, YM2612, YM2151, YM2203, YM2608, YM2610, NES APU, Game Boy)".into()) }
    if sn & 0x4000_0000 != 0 || ym2612 & 0x4000_0000 != 0 || ay & 0x4000_0000 != 0 { warnings.push("the second chip of a dual-chip setup is not converted".into()) }

    // Commands → timed register writes. Data blocks: sample banks by type (for 0x8n and DAC streams, with each
    // block's start for 0x95), and memory images passed to their chips.
    let (mut writes, mut t, mut i) = (vec![], 0u64, start);
    let (mut banks, mut starts, mut blocks): (Vec<Vec<u8>>, Vec<Vec<(usize, usize)>>, Vec<(u8, usize, Vec<u8>)>) = (vec![vec![]; 0x40], vec![vec![]; 0x40], vec![]);
    let (mut ptr, mut streams, mut compressed) = (0usize, vec![DacStream::default(); 256], false);
    // VGM chip numbers (DAC stream targets) → command bytes.
    // The loop point: a byte offset (from 0x1c) into the commands, reached at sample time `loop_at`.
    let loop_ofs = (u32at(0x1c) != 0 && u32at(0x20) != 0).then(|| 0x1c + u32at(0x1c) as usize);
    let mut loop_at = None;
    let chip_cmd = |id: u8| match id & 0x7f { 0 => 0x50, 2 => 0x52, 3 => 0x54, 6 => 0x55, 7 => 0x56, 8 => 0x58, 0x12 => 0xa0, 0x13 => 0xb3, 0x14 => 0xb4, _ => 0 };
    while i < d.len() {
        if loop_ofs == Some(i) { loop_at = Some(t) }
        let c = d[i];
        let arg = |k: usize| d.get(i + k).copied().unwrap_or(0);
        let a32 = |k: usize| u32at(i + k) as usize;
        let len = match c {
            0x66 => break,
            0x61 => { t += (arg(1) as u16 | (arg(2) as u16) << 8) as u64; 3 }
            0x62 => { t += 735; 1 }
            0x63 => { t += 882; 1 }
            0x70..=0x7f => { t += (c & 15) as u64 + 1; 1 }
            0x80..=0x8f => {
                // YM2612 DAC: the next byte of the sample bank, then a wait.
                let s = slot[0x52];
                if let (true, Some(&val)) = (s != usize::MAX, banks[0].get(ptr)) { writes.push(Write { at: t, chip: s, port: 0, reg: 0x2a, val, block: None, src: Some(ptr) }) }
                (ptr, t) = (ptr + 1, t + (c & 15) as u64);
                1
            }
            0xe0 => { ptr = a32(1); 5 }
            0x67 => {
                let (ty, n) = (arg(2), (u32at(i + 3) & 0x7fff_ffff) as usize);
                let body = d.get(i + 7..(i + 7 + n).min(d.len())).unwrap_or(&[]);
                match ty {
                    0x00..=0x3f => {
                        if ty == 0 && slot[0x52] != usize::MAX { blocks.push((0, banks[0].len(), body.to_vec())); writes.push(Write { at: t, chip: slot[0x52], port: 0, reg: 0, val: 0, block: Some(blocks.len() - 1), src: None }) }
                        starts[ty as usize].push((banks[ty as usize].len(), body.len()));
                        banks[ty as usize].extend_from_slice(body)
                    }
                    0x40..=0x7f => compressed = true,
                    0x80..=0xbf if body.len() >= 8 => {
                        let cmd = match ty { 0x81 => 0x56, 0x82 | 0x83 => 0x58, _ => 0 };
                        if slot[cmd] != usize::MAX { blocks.push((ty, u32at(i + 11) as usize, body[8..].to_vec())); writes.push(Write { at: t, chip: slot[cmd], port: 0, reg: 0, val: 0, block: Some(blocks.len() - 1), src: None }) }
                    }
                    0xc0..=0xdf if body.len() >= 2 => {
                        let cmd = match ty { 0xc2 => 0xb4, _ => 0 };
                        if slot[cmd] != usize::MAX { blocks.push((ty, body[0] as usize | (body[1] as usize) << 8, body[2..].to_vec())); writes.push(Write { at: t, chip: slot[cmd], port: 0, reg: 0, val: 0, block: Some(blocks.len() - 1), src: None }) }
                    }
                    _ => {}
                }
                7 + n
            }
            0x68 => 12,
            0x90 => {
                let cmd = chip_cmd(arg(2));
                let st = &mut streams[arg(1) as usize];
                st.target = (cmd != 0 && slot[cmd] != usize::MAX && arg(2) & 0x80 == 0).then(|| (slot[cmd], arg(3), arg(4)));
                5
            }
            0x91 => { let st = &mut streams[arg(1) as usize]; (st.bank, st.step, st.base) = (arg(2) & 0x3f, arg(3) as usize, arg(4) as usize); 5 }
            0x92 => { streams[arg(1) as usize].freq = u32at(i + 2) as f64; 6 }
            0x93 => {
                let st = &mut streams[arg(1) as usize];
                let prev = st.run;
                st.flush(t, &banks, &mut writes);
                let first = if a32(2) == 0xffff_ffff { prev.map_or(0, |r| r.1) } else { a32(2) };
                let (mode, l) = (arg(6), a32(7));
                let total = banks.get(st.bank as usize).map_or(0, |b| b.len());
                let count = match mode & 3 {
                    1 => l,
                    2 => (l as f64 * st.freq / 1000.0) as usize,
                    3 => total.saturating_sub(first) / st.step.max(1),
                    _ => prev.map_or(0, |r| r.2),
                };
                st.run = Some((t, first, count, mode & 0x80 != 0));
                11
            }
            0x94 => {
                for (k, st) in streams.iter_mut().enumerate() { if arg(1) == 0xff || k == arg(1) as usize { st.flush(t, &banks, &mut writes) } }
                2
            }
            0x95 => {
                let st = &mut streams[arg(1) as usize];
                st.flush(t, &banks, &mut writes);
                let id = arg(2) as usize | (arg(3) as usize) << 8;
                if let Some(&(first, n)) = starts[st.bank as usize].get(id) { st.run = Some((t, first, n / st.step.max(1), arg(4) & 1 != 0)) }
                5
            }
            0x30..=0x3f | 0x4f | 0x50 => 2,
            0x40..=0x4e => 3,
            0x51..=0x5f | 0xa0..=0xbf => 3,
            0xc0..=0xdf => 4,
            0xe1..=0xff => 5,
            _ => 1,
        };
        let s = slot[c as usize];
        if s != usize::MAX {
            let (port, reg, val) = match c {
                0x50 => (0, 0, arg(1)),
                0x4f => (1, 0, arg(1)),
                0x53 | 0x57 | 0x59 => (1, arg(1), arg(2)),
                0xa0 | 0xb3 | 0xb4 if arg(1) & 0x80 != 0 => { i += len; continue } // second chip
                _ => (0, arg(1), arg(2)),
            };
            writes.push(Write { at: t, chip: s, port, reg, val, block: None, src: None });
        }
        i += len;
    }
    let end = t.max(u32at(0x18) as u64);
    for st in &mut streams { st.flush(end, &banks, &mut writes) }
    writes.sort_by_key(|w| w.at);
    if compressed { warnings.push("compressed sample data blocks are not converted".into()) }
    if writes.is_empty() { return Err("the VGM writes to no supported chip".into()) }

    let rate = hdr(0x24);
    let log = Log { chips, level, writes, blocks, end, loop_at, period: (10..=300).contains(&rate).then(|| 44100.0 / rate as f64), phase: None,
        dac: (slot[0x52] != usize::MAX).then_some(slot[0x52]), shelf: sn != 0 || ym2612 != 0 };
    render(log, warnings)
}

/// Sample every channel of the log's chips once per driver frame and turn the frames (and the samples the chips
/// played) into a score.
pub(crate) fn render(log: Log, mut warnings: Vec<String>) -> Result<(Score, Vec<String>), String> {
    let Log { mut chips, level, writes, blocks, end, loop_at, period, phase, dac, shelf } = log;
    // Driver frame rate: the most common gap between bursts of writes, unless the header gives a rate. (Sample
    // data written to the YM2612 DAC isn't the driver's frame.)
    let timing: Vec<&Write> = writes.iter().filter(|w| !(w.reg == 0x2a && w.port == 0 && dac == Some(w.chip)) && w.block.is_none()).collect();
    let bursts: Vec<u64> = timing.iter().enumerate().filter(|(k, w)| *k == 0 || w.at > timing[k - 1].at + 32).map(|(_, w)| w.at).collect();
    let mut gaps = std::collections::HashMap::new();
    for p in bursts.windows(2) { if (147..=2205).contains(&(p[1] - p[0])) { *gaps.entry(p[1] - p[0]).or_insert(0usize) += 1 } }
    let period = match gaps.iter().max_by_key(|(g, n)| (**n, std::cmp::Reverse(**g))) {
        _ if period.is_some() => period.unwrap(),
        Some((&g, _)) => g as f64,
        None => 735.0,
    };
    // Frame phase: where in the period the bursts mostly fall.
    let mut hist = [0usize; 32];
    for &b in &bursts { hist[((b as f64 % period) / period * 32.0) as usize & 31] += 1 }
    // (Whole frames, so captured samples line up with SiON's output frames.)
    let phase = phase.unwrap_or_else(|| ((0..32).max_by_key(|&k| hist[k]).unwrap_or(0) as f64 / 32.0 * period).round());
    for c in &mut chips { c.set_fps(44100.0 / period) }

    let mut st = Stream { chans: vec![vec![]; chips.iter().map(|c| c.chans()).sum()], ..Stream::default() };
    let mut defs = Defs::default();
    let frames = ((end as f64 - phase) / period).ceil().max(1.0) as usize;
    let (mut wi, mut now, mut row) = (0, 0f64, vec![]);
    for k in 0..frames {
        let snap = phase + (k as f64 + 0.5) * period;
        while wi < writes.len() && (writes[wi].at as f64) < snap {
            let w = &writes[wi];
            let dt = w.at as f64 - now;
            if dt > 0.0 { for c in &mut chips { c.run(dt / 44100.0) } now = w.at as f64 }
            chips[w.chip].at(w.at as f64);
            match (w.block, w.src) {
                (Some(b), _) => { let (ty, at, bytes) = &blocks[b]; chips[w.chip].data(*ty, *at, bytes) }
                (None, Some(src)) => chips[w.chip].write_src(w.port, w.reg, w.val, src),
                (None, None) => chips[w.chip].write(w.port, w.reg, w.val),
            }
            wi += 1;
        }
        for c in &mut chips { c.run((snap - now) / 44100.0) }
        now = snap;
        row.clear();
        for (c, &l) in chips.iter_mut().zip(&level) {
            let from = row.len();
            c.frame(&mut st, &mut defs, &mut row);
            for f in row[from..].iter_mut().flatten() { f.amp *= l * side(f.pan) }
        }
        for (ch, f) in st.chans.iter_mut().zip(&row) { ch.push(*f) }
    }
    for c in &chips { warnings.extend(c.warnings()) }
    let ch0 = st.chans.len() as u32;
    let mut hits = vec![];
    for (k, c) in chips.iter_mut().enumerate() {
        c.at(end as f64);
        hits.extend(c.hits(&mut defs, end as f64).into_iter().map(|mut h| {
            let g = level[k] * side(h.pan);
            h.amp *= g;
            for v in &mut h.vols { v.1 *= g }
            (k, h)
        }));
    }

    let mut notes = st.notes(0, 2);
    let fine = !hits.is_empty();
    // Rows: the longest frame count (up to 12) that most notes start on a multiple of, as a 1/16 note.
    let (mut rowlen, mut shift) = (6u64, 0u64);
    if !notes.is_empty() {
        for r in (2..=12u64).rev() {
            let mut count = vec![0usize; r as usize];
            for n in &notes { count[(n.start % r) as usize] += 1 }
            let (best, &c) = count.iter().enumerate().max_by_key(|x| x.1).unwrap();
            if c * 100 >= notes.len() * 85 { (rowlen, shift) = (r, (r - best as u64) % r); break }
        }
    }
    // With samples, count in SiON's own ticks (480 per quarter note, a row being a 1/16 note: `sub` ticks per
    // frame), so a sample note ends within a twentieth of a frame of when its sound did.
    let sub = if fine && 120 % rowlen == 0 { 120 / rowlen } else { 1 };
    for n in &mut notes {
        (n.start, n.end) = ((n.start + shift) * sub, (n.end + shift) * sub);
        for b in &mut n.bends { b.0 = (b.0 + shift) * sub }
        for v in &mut n.vols { v.0 = (v.0 + shift) * sub }
        for l in &mut n.lfos { l.0 = (l.0 + shift) * sub }
        for w in &mut n.waves { w.0 = (w.0 + shift) * sub }
        for p in &mut n.pans { p.0 = (p.0 + shift) * sub }
        if let Some(c) = &mut n.cut { *c = (*c + shift) * sub }
        // Release pitch per driver frame → per SiON frame (1/60 s, when key-off tables step).
        if !n.rel.is_empty() && period != 735.0 {
            let len = (n.rel.len() as f64 * period / 735.0).ceil() as usize;
            n.rel = (0..len).map(|j| n.rel[((j as f64 * 735.0 / period) as usize).min(n.rel.len() - 1)]).collect();
        }
    }
    let note = |(k, h): &(usize, Hit)| hit_note(h, ch0 + 16 * *k as u32 + h.voice as u32, phase, period / sub as f64, shift * sub);
    notes.extend(hits.iter().filter(|(_, h)| !h.dmc).filter_map(note));
    // NES DPCM hits: the channel holds the level between them and plays a sample on until the next one starts,
    // so they share one track: each note ends by the time the next starts (where its sample may play on).
    let mut dmc: Vec<Note> = hits.iter().filter(|(_, h)| h.dmc).filter_map(note).collect();
    dmc.sort_by_key(|n| (n.ch, n.start));
    for i in 1..dmc.len() {
        let (ch, start) = (dmc[i].ch, dmc[i].start);
        let n = &mut dmc[i - 1];
        if n.ch == ch && n.end > start {
            n.end = start.max(n.start + 1);
            let end = n.end;
            n.bends.retain(|b| b.0 < end);
            n.vols.retain(|v| v.0 < end);
        }
    }
    notes.extend(dmc);
    if defs.pcm.len() >= 256 { warnings.push("more than 256 different samples: the rest are left out".into()) }
    let ppq = 4 * (rowlen * sub) as u32;
    let bpm = 60.0 * 44100.0 / period / (4 * rowlen) as f64;
    let end = (frames as u64 + shift) * sub;
    // Loop back to the first frame that includes the writes at the loop point (if the loop is still there
    // once frames are rounded).
    let lp = loop_at.map(|at| ((((at as f64 - phase) / period - 0.5).floor() + 1.0).max(0.0) as u64 + shift) * sub).filter(|&l| l < end);
    let mut header = defs.header();
    // Mega Drive chips: libvgm's output (resampled from the chips' own rates) is about 3 dB down above 9 kHz from
    // SiON's, which renders at 44.1 kHz; a high shelf on the master matches it (measured on two songs).
    if shelf { header.push("#EFFECT0{hb9000,1,-3}".into()) }
    Ok((Score { ppq, tempos: vec![(0, bpm)], header, notes, mix: vec![], end, lp, fine, db: !fine, kmilli: false, ..Score::default() }, warnings))
}

/// Chips pan by switching each side on or off: a channel on one side is as loud there as a centred one. SiON pans
/// at constant power (a centred note at 0.71 on each side, one on one side at 1), so one side is 3 dB down.
fn side(pan: i32) -> f32 { if pan == 0 { 1.0 } else { std::f32::consts::FRAC_1_SQRT_2 } }

/// A sample hit as a `%7` note on channel `ch`, in ticks of `period` VGM frames from `phase` (then `shift` ticks
/// later): it starts on the tick at or before its time and waits the rest (a negative `@ph`), so it sounds exactly
/// when it did.
fn hit_note(h: &Hit, ch: u32, phase: f64, period: f64, shift: u64) -> Option<Note> {
    let tick = |t: f64| ((t - phase) / period).floor().max(0.0) as u64;
    let start = tick(h.at);
    let end = (((h.end - phase) / period).ceil().max(0.0) as u64).max(start + 1);
    let key = h.key.round().clamp(0.0, 127.0);
    let rate = h.rate * 2f64.powf((h.key - 60.0) / 12.0);
    let mut n = Note::new(ch, start, end, key as u8, h.amp, "%7".into());
    n.env = format!("@{}s63", h.smp);
    n.pan = h.pan;
    n.k0 = ((h.key - key) * 64.0).round() as i32;
    n.ofs = ((h.ofs + (start as f64 * period + phase - h.at) * rate / 44100.0) * 1000.0).round() / 1000.0;
    // A hit that stops partway into its sample plays just as many frames (from the note's start, its wait
    // included): exact, where its key-off can only fall on a tick.
    if h.cut { n.plen = (((h.end - start as f64 * period - phase) * rate / 44100.0) * 1000.0).round() / 1000.0 }
    for &(t, a) in &h.vols { let t = tick(t); if t > start && t < end { n.vols.push((t, a)) } }
    for &(t, k) in &h.keys { let t = tick(t); if t > start && t < end { n.bends.push((t, ((k - key) * 64.0).round() as i32)) } }
    (n.start, n.end) = (n.start + shift, n.end + shift);
    for b in &mut n.bends { b.0 += shift }
    for v in &mut n.vols { v.0 += shift }
    (h.end > h.at).then_some(n)
}
