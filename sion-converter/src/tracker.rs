//! Tracker module loaders: ProTracker-style MOD (and its multichannel variants), Scream Tracker 3 S3M,
//! FastTracker 2 XM and Impulse Tracker IT, into one `Module` that `play.rs` performs.
//!
//! Notes are numbered 0..119; after the instrument's note map and `note_ofs`, 48 is the reference note a
//! sample's `c4` rate is given for (MOD C-2, S3M and XM C-4, IT C-5). Effects are translated to one set with Impulse Tracker semantics (`Fx`).

use crate::bytes::{byte, u16be, u16le, u32le};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Mod, S3m, Xm, It }

pub struct Module {
    pub kind: Kind,
    pub chans: usize,
    /// Pattern numbers; `SKIP` entries are passed over and `END` ends the song.
    pub orders: Vec<u16>,
    /// The order the song goes back to after its last one (XM and MOD restart position; otherwise the first).
    pub restart: usize,
    pub patterns: Vec<Pattern>,
    /// Indexed by a cell's instrument number - 1.
    pub instruments: Vec<Instrument>,
    pub samples: Vec<Sample>,
    pub speed: u32,
    pub tempo: u32,
    /// 0..128.
    pub gvol: u32,
    /// Initial channel panning (-64..64) and volume (0..64).
    pub pan: Vec<i32>,
    pub chvol: Vec<u8>,
    /// Frequencies slide in 1/64 semitones (XM linear, IT linear); otherwise in Amiga periods.
    pub linear: bool,
    /// IT "compatible Gxx": tone portamento keeps its own effect memory.
    pub compat_g: bool,
    /// Added to a note after the instrument's note map, to make the reference note 48 (IT: -12).
    pub note_ofs: i32,
    /// What the loader had to leave out.
    pub warnings: Vec<String>,
    /// Song title (may be empty).
    pub title: String,
}

pub const SKIP: u16 = 0xfffe;
pub const END: u16 = 0xffff;

pub struct Pattern { pub rows: usize, pub cells: Vec<Cell> }

/// Notes: 0 none, 1..=120 note + 1, `CUT`, `OFF`, `FADE`.
pub const CUT: u8 = 254;
pub const OFF: u8 = 255;
pub const FADE: u8 = 253;

#[derive(Clone, Copy, Debug, Default)]
pub struct Cell { pub note: u8, pub ins: u8, pub vol: Vol, pub fx: Fx }

/// Volume column.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Vol {
    #[default]
    None,
    Set(u8),
    Pan(u8),
    SlideUp(u8), SlideDown(u8), FineUp(u8), FineDown(u8),
    PortaUp(u8), PortaDown(u8), TonePorta(u8),
    VibSpeed(u8), Vibrato(u8),
    PanLeft(u8), PanRight(u8),
}

/// Effects (Impulse Tracker letters where they exist). A zero parameter recalls the effect's memory where
/// the format has one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Fx {
    #[default]
    None,
    Speed(u8), Tempo(u8), Jump(u8), Break(u8),
    /// `Dxy`: x0 up, 0y down, xF fine up, Fy fine down.
    VolSlide(u8),
    /// `Exx` / `Fxx`: coarse, `EFx` fine, `EEx` extra fine.
    PortaDown(u8), PortaUp(u8),
    /// XM / MOD fine (E1x, E2x) and extra fine (X1x, X2x) slides.
    FinePortaUp(u8), FinePortaDown(u8), XFinePortaUp(u8), XFinePortaDown(u8),
    /// XM / MOD fine volume slides (EAx, EBx).
    FineVolUp(u8), FineVolDown(u8),
    TonePorta(u8), Vibrato(u8), FineVibrato(u8), TonePortaVol(u8), VibratoVol(u8),
    /// Note volume (MOD / XM `Cxx`, 0..64); IT channel volume (`Mxx`, 0..64).
    SetVol(u8),
    Tremor(u8), Arp(u8), ChanVol(u8), ChanVolSlide(u8), Offset(u8),
    /// `Pxy`: x0 left... as IT: P0x right, Px0 left.
    PanSlide(u8),
    Retrig(u8), Tremolo(u8),
    /// 0..255.
    Pan(u8),
    /// 0..128.
    GlobalVol(u8),
    GlobalVolSlide(u8),
    /// `Sxy` sub-effects.
    NoteCut(u8), NoteDelay(u8), PatDelay(u8), PatLoop(u8), FinePatDelay(u8), SetFinetune(u8),
    VibWave(u8), TremWave(u8), HighOffset(u8),
    /// NNA / envelope controls (`S7x`).
    Past(u8),
    KeyOff(u8), EnvPos(u8),
    TempoSlide(u8),
}

#[derive(Clone, Default)]
pub struct Env {
    /// (tick, value 0..64 for volume, -32..32 for panning).
    pub pts: Vec<(u16, i8)>,
    pub sustain: Option<(usize, usize)>,
    pub lp: Option<(usize, usize)>,
}

