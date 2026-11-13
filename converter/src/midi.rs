//! Standard MIDI File reader (every track merged into one time-ordered event list), and its interpretation as a
//! `Score`: note pairing, sustain pedal, volume, pan, pitch bend, General MIDI instruments and drum kit.

use crate::score::{Note, Score};

pub struct Smf {
    /// Ticks per quarter note (for SMPTE files: ticks per second, with a fixed 60 BPM tempo).
    pub ppq: u32,
    pub events: Vec<Event>,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub tick: u64,
    pub msg: Msg,
}

#[derive(Clone, Copy, Debug)]
pub enum Msg {
    On { ch: u8, key: u8, vel: u8 },
    Off { ch: u8, key: u8 },
    Cc { ch: u8, num: u8, val: u8 },
    Program { ch: u8, prog: u8 },
    /// Pitch bend, -8192..8191.
    Bend { ch: u8, val: i32 },
    /// Microseconds per quarter note.
    Tempo(u32),
    /// Time signature: beats per bar, and the beat as a power of two (2: quarter notes).
    TimeSig(u8, u8),
}

fn be(b: &[u8]) -> usize { b.iter().fold(0, |a, &x| a << 8 | x as usize) }

pub fn parse(data: &[u8]) -> Result<Smf, String> {
    // Also accepts RIFF (RMID) wrapped files by looking for the header anywhere.
    let at = data.windows(4).position(|w| w == b"MThd").ok_or("not a MIDI file (no MThd header)")?;
    let d = &data[at..];
    if d.len() < 14 { return Err("truncated MIDI header".into()) }
    let div = be(&d[12..14]);
    let smpte = div & 0x8000 != 0;
    let ppq = (if smpte { (256 - (div >> 8)) * (div & 0xff) } else { div }) as u32;
    if ppq == 0 { return Err("invalid MIDI time division".into()) }

    let (mut raw, mut pos, mut trk) = (vec![], 8 + be(&d[4..8]), 0);
    while pos + 8 <= d.len() {
        let len = be(&d[pos + 4..pos + 8]);
        let body = &d[pos + 8..d.len().min(pos + 8 + len)];
        if &d[pos..pos + 4] == b"MTrk" {
            track(body, trk, &mut raw);
            trk += 1;
        }
        pos = pos.saturating_add(8 + len);
    }
    if trk == 0 { return Err("MIDI file has no tracks".into()) }
    raw.sort_by_key(|&(t, k, s, _)| (t, k, s));
    let mut events: Vec<Event> = raw.into_iter().map(|(tick, _, _, msg)| Event { tick, msg }).collect();
    if smpte {
        events.retain(|e| !matches!(e.msg, Msg::Tempo(_)));
        events.insert(0, Event { tick: 0, msg: Msg::Tempo(1_000_000) });
    }
    Ok(Smf { ppq, events })
}