impl Env {
    /// Value at `t` ticks.
    pub fn at(&self, t: u32) -> f32 {
        let p = &self.pts;
        let Some(&(last, lv)) = p.last() else { return 64.0 };
        if t >= last as u32 { return lv as f32 }
        let i = p.partition_point(|x| x.0 as u32 <= t).max(1);
        let ((x0, y0), (x1, y1)) = (p[i - 1], p[i]);
        if x1 <= x0 { return y1 as f32 }
        y0 as f32 + (y1 - y0) as f32 * (t - x0 as u32) as f32 / (x1 - x0) as f32
    }
}

#[derive(Clone)]
pub struct Instrument {
    /// Per note: (note to play, sample number + 1, 0 = none).
    pub map: Vec<(u8, u16)>,
    pub venv: Option<Env>,
    pub penv: Option<Env>,
    /// Volume lost per tick after key-off, of 65536 (XM, IT).
    pub fadeout: u32,
    /// 0..128.
    pub gvol: u32,
    pub pan: Option<i32>,
    /// New note action (IT): 0 cut, 1 continue, 2 note off, 3 fade.
    pub nna: u8,
    /// Instrument (or MOD/S3M sample) name, trimmed (may be empty).
    pub name: String,
}

#[derive(Clone, Default)]
pub struct Sample {
    pub data: Vec<f32>,
    /// Loop [start, end), and whether it bounces back and forth.
    pub lp: Option<(usize, usize, bool)>,
    /// 0..64.
    pub vol: u8,
    /// 0..64 (IT sample global volume).
    pub gvol: u8,
    pub pan: Option<i32>,
    /// Playback rate (Hz) at note 48.
    pub c4: f64,
}

/// Text field of `n` bytes at `o`: up to the first NUL, trimmed, printable ASCII only.
fn text(d: &[u8], o: usize, n: usize) -> String {
    let b = d.get(o..(o + n).min(d.len())).unwrap_or(&[]);
    let b = &b[..b.iter().position(|&c| c == 0).unwrap_or(b.len())];
    b.iter().map(|&c| if (32..127).contains(&c) { c as char } else { ' ' }).collect::<String>().trim().to_string()
}

pub fn detect(d: &[u8]) -> Option<Kind> {
    if d.starts_with(b"IMPM") { return Some(Kind::It) }
    if d.starts_with(b"Extended Module: ") { return Some(Kind::Xm) }
    if d.get(0x2c..0x30) == Some(b"SCRM") { return Some(Kind::S3m) }
    if mod_chans(d).is_some() { return Some(Kind::Mod) }
    None
}

pub fn load(d: &[u8]) -> Result<Module, String> {
    match detect(d) {
        Some(Kind::Mod) => load_mod(d),
        Some(Kind::S3m) => load_s3m(d),
        Some(Kind::Xm) => load_xm(d),
        Some(Kind::It) => load_it(d),
        None => Err("not a tracker module".into()),
    }
}

/// One instrument per sample, playing it on every note (MOD, S3M).
fn plain_instruments(names: Vec<String>) -> Vec<Instrument> {
    names.into_iter().enumerate().map(|(s, name)| Instrument { map: (0..120).map(|k| (k as u8, s as u16 + 1)).collect(), venv: None, penv: None, fadeout: 0, gvol: 128, pan: None, nna: 0, name }).collect()
}

// ---------------------------------------------------------------------------------------------------------
// MOD

fn mod_chans(d: &[u8]) -> Option<usize> {
    let tag = d.get(1080..1084)?;
    let n = match tag {
        b"M.K." | b"M!K!" | b"M&K!" | b"FLT4" | b"4CHN" | b"N.T." => 4,
        b"FLT8" | b"OKTA" | b"CD81" => 8,
        [a @ b'1'..=b'9', b'C', b'H', b'N'] => (a - b'0') as usize,
        [a @ b'1'..=b'9', b @ b'0'..=b'9', b'C', b'H' | b'N'] => ((a - b'0') * 10 + b - b'0') as usize,
        _ => return None,
    };
    Some(n)
}

/// ProTracker effect → `Fx` (also used for XM, whose effects extend these).
fn mod_fx(fx: u8, p: u8, xm: bool) -> Fx {
    let (x, y) = (p >> 4, p & 15);
    match fx {
        0 if p != 0 => Fx::Arp(p),
        1 if p != 0 || xm => Fx::PortaUp(p),
        2 if p != 0 || xm => Fx::PortaDown(p),
        3 => Fx::TonePorta(p),
        4 => Fx::Vibrato(p),
        5 => Fx::TonePortaVol(xy_slide(p)),
        6 => Fx::VibratoVol(xy_slide(p)),
        7 => Fx::Tremolo(p),
        8 => Fx::Pan(p),
        9 => Fx::Offset(p),
        10 if p != 0 || xm => Fx::VolSlide(xy_slide(p)),
        11 => Fx::Jump(p),
        12 => Fx::SetVol(p.min(64)),
        13 => Fx::Break(x * 10 + y),
        14 => match x {
            1 if y != 0 || xm => Fx::FinePortaUp(y),
            2 if y != 0 || xm => Fx::FinePortaDown(y),
            4 => Fx::VibWave(y),
            5 => Fx::SetFinetune(y),
            6 => Fx::PatLoop(y),
            7 => Fx::TremWave(y),
            8 => Fx::Pan(y * 17),
            9 => Fx::Retrig(y),
            10 if y != 0 || xm => Fx::FineVolUp(y),
            11 if y != 0 || xm => Fx::FineVolDown(y),
            12 => Fx::NoteCut(y),
            13 => Fx::NoteDelay(y),
            14 => Fx::PatDelay(y),
            _ => Fx::None,
        },
        15 if p == 0 => Fx::None,
        15 if p < 32 => Fx::Speed(p),
        15 => Fx::Tempo(p),
        _ => Fx::None,
    }
}

/// MOD / XM `Axy` (x up, y down; up wins) as an IT `Dxy`.
fn xy_slide(p: u8) -> u8 { if p >> 4 != 0 { p & 0xf0 } else { p & 15 } }

fn load_mod(d: &[u8]) -> Result<Module, String> {
    let chans = mod_chans(d).ok_or("unknown MOD format")?;
    let mut samples = vec![];
    for i in 0..31 {
        let o = 20 + 30 * i;
        let (len, ft, vol, ls, ll) = (u16be(d, o + 22) as usize * 2, byte(d, o + 24) & 15, byte(d, o + 25).min(64), u16be(d, o + 26) as usize * 2, u16be(d, o + 28) as usize * 2);
        let ft = if ft > 7 { ft as i32 - 16 } else { ft as i32 };
        samples.push((len, ft, vol, ls, ll));
    }
    let songlen = (byte(d, 950) as usize).clamp(1, 128);
    let orders: Vec<u16> = d[952..952 + 128].iter().take(songlen).map(|&o| o as u16).collect();
    let npat = d[952..952 + 128].iter().max().copied().unwrap_or(0) as usize + 1;
    let (mut patterns, mut o) = (vec![], 1084);
    for _ in 0..npat {
        let mut cells = vec![];
        for _ in 0..64 * chans {
            let b = d.get(o..o + 4).unwrap_or(&[0; 4]);
            o += 4;
            let ins = (b[0] & 0xf0) | (b[2] >> 4);
            let period = ((b[0] & 15) as u32) << 8 | b[1] as u32;
            let note = if period == 0 { 0 } else { (48.0 + 12.0 * (428.0 / period as f64).log2()).round().clamp(0.0, 119.0) as u8 + 1 };
            cells.push(Cell { note, ins, vol: Vol::None, fx: mod_fx(b[2] & 15, b[3], false) });
        }
        patterns.push(Pattern { rows: 64, cells });
    }
    let mut smp = vec![];
    for (len, ft, vol, ls, ll) in samples {
        let data: Vec<f32> = d.get(o..(o + len).min(d.len())).unwrap_or(&[]).iter().map(|&b| b as i8 as f32 / 128.0).collect();
        o += len;
        let lp = (ll > 2 && ls + ll <= data.len().max(ls + ll) && ls < data.len()).then(|| (ls, (ls + ll).min(data.len()), false));
        // ProTracker plays C-2 at 3546895 / 428 Hz; finetune steps are 1/8 semitone.
        smp.push(Sample { data, lp, vol, gvol: 64, pan: None, c4: 3546895.0 / 428.0 * 2f64.powf(ft as f64 / 96.0) });
    }
    // Amiga hard panning (LRRL), narrowed so it isn't extreme on headphones.
    let pan = (0..chans).map(|c| if matches!(c % 4, 0 | 3) { -40 } else { 40 }).collect();
    Ok(Module {
        kind: Kind::Mod, chans, restart: (byte(d, 951) as usize).min(127), orders, patterns, instruments: plain_instruments((0..31).map(|i| text(d, 20 + 30 * i, 22)).collect()), samples: smp, speed: 6, tempo: 125, gvol: 128,
        pan, chvol: vec![64; chans], linear: false, compat_g: false, note_ofs: 0, warnings: vec![], title: text(d, 0, 20),
    })
}

// ---------------------------------------------------------------------------------------------------------
// S3M / IT effects (letters A..Z = 1..26)