/// Read one `MTrk` chunk into `(tick, track, order, message)` tuples. Stops quietly at truncated data.
fn track(b: &[u8], trk: usize, out: &mut Vec<(u64, usize, usize, Msg)>) {
    let byte = |i: &mut usize| { let v = b.get(*i).copied(); *i += 1; v };
    let var = |i: &mut usize| -> Option<usize> {
        let mut v = 0;
        for _ in 0..4 {
            let c = byte(i)?;
            v = v << 7 | (c & 0x7f) as usize;
            if c & 0x80 == 0 { break }
        }
        Some(v)
    };
    let (mut i, mut tick, mut status, mut seq) = (0, 0u64, 0u8, 0);
    loop {
        let Some(dt) = var(&mut i) else { break };
        tick += dt as u64;
        let Some(c) = byte(&mut i) else { break };
        let msg = match c {
            0xFF => {
                let (Some(ty), Some(len)) = (byte(&mut i), var(&mut i)) else { break };
                let Some(data) = b.get(i..i + len) else { break };
                i += len;
                if ty == 0x2F { break }
                match (ty, len) {
                    (0x51, 3) => Some(Msg::Tempo(be(data) as u32)),
                    (0x58, 4..) => Some(Msg::TimeSig(data[0], data[1])),
                    _ => None,
                }
            }
            0xF0 | 0xF7 => {
                let Some(len) = var(&mut i) else { break };
                i += len;
                None
            }
            _ => {
                let first = if c & 0x80 != 0 { status = c; byte(&mut i) } else { Some(c) };
                if status < 0x80 { break }
                if status >= 0xF0 {
                    // System common message inside a track (not valid SMF, but skip it).
                    match status { 0xF2 => { byte(&mut i); } 0xF1 | 0xF3 => {} _ => i -= 1 }
                    status = 0;
                    continue;
                }
                let Some(a) = first else { break };
                let bb = if matches!(status & 0xF0, 0xC0 | 0xD0) { 0 } else { match byte(&mut i) { Some(x) => x & 0x7f, None => break } };
                let (ch, a) = (status & 15, a & 0x7f);
                match status & 0xF0 {
                    0x80 => Some(Msg::Off { ch, key: a }),
                    0x90 if bb == 0 => Some(Msg::Off { ch, key: a }),
                    0x90 => Some(Msg::On { ch, key: a, vel: bb }),
                    0xB0 => Some(Msg::Cc { ch, num: a, val: bb }),
                    0xC0 => Some(Msg::Program { ch, prog: a }),
                    0xE0 => Some(Msg::Bend { ch, val: ((bb as i32) << 7 | a as i32) - 8192 }),
                    _ => None,
                }
            }
        };
        if let Some(m) = msg {
            out.push((tick, trk, seq, m));
            seq += 1;
        }
    }
}

const DRUMS: u8 = 9;

/// Pitch sweeps for the drum kit (`#TABLE` + `np`, 1/64 semitones per frame): kick, toms.
const DRUM_TABLES: [&str; 3] = ["(0,-1536)5 -1536", "(0,-1024)9 -1024", "(0,-768)4 -768"];

/// SiON tone for a General MIDI program (NES-style APU module `%1`).
fn instrument(prog: u8) -> &'static str {
    match prog {
        32..=39 | 72..=79 => "%1@8",                      // bass, pipe: triangle
        0..=15 | 24..=31 | 80..=87 | 104..=111 => "%1@2", // piano, chromatic percussion, guitar, lead, ethnic: 25% pulse
        _ => "%1@4",                                      // organ, strings, brass, reed, pads...: 50% pulse
    }
}