fn it_fx(cmd: u8, p: u8, s3m: bool) -> Fx {
    let (x, y) = (p >> 4, p & 15);
    match cmd {
        1 if p != 0 => Fx::Speed(p),
        2 => Fx::Jump(p),
        3 => Fx::Break(if s3m { x * 10 + y } else { p }),
        4 => Fx::VolSlide(p),
        5 => Fx::PortaDown(p),
        6 => Fx::PortaUp(p),
        7 => Fx::TonePorta(p),
        8 => Fx::Vibrato(p),
        9 => Fx::Tremor(p),
        10 => Fx::Arp(p),
        11 => Fx::VibratoVol(p),
        12 => Fx::TonePortaVol(p),
        13 => Fx::ChanVol(p.min(64)),
        14 => Fx::ChanVolSlide(p),
        15 => Fx::Offset(p),
        16 => Fx::PanSlide(p),
        17 => Fx::Retrig(p),
        18 => Fx::Tremolo(p),
        19 => match x {
            1..=2 => Fx::None,
            3 => Fx::VibWave(y),
            4 => Fx::TremWave(y),
            6 => Fx::FinePatDelay(y),
            7 => Fx::Past(y),
            8 => Fx::Pan(y * 17),
            10 => Fx::HighOffset(y),
            11 => Fx::PatLoop(y),
            12 => Fx::NoteCut(y),
            13 => Fx::NoteDelay(y),
            14 => Fx::PatDelay(y),
            _ => Fx::None,
        },
        20 if p >= 0x20 => Fx::Tempo(p),
        20 => Fx::TempoSlide(p),
        21 => Fx::FineVibrato(p),
        22 => Fx::GlobalVol(if s3m { p.min(64) * 2 } else { p.min(128) }),
        23 => Fx::GlobalVolSlide(p),
        24 => Fx::Pan(p),
        _ => Fx::None,
    }
}

// ---------------------------------------------------------------------------------------------------------
// S3M

fn load_s3m(d: &[u8]) -> Result<Module, String> {
    let (ordnum, insnum, patnum, flags, ffi) = (u16le(d, 0x20) as usize, u16le(d, 0x22) as usize, u16le(d, 0x24) as usize, u16le(d, 0x26), u16le(d, 0x2a));
    let (gv, speed, tempo, mv) = (byte(d, 0x30), byte(d, 0x31), byte(d, 0x32), byte(d, 0x33));
    let _ = flags;
    let settings = d.get(0x40..0x60).ok_or("truncated S3M header")?;
    let orders: Vec<u16> = (0..ordnum).map(|i| match byte(d, 0x60 + i) { 255 => END, 254 => SKIP, o => o as u16 }).collect();
    let ip = 0x60 + ordnum;
    let pp = ip + insnum * 2;
    let para = |o: usize| u16le(d, o) as usize * 16;
    // Channels: enabled ones (settings < 16 are PCM channels); panning from the table when present.
    let used: Vec<usize> = (0..32).filter(|&c| settings[c] < 16).collect();
    let chans = used.iter().max().map_or(0, |c| c + 1);
    let stereo = mv & 0x80 != 0;
    let mut pan: Vec<i32> = (0..chans).map(|c| if !stereo { 0 } else if settings[c] & 15 < 8 { -32 } else { 32 }).collect();
    if byte(d, 0x35) == 252 {
        let pt = pp + patnum * 2;
        for (c, p) in pan.iter_mut().enumerate() { let v = byte(d, pt + c); if v & 0x20 != 0 && stereo { *p = (v & 15) as i32 * 128 / 15 - 64 } }
    }
    let (mut samples, mut adlib) = (vec![], false);
    for i in 0..insnum {
        let o = para(ip + i * 2);
        let ty = byte(d, o);
        if (2..=7).contains(&ty) { adlib = true }
        if ty != 1 { samples.push(Sample::default()); continue }
        let mem = ((byte(d, o + 13) as usize) << 16 | u16le(d, o + 14) as usize) * 16;
        let (len, ls, le, vol, fl, c2) = (u32le(d, o + 16) as usize, u32le(d, o + 20) as usize, u32le(d, o + 24) as usize, byte(d, o + 28).min(64), byte(d, o + 31), u32le(d, o + 32));
        let (bits16, stereo) = (fl & 4 != 0, fl & 2 != 0);
        let bytes = if bits16 { 2 } else { 1 };
        let mut data = Vec::with_capacity(len);
        for k in 0..len {
            let at = mem + k * bytes;
            let v = if bits16 { u16le(d, at) as i32 } else { byte(d, at) as i32 };
            let v = if ffi == 2 { v - if bits16 { 32768 } else { 128 } } else if bits16 { v as i16 as i32 } else { v as i8 as i32 };
            data.push(v as f32 / if bits16 { 32768.0 } else { 128.0 });
        }
        let _ = stereo;
        let lp = (fl & 1 != 0 && le > ls + 1 && ls < len).then(|| (ls, le.min(len), false));
        samples.push(Sample { data, lp, vol, gvol: 64, pan: None, c4: c2.max(1) as f64 });
    }
    let mut patterns = vec![];
    for p in 0..patnum {
        let mut cells = vec![Cell::default(); 64 * chans];
        let mut o = para(pp + p * 2);
        if o == 0 { patterns.push(Pattern { rows: 64, cells }); continue }
        o += 2;
        let mut row = 0;
        while row < 64 && o < d.len() {
            let what = d[o];
            o += 1;
            if what == 0 { row += 1; continue }
            let c = (what & 31) as usize;
            let mut cell = Cell::default();
            if what & 32 != 0 {
                let (n, ins) = (byte(d, o), byte(d, o + 1));
                o += 2;
                cell.note = match n { 255 => 0, 254 => CUT, n => ((n >> 4) * 12 + (n & 15)).min(119) + 1 };
                cell.ins = ins;
            }
            if what & 64 != 0 { cell.vol = Vol::Set(byte(d, o).min(64)); o += 1 }
            if what & 128 != 0 { cell.fx = it_fx(byte(d, o), byte(d, o + 1), true); o += 2 }
            if c < chans { cells[row * chans + c] = cell }
        }
        patterns.push(Pattern { rows: 64, cells });
    }
    Ok(Module {
        kind: Kind::S3m, chans, restart: 0, orders, patterns, instruments: plain_instruments((0..insnum).map(|i| text(d, para(ip + i * 2) + 0x30, 28)).collect()), samples, speed: speed.max(1) as u32, tempo: tempo.max(32) as u32,
        gvol: (gv.min(64) as u32) * 2, pan, chvol: vec![64; chans], linear: false, compat_g: false, note_ofs: 0,
        warnings: if adlib { vec!["AdLib (FM) instruments in S3M files are not converted".into()] } else { vec![] },
        title: text(d, 0, 28),
    })
}

// ---------------------------------------------------------------------------------------------------------
// XM

fn xm_env(d: &[u8], pts: usize, n: u8, sus: u8, ls: u8, le: u8, flags: u8, pan: bool) -> Option<Env> {
    if flags & 1 == 0 || n == 0 { return None }
    let p = (0..(n as usize).min(12)).map(|i| (u16le(d, pts + i * 4), (u16le(d, pts + i * 4 + 2) as i32 - if pan { 32 } else { 0 }).clamp(-64, 64) as i8)).collect();
    Some(Env { pts: p, sustain: (flags & 2 != 0).then_some((sus as usize, sus as usize)), lp: (flags & 4 != 0).then_some((ls as usize, le as usize)) })
}

fn load_xm(d: &[u8]) -> Result<Module, String> {
    let h = 60;
    let hsize = u32le(d, h) as usize;
    let (songlen, restart, chans, npat, nins, flags, speed, tempo) =
        (u16le(d, h + 4) as usize, u16le(d, h + 6), u16le(d, h + 8) as usize, u16le(d, h + 10) as usize, u16le(d, h + 12) as usize, u16le(d, h + 14), u16le(d, h + 16), u16le(d, h + 18));
    if chans == 0 || chans > 64 { return Err("bad XM channel count".into()) }
    let orders: Vec<u16> = (0..songlen.min(256)).map(|i| byte(d, h + 20 + i) as u16).collect();
    let mut o = h + hsize;
    let mut patterns = vec![];
    for _ in 0..npat {
        let (plen, rows, size) = (u32le(d, o) as usize, u16le(d, o + 5) as usize, u16le(d, o + 7) as usize);
        let rows = if rows == 0 { 64 } else { rows };
        let mut p = o + plen;
        let end = p + size;
        o = end;
        let mut cells = vec![Cell::default(); rows * chans];
        if size == 0 { patterns.push(Pattern { rows, cells }); continue }
        for cell in cells.iter_mut() {
            if p >= end { break }
            let first = byte(d, p);
            let mut f = [0u8; 5];
            if first & 0x80 != 0 {
                p += 1;
                for (k, v) in f.iter_mut().enumerate() { if first >> k & 1 != 0 { *v = byte(d, p); p += 1 } }
            } else {
                for (k, v) in f.iter_mut().enumerate() { *v = byte(d, p + k) }
                p += 5;
            }
            let [n, ins, v, fx, prm] = f;
            cell.note = match n { 97 => OFF, 1..=96 => n, _ => 0 };
            cell.ins = ins;
            cell.vol = match v >> 4 {
                1..=4 => Vol::Set((v - 16).min(64)),
                5 if v == 0x50 => Vol::Set(64),
                6 => Vol::SlideDown(v & 15), 7 => Vol::SlideUp(v & 15), 8 => Vol::FineDown(v & 15), 9 => Vol::FineUp(v & 15),
                10 => Vol::VibSpeed(v & 15), 11 => Vol::Vibrato(v & 15), 12 => Vol::Pan(((v & 15) as u32 * 64 / 15) as u8),
                13 => Vol::PanLeft(v & 15), 14 => Vol::PanRight(v & 15), 15 => Vol::TonePorta((v & 15) << 4),
                _ => Vol::None,
            };
            let (x, y) = (prm >> 4, prm & 15);
            cell.fx = match fx {
                0..=15 => mod_fx(fx, prm, true),
                16 => Fx::GlobalVol(prm.min(64) * 2),
                17 => Fx::GlobalVolSlide(xy_slide(prm)),
                20 => Fx::KeyOff(prm),
                21 => Fx::EnvPos(prm),
                25 => Fx::PanSlide(if x != 0 { x } else { y << 4 }), // XM P: x0 right, 0y left
                27 => Fx::Retrig(prm),
                29 => Fx::Tremor(prm),
                33 => match x { 1 => Fx::XFinePortaUp(y), 2 => Fx::XFinePortaDown(y), _ => Fx::None },
                _ => Fx::None,
            };
            // XM Cxx sets the volume directly, like the volume column.
            if let (Fx::SetVol(v), Vol::None) = (cell.fx, cell.vol) { (cell.vol, cell.fx) = (Vol::Set(v), Fx::None) }
        }
        patterns.push(Pattern { rows, cells });
    }
    let (mut instruments, mut samples) = (vec![], vec![]);
    for _ in 0..nins {
        let isize = u32le(d, o) as usize;
        let ns = u16le(d, o + 27) as usize;
        let mut ins = Instrument { map: vec![(0, 0); 120], venv: None, penv: None, fadeout: 0, gvol: 128, pan: None, nna: 0, name: text(d, o + 4, 22) };
        let mut shdr = o + isize;
        if ns > 0 {
            let b = o + 29;
            let ssize = u32le(d, b) as usize;
            let first = samples.len();
            for k in 0..96 { ins.map[k] = (k as u8, (first + byte(d, b + 4 + k) as usize + 1) as u16) }
            for k in 96..120 { ins.map[k] = (k as u8, ins.map[95].1) }
            let e = b + 4 + 96;
            ins.venv = xm_env(d, e, byte(d, e + 96), byte(d, e + 98), byte(d, e + 99), byte(d, e + 100), byte(d, e + 104), false);
            ins.penv = xm_env(d, e + 48, byte(d, e + 97), byte(d, e + 101), byte(d, e + 102), byte(d, e + 103), byte(d, e + 105), true);
            ins.fadeout = u16le(d, e + 110) as u32 * 2;
            let mut heads = vec![];
            for _ in 0..ns {
                heads.push((u32le(d, shdr) as usize, u32le(d, shdr + 4) as usize, u32le(d, shdr + 8) as usize, byte(d, shdr + 12), byte(d, shdr + 13) as i8, byte(d, shdr + 14), byte(d, shdr + 15), byte(d, shdr + 16) as i8));
                shdr += ssize.max(40);
            }
            let mut p = shdr;
            for (len, ls, ll, vol, ft, ty, pan, rel) in heads {
                let bits16 = ty & 16 != 0;
                let (mut acc, mut data) = (0i32, Vec::with_capacity(if bits16 { len / 2 } else { len }));
                if bits16 {
                    for k in 0..len / 2 { acc = (acc + u16le(d, p + k * 2) as i16 as i32) as i16 as i32; data.push(acc as f32 / 32768.0) }
                } else {
                    for k in 0..len { acc = (acc + byte(d, p + k) as i8 as i32) as i8 as i32; data.push(acc as f32 / 128.0) }
                }
                p += len;
                let (ls, ll) = if bits16 { (ls / 2, ll / 2) } else { (ls, ll) };
                let lp = (ty & 3 != 0 && ll > 1 && ls < data.len()).then(|| (ls, (ls + ll).min(data.len()), ty & 3 == 2));
                samples.push(Sample { data, lp, vol: vol.min(64), gvol: 64, pan: Some(pan as i32 / 2 - 64), c4: 8363.0 * 2f64.powf((rel as f64 + ft as f64 / 128.0) / 12.0) });
            }
            o = p;
        } else {
            o += isize;
        }
        instruments.push(ins);
    }
    Ok(Module {
        kind: Kind::Xm, chans, restart: restart as usize, orders, patterns, instruments, samples, speed: speed.max(1) as u32, tempo: tempo.max(32) as u32, gvol: 128,
        pan: vec![0; chans], chvol: vec![64; chans], linear: flags & 1 != 0, compat_g: false, note_ofs: 0, warnings: vec![], title: text(d, 17, 20),
    })
}