struct Drum { group: usize, instr: &'static str, tone: u8, note: u8, decay: u8, ptab: Option<u8>, gain: f32 }

/// Drum groups 0 (kick), 4 (toms) and 6 (snare body) are tones: sine waves (`%5@0`) swept down in pitch
/// (written that many semitones above where they land), held until the next hit and faded by their envelope's
/// decay. The others are noise (`%2@0` white, `%2@3` high-passed) keyed off at once (`q0`) and faded by their
/// release. Decay/release rate: 28 takes about 0.9 s to fade, every +4 halves it.
fn tonal(group: usize) -> bool { matches!(group, 0 | 4 | 6) }

/// GM drum key → the sounds layered for it. Each group becomes its own track(s).
fn drum(key: u8) -> Vec<Drum> {
    let d = |group, instr, tone, note, decay, ptab, gain| Drum { group, instr, tone, note, decay, ptab, gain };
    let body = |note: u8| d(6, "%5", 0, note + 12, 40, Some(2), 0.6);
    match key {
        35 => vec![d(0, "%5", 0, 31 + 24, 34, Some(0), 1.0)],
        36 => vec![d(0, "%5", 0, 33 + 24, 34, Some(0), 1.0)],
        38 => vec![d(1, "%2", 0, 72, 38, None, 0.9), body(54)],
        40 => vec![d(1, "%2", 0, 76, 39, None, 0.9), body(56)],
        39 => vec![d(1, "%2", 0, 74, 41, None, 0.9)],
        37 => vec![d(1, "%2", 3, 80, 46, None, 1.0)],
        41 => vec![d(4, "%5", 0, 40 + 16, 32, Some(1), 0.9)],
        43 => vec![d(4, "%5", 0, 43 + 16, 32, Some(1), 0.9)],
        45 => vec![d(4, "%5", 0, 45 + 16, 32, Some(1), 0.9)],
        47 => vec![d(4, "%5", 0, 48 + 16, 33, Some(1), 0.9)],
        48 => vec![d(4, "%5", 0, 50 + 16, 33, Some(1), 0.9)],
        50 => vec![d(4, "%5", 0, 53 + 16, 33, Some(1), 0.9)],
        42 => vec![d(2, "%2", 3, 92, 44, None, 0.8)],
        44 => vec![d(2, "%2", 3, 90, 46, None, 0.8)],
        46 => vec![d(2, "%2", 3, 92, 34, None, 0.7)],
        49 | 57 => vec![d(3, "%2", 3, 86, 27, None, 0.5)],
        52 => vec![d(3, "%2", 3, 82, 29, None, 0.5)],
        55 => vec![d(3, "%2", 3, 90, 31, None, 0.5)],
        51 | 59 => vec![d(3, "%2", 3, 96, 32, None, 0.5)],
        53 => vec![d(3, "%2", 3, 100, 30, None, 0.5)],
        54 => vec![d(5, "%2", 3, 98, 40, None, 0.7)],
        56 => vec![d(5, "%1", 4, 80, 40, None, 0.4)],
        _ => vec![d(5, "%2", 3, 88, 40, None, 0.6)],
    }
}

struct Raw { ch: u8, key: u8, start: u64, end: u64, amp: f32, prog: u8, pan: u8 }

/// The performance: channels split by voice later; channel `c` is score channel `16c` and drum group `g`
/// is `16 * 9 + g`, so the tracks come out in channel order.
pub fn to_score(smf: &Smf) -> Score {
    struct Ch { prog: u8, vol: f32, expr: f32, pan: u8, pedal: bool, range: f32, rpn: (u8, u8), on: Vec<Option<(u64, f32, u8, u8)>>, held: Vec<bool> }
    let mut chs: Vec<Ch> = (0..16).map(|_| Ch {
        prog: 0, vol: 100.0 / 127.0, expr: 1.0, pan: 64, pedal: false, range: 2.0, rpn: (127, 127), on: vec![None; 128], held: vec![false; 128],
    }).collect();
    let (mut raw, mut bends, mut tempos, mut bars) = (vec![], vec![vec![]; 16], vec![], vec![]);
    fn close(c: &mut Ch, ch: u8, key: u8, end: u64, raw: &mut Vec<Raw>) {
        if let Some((start, amp, prog, pan)) = c.on[key as usize].take() { raw.push(Raw { ch, key, start, end, amp, prog, pan }) }
        c.held[key as usize] = false;
    }
    for e in &smf.events {
        let t = e.tick;
        match e.msg {
            Msg::On { ch, key, vel } => {
                let c = &mut chs[ch as usize];
                close(c, ch, key, t, &mut raw);
                c.on[key as usize] = Some((t, vel as f32 / 127.0 * c.vol * c.expr, c.prog, c.pan));
            }
            Msg::Off { ch, key } => {
                let c = &mut chs[ch as usize];
                if c.pedal && c.on[key as usize].is_some() { c.held[key as usize] = true } else { close(c, ch, key, t, &mut raw) }
            }
            Msg::Cc { ch, num, val } => {
                let c = &mut chs[ch as usize];
                match num {
                    7 => c.vol = val as f32 / 127.0,
                    11 => c.expr = val as f32 / 127.0,
                    10 => c.pan = val,
                    64 => {
                        c.pedal = val >= 64;
                        if !c.pedal { for k in 0..128 { if c.held[k] { close(c, ch, k as u8, t, &mut raw) } } }
                    }
                    101 => c.rpn.0 = val,
                    100 => c.rpn.1 = val,
                    6 if c.rpn == (0, 0) => c.range = val as f32 + c.range.fract(),
                    38 if c.rpn == (0, 0) => c.range = c.range.trunc() + val as f32 / 100.0,
                    121 => {
                        (c.expr, c.pedal) = (1.0, false);
                        for k in 0..128 { if c.held[k] { close(c, ch, k as u8, t, &mut raw) } }
                        bends[ch as usize].push((t, 0));
                    }
                    120 | 123 => for k in 0..128 { close(c, ch, k as u8, t, &mut raw) },
                    _ => {}
                }
            }
            Msg::Program { ch, prog } => chs[ch as usize].prog = prog,
            Msg::Bend { ch, val } => {
                let r = chs[ch as usize].range;
                bends[ch as usize].push((t, (val as f32 / 8192.0 * r * 64.0).round() as i32));
            }
            Msg::Tempo(us) => tempos.push((t, 6e7 / us.max(1) as f64)),
            Msg::TimeSig(n, d) => {
                // (A change takes effect at the next bar line, where a well-formed file puts it.)
                let len = (smf.ppq as u64 * 4 * n.max(1) as u64) >> d.min(6);
                if bars.last().is_none_or(|b: &(u64, u64)| b.1 != len) {
                    if bars.last().is_some_and(|b: &(u64, u64)| b.0 == t) { bars.pop(); }
                    bars.push((t, len));
                }
            }
        }
    }
    let last = smf.events.last().map_or(0, |e| e.tick);
    for (ch, c) in chs.iter_mut().enumerate() { for k in 0..128 { close(c, ch as u8, k as u8, last, &mut raw) } }
    raw.retain(|n| n.end > n.start || n.ch == DRUMS);
    raw.sort_by_key(|n| (n.ch, n.start, std::cmp::Reverse(n.key)));

    let (mut notes, mut tables) = (vec![], [false; 3]);
    for n in &raw {
        let pan = n.pan as i32 - 64;
        if n.ch == DRUMS {
            for d in drum(n.key) {
                if let Some(t) = d.ptab { tables[t as usize] = true }
                let mut note = Note::new(16 * DRUMS as u32 + d.group as u32, n.start, n.start + 1, d.note, n.amp * d.gain, d.instr.into());
                note.env = if tonal(d.group) { format!("@{},63,{},0,{},15", d.tone, d.decay, d.decay) } else { format!("@{}s{}", d.tone, d.decay) };
                (note.pan, note.ptab, note.drum) = (pan, d.ptab, Some(!tonal(d.group)));
                notes.push(note);
            }
            continue;
        }
        let b = &bends[n.ch as usize];
        let i = b.partition_point(|x| x.0 <= n.start);
        let mut note = Note::new(16 * n.ch as u32, n.start, n.end, n.key, n.amp, instrument(n.prog).into());
        note.pan = pan;
        note.k0 = if i > 0 { b[i - 1].1 } else { 0 };
        note.bends = b[i..].iter().take_while(|x| x.0 < n.end).copied().collect();
        notes.push(note);
    }
    let header = DRUM_TABLES.iter().enumerate().filter(|(i, _)| tables[*i]).map(|(i, t)| format!("#TABLE{i}{{{t}}}")).collect();
    let mix = raw.iter().map(|n| (n.start, n.end, n.amp)).collect();
    let mut names = vec![String::new(); 16 * 16];
    for c in 0..16 { names[16 * c] = format!("channel {}", c + 1) }
    for (g, n) in ["kick", "snare", "hi-hat", "cymbals", "toms", "percussion", "snare body"].iter().enumerate() {
        names[16 * DRUMS as usize + g] = format!("drums: {n}");
    }
    let mut sc = Score { ppq: smf.ppq, tempos, header, notes, mix, end: 0, lp: None, fine: false, db: false, kmilli: false, names, ..Score::default() };
    sc.set_bars(&bars);
    sc
}