// ---------------------------------------------------------------------------------------------------------
// IT

fn it_env(d: &[u8], o: usize, pan: bool) -> Option<Env> {
    let (fl, n, lb, le, sb, se) = (byte(d, o), byte(d, o + 1) as usize, byte(d, o + 2) as usize, byte(d, o + 3) as usize, byte(d, o + 4) as usize, byte(d, o + 5) as usize);
    if fl & 1 == 0 || n == 0 { return None }
    let pts = (0..n.min(25)).map(|i| (u16le(d, o + 7 + i * 3), (byte(d, o + 6 + i * 3) as i8).clamp(if pan { -32 } else { 0 }, if pan { 32 } else { 64 }))).collect();
    Some(Env { pts, lp: (fl & 2 != 0).then_some((lb, le)), sustain: (fl & 4 != 0).then_some((sb, se)) })
}

/// IT 2.14 / 2.15 compressed sample data.
fn it_unpack(d: &[u8], mut o: usize, len: usize, bits16: bool, it215: bool) -> Vec<f32> {
    let mut out = Vec::with_capacity(len);
    let (block, top) = if bits16 { (0x4000, 17u32) } else { (0x8000, 9u32) };
    while out.len() < len && o + 2 <= d.len() {
        let size = u16le(d, o) as usize;
        let blk = &d[(o + 2).min(d.len())..(o + 2 + size).min(d.len())];
        o += 2 + size;
        let (mut pos, mut width, mut d1, mut d2) = (0usize, top, 0i32, 0i32);
        let read = |n: u32, pos: &mut usize| -> u32 {
            let mut v = 0u32;
            for k in 0..n { let b = *pos + k as usize; v |= ((blk.get(b / 8).copied().unwrap_or(0) as u32 >> (b % 8)) & 1) << k }
            *pos += n as usize;
            v
        };
        let count = block.min(len - out.len());
        let mut k = 0;
        while k < count && pos < blk.len() * 8 {
            let v = read(width, &mut pos);
            let mut value = None;
            if width < 7 {
                if v == 1 << (width - 1) {
                    let nw = read(if bits16 { 4 } else { 3 }, &mut pos) + 1;
                    width = if nw >= width { nw + 1 } else { nw };
                } else { value = Some((v, width)) }
            } else if width < top {
                let border = if bits16 { (0xffffu32 >> (17 - width)).wrapping_sub(8) } else { (0xffu32 >> (9 - width)).wrapping_sub(4) };
                if v > border && v <= border + (if bits16 { 16 } else { 8 }) {
                    let nw = v - border;
                    width = if nw >= width { nw + 1 } else { nw };
                } else { value = Some((v, width)) }
            } else if v & (1 << (top - 1)) != 0 {
                width = (v + 1) & 0xff;
                if width == 0 { break }
            } else { value = Some((v, top - 1)) }
            if let Some((v, w)) = value {
                // Sign-extend the w-bit value, then integrate once (twice for IT 2.15).
                let s = ((v << (32 - w)) as i32) >> (32 - w);
                if bits16 {
                    d1 = (d1 + s) as i16 as i32;
                    d2 = (d2 + d1) as i16 as i32;
                    out.push((if it215 { d2 } else { d1 }) as f32 / 32768.0);
                } else {
                    d1 = (d1 + s) as i8 as i32;
                    d2 = (d2 + d1) as i8 as i32;
                    out.push((if it215 { d2 } else { d1 }) as f32 / 128.0);
                }
                k += 1;
            }
        }
        if k == 0 { break }
    }
    out.resize(len, 0.0);
    out
}

fn load_it(d: &[u8]) -> Result<Module, String> {
    let (ordnum, insnum, smpnum, patnum) = (u16le(d, 0x20) as usize, u16le(d, 0x22) as usize, u16le(d, 0x24) as usize, u16le(d, 0x26) as usize);
    let (flags, gv, speed, tempo) = (u16le(d, 0x2c), byte(d, 0x30), byte(d, 0x32), byte(d, 0x33));
    let orders: Vec<u16> = (0..ordnum).map(|i| match byte(d, 0xc0 + i) { 255 => END, 254 => SKIP, o => o as u16 }).collect();
    let ip = 0xc0 + ordnum;
    let (sp, pp) = (ip + insnum * 4, ip + insnum * 4 + smpnum * 4);
    let use_ins = flags & 4 != 0;
    let mut samples = vec![];
    for i in 0..smpnum {
        let o = u32le(d, sp + i * 4) as usize;
        let (gvol, fl, vol, len, ls, le, c5, ptr, cvt, dfp) = (byte(d, o + 0x11), byte(d, o + 0x12), byte(d, o + 0x13), u32le(d, o + 0x30) as usize, u32le(d, o + 0x34) as usize, u32le(d, o + 0x38) as usize, u32le(d, o + 0x3c), u32le(d, o + 0x48) as usize, byte(d, o + 0x2e), byte(d, o + 0x2f));
        let bits16 = fl & 2 != 0;
        let data = if fl & 1 == 0 || len == 0 { vec![] } else if fl & 8 != 0 { it_unpack(d, ptr, len, bits16, cvt & 4 != 0) } else {
            (0..len).map(|k| {
                let v = if bits16 { u16le(d, ptr + k * 2) as i32 } else { byte(d, ptr + k) as i32 };
                let v = if cvt & 1 != 0 { if bits16 { v as i16 as i32 } else { v as i8 as i32 } } else { v - if bits16 { 32768 } else { 128 } };
                v as f32 / if bits16 { 32768.0 } else { 128.0 }
            }).collect()
        };
        let lp = (fl & 16 != 0 && le > ls + 1 && ls < data.len()).then(|| (ls, le.min(data.len()), fl & 64 != 0))
            .or_else(|| { let (ss, se) = (u32le(d, o + 0x40) as usize, u32le(d, o + 0x44) as usize); (fl & 32 != 0 && se > ss + 1 && ss < data.len()).then(|| (ss, se.min(data.len()), fl & 128 != 0)) });
        let pan = (dfp & 128 != 0).then(|| (dfp & 127).min(64) as i32 * 2 - 64);
        samples.push(Sample { data, lp, vol: vol.min(64), gvol: gvol.min(64), pan, c4: c5.max(1) as f64 });
    }
    let instruments = if use_ins {
        (0..insnum).map(|i| {
            let o = u32le(d, ip + i * 4) as usize;
            let map = (0..120).map(|k| (byte(d, o + 0x40 + k * 2).min(119), byte(d, o + 0x41 + k * 2) as u16)).collect();
            let dfp = byte(d, o + 0x19);
            Instrument {
                map, venv: it_env(d, o + 0x130, false), penv: it_env(d, o + 0x182, true), fadeout: u16le(d, o + 0x14) as u32 * 32,
                gvol: byte(d, o + 0x18).min(128) as u32, pan: (dfp & 128 == 0).then(|| dfp.min(64) as i32 * 2 - 64), nna: byte(d, o + 0x11) & 3,
                name: text(d, o + 0x20, 26),
            }
        }).collect()
    } else { plain_instruments((0..smpnum).map(|i| text(d, u32le(d, sp + i * 4) as usize + 0x14, 26)).collect()) };
    let chans_max = 64;
    let mut patterns = vec![];
    let mut chans = 1;
    for p in 0..patnum {
        let o = u32le(d, pp + p * 4) as usize;
        if o == 0 { patterns.push(Pattern { rows: 64, cells: vec![Cell::default(); 64 * chans_max] }); continue }
        let rows = u16le(d, o + 2) as usize;
        let mut cells = vec![Cell::default(); rows * chans_max];
        let (mut q, mut row) = (o + 8, 0);
        let (mut mask, mut last) = ([0u8; 64], [Cell::default(); 64]);
        let mut lastfx = [(0u8, 0u8); 64];
        while row < rows && q < d.len() {
            let cv = d[q];
            q += 1;
            if cv == 0 { row += 1; continue }
            let c = ((cv - 1) & 63) as usize;
            if cv & 128 != 0 { mask[c] = byte(d, q); q += 1 }
            let m = mask[c];
            let mut cell = Cell::default();
            if m & 1 != 0 {
                let n = byte(d, q);
                q += 1;
                last[c].note = match n { 255 => OFF, 254 => CUT, 120..=253 => FADE, n => n + 1 };
            }
            if m & 2 != 0 { last[c].ins = byte(d, q); q += 1 }
            if m & 4 != 0 {
                let v = byte(d, q);
                q += 1;
                last[c].vol = match v {
                    0..=64 => Vol::Set(v), 65..=74 => Vol::FineUp(v - 65), 75..=84 => Vol::FineDown(v - 75), 85..=94 => Vol::SlideUp(v - 85), 95..=104 => Vol::SlideDown(v - 95),
                    105..=114 => Vol::PortaDown(v - 105), 115..=124 => Vol::PortaUp(v - 115), 128..=192 => Vol::Pan(v - 128),
                    193..=202 => Vol::TonePorta([0, 1, 4, 8, 16, 32, 64, 96, 128, 255][(v - 193) as usize]), 203..=212 => Vol::Vibrato(v - 203), _ => Vol::None,
                };
            }
            if m & 8 != 0 { lastfx[c] = (byte(d, q), byte(d, q + 1)); q += 2 }
            if m & 0x11 != 0 { cell.note = last[c].note }
            if m & 0x22 != 0 { cell.ins = last[c].ins }
            if m & 0x44 != 0 { cell.vol = last[c].vol }
            if m & 0x88 != 0 { cell.fx = it_fx(lastfx[c].0, lastfx[c].1, false) }
            chans = chans.max(c + 1);
            cells[row * chans_max + c] = cell;
        }
        patterns.push(Pattern { rows, cells });
    }
    // Keep only the channels in use.
    for p in &mut patterns {
        let rows = p.rows;
        p.cells = (0..rows).flat_map(|r| p.cells[r * chans_max..r * chans_max + chans].to_vec()).collect();
    }
    let pan = (0..chans).map(|c| { let v = byte(d, 0x40 + c); if v >= 100 { 0 } else { v.min(64) as i32 * 2 - 64 } }).collect();
    // Disabled channels (pan + 128) are silent.
    let chvol = (0..chans).map(|c| if byte(d, 0x40 + c) >= 128 { 0 } else { byte(d, 0x80 + c).min(64) }).collect();
    Ok(Module {
        kind: Kind::It, chans, restart: 0, orders, patterns, instruments, samples, speed: speed.max(1) as u32, tempo: tempo.max(31) as u32, gvol: gv.min(128) as u32,
        pan, chvol, linear: flags & 8 != 0, compat_g: flags & 32 != 0, note_ofs: -12, warnings: vec![], title: text(d, 4, 26),
    })
}
