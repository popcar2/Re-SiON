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
    while let Some(dt) = var(&mut i) {
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

/// Pitch drops for the drum kit (`#TABLE` + `np`, 1/64 semitones per 1/60 s frame): kick and toms fall 4 or 8
/// semitones within a frame or two, as GM's do; the snare body sweeps further.
const DRUM_TABLES: [&str; 5] = ["0 -512", "0 -128 -256", "0 -256", "0 -256 -512", "(0,-768)4 -768"];

/// The waveform for a General MIDI program.
fn wave(prog: u8) -> u8 {
    match prog {
        32..=39 | 72..=79 => 8,                      // bass, pipe: triangle
        0..=15 | 24..=31 | 80..=87 | 104..=111 => 2, // piano, chromatic percussion, guitar, lead, ethnic: 25% pulse
        _ => 4,                                      // organ, strings, brass, reed, pads...: 50% pulse
    }
}

/// How loud each GM program comes out against the drum kit (dB), for notes as loud as a GM synthesizer plays
/// them: measured by playing every program through a GM sound font and through its SiON tone, and comparing
/// loudness as heard (K-weighted, ITU-R BS.1770), not raw level: the kit's sines and noise and GM's samples
/// spread their energy differently, and raw level understates how loud bright sounds (distorted guitars) are. SiON's pulse waves
/// come out about 13 dB louder next to the drums than GM instruments do, its triangle about 7; and GM instruments
/// differ among themselves (electric basses are loud, pads and effects quiet).
const PROG_DB: [i8; 128] = [
    -14, -13, -12, -12, -11, -12, -14, -14, -10, -12,  -8, -11, -11, -18,  -8, -11,
    -13, -15, -22, -13, -13, -13, -13, -13, -12, -12,  -9, -14, -12, -11, -12,  -9,
     -6,   3,   4,  -6, -10,  -7,  -7,  -8, -11, -11, -11, -12, -14, -13, -11, -16,
    -14, -18, -13, -13, -15, -11, -10, -15, -15, -15, -15, -15, -13, -18, -17, -11,
    -12, -15, -13, -14, -12, -12, -12, -11,  -8,  -8,  -5,  -6,  -8,  -9,  -4,  -3,
    -13,  -5, -12, -16, -13, -13, -13, -12, -11, -17, -15, -12, -15, -18, -16, -15,
    -15, -15, -11, -15, -13, -19, -18, -14,  -8, -10, -14, -10,  -9,  -9,  -8,  -9,
    -12, -14, -10, -19,  -9, -13,  -8, -20, -22, -24, -24, -14, -13, -24, -21, -14,
];

fn prog_gain(prog: u8) -> f32 { 10f32.powf(PROG_DB[prog as usize & 127] as f32 / 20.0) }

/// SiON tone for a General MIDI program (NES-style APU module `%1`): the waveform, and an envelope shaped like
/// the GM instrument's (`@wave,ar,dr,sr,rr,sl`): attack, how fast a held note fades, and its release. SiON's
/// default release takes half a second, which smears short notes (staccato guitar, bass) into a drone; GM
/// instruments stop within a few tens of milliseconds of their note-off, except pianos, mallets, ensembles and pads.
/// Rates: release 28 fades out in about 0.3 s, 32 in 0.12 s, 36 in 30 ms; sustain rate 12 loses 3 dB per second,
/// 16 about 7, 20 about 14; attack 24 takes 0.16 s, 32 40 ms (each +4 halves the time).
fn instrument(prog: u8) -> (&'static str, String) {
    let wave = wave(prog);
    let (ar, sr, rr) = match prog {
        0..=7 => (63, 15, 29),     // piano: decays while held, rings a little after
        8..=15 => (63, 19, 28),    // chromatic percussion (mallets, bells)
        16..=23 => (63, 0, 36),    // organ
        29 | 30 => (35, 12, 38),   // overdriven, distortion guitar (fading in over 25 ms, as GM's do)
        24..=31 => (63, 16, 36),   // guitar
        32..=39 => (63, 17, 38),   // bass
        45 => (63, 21, 36),        // pizzicato strings
        46 | 47 => (63, 17, 30),   // harp, timpani
        40..=44 => (30, 0, 32),    // strings
        48..=55 => (26, 0, 29),    // ensemble, choir
        56..=63 => (32, 0, 36),    // brass
        64..=79 => (30, 0, 36),    // reed, pipe
        80..=87 => (63, 0, 38),    // lead
        88..=95 => (24, 0, 27),    // pad
        96..=103 => (30, 0, 28),   // synth effects
        104..=111 => (63, 16, 34), // ethnic (plucked)
        112..=119 => (63, 22, 32), // percussive
        _ => (63, 0, 32),          // sound effects
    };
    ("%1", format!("@{wave},{ar},0,{sr},{rr},0"))
}

/// One sound of a drum hit. `env`: for tones, decay rate, the level it decays to (`sl`, 3 dB steps) and the
/// rate after that; for noise, the release rate (the others unused).
struct Drum { group: usize, instr: &'static str, tone: u8, note: u8, env: [u8; 3], ptab: Option<u8>, gain: f32 }

/// Drum groups 0 (kick), 4 (toms) and 6 (snare body) are tones: sine waves (`%5@0`) dropping in pitch
/// (written that many semitones above where they land), held until the next hit and shaped by their envelope:
/// a slow decay to just below full (`sl` 1), then a fast one, so a kick holds its body for 50 ms and is gone by
/// 110 ms, as GM's is. Fitted against a GM sound font, hit by hit (level over time and spectrum). The others
/// are noise (`%2@0` white, `%2@3` high-passed) keyed off at once (`q0`) and faded by their release.
/// Decay/release rate: 28 takes about 0.9 s to fade, every +4 halves it.
fn tonal(group: usize) -> bool { matches!(group, 0 | 4 | 6) }

/// GM drum key → the sounds layered for it. Each group becomes its own track(s).
fn drum(key: u8) -> Vec<Drum> {
    let d = |group, instr, tone, note, decay, ptab, gain| Drum { group, instr, tone, note, env: [decay, 0, 0], ptab, gain };
    let t = |group, note, ptab, dr, sl, sr, gain| Drum { group, instr: "%5", tone: 0, note, env: [dr, sl, sr], ptab: Some(ptab), gain };
    let body = |note: u8| t(6, note + 12, 4, 40, 15, 40, 0.61);
    match key {
        // (With the beater's click: a few milliseconds of noise.)
        35 => vec![t(0, 31 + 8, 0, 30, 1, 48, 0.8), d(7, "%2", 0, 84, 52, None, 0.5)],
        36 => vec![t(0, 36 + 8, 0, 26, 1, 42, 0.73), d(7, "%2", 0, 84, 52, None, 0.46)],
        38 => vec![d(1, "%2", 0, 72, 38, None, 0.85), body(54)],
        40 => vec![d(1, "%2", 0, 76, 39, None, 0.95), body(56)],
        39 => vec![d(1, "%2", 0, 74, 41, None, 0.9)],
        37 => vec![d(1, "%2", 3, 80, 46, None, 0.73)],
        41 => vec![t(4, 40 + 4, 1, 26, 1, 24, 0.33)],
        43 => vec![t(4, 44 + 4, 2, 26, 1, 24, 0.39)],
        45 => vec![t(4, 46 + 8, 3, 30, 1, 24, 0.45)],
        47 => vec![t(4, 50 + 8, 0, 26, 2, 28, 0.36)],
        48 => vec![t(4, 52 + 4, 1, 30, 1, 28, 0.45)],
        50 => vec![t(4, 56 + 8, 0, 30, 1, 28, 0.39)],
        42 => vec![d(2, "%2", 3, 92, 44, None, 0.72)],
        44 => vec![d(2, "%2", 3, 90, 46, None, 0.71)],
        46 => vec![d(2, "%2", 3, 92, 34, None, 0.87)],
        49 | 57 => vec![d(3, "%2", 3, 86, 27, None, 0.68)],
        52 => vec![d(3, "%2", 3, 82, 29, None, 0.52)],
        55 => vec![d(3, "%2", 3, 90, 31, None, 0.74)],
        51 | 59 => vec![d(3, "%2", 3, 96, 32, None, 0.38)],
        53 => vec![d(3, "%2", 3, 100, 30, None, 0.43)],
        54 => vec![d(5, "%2", 3, 98, 40, None, 0.8)],
        56 => vec![d(5, "%1", 4, 80, 40, None, 0.61)],
        _ => vec![d(5, "%2", 3, 88, 40, None, 0.6)],
    }
}

/// GM loudness curves: velocity, channel volume (CC7) and expression (CC11) each scale the amplitude by their
/// square (40·log10(x/127) dB), as GM synthesizers do; SiON's `v` is linear in amplitude.
fn gm(x: u8) -> f32 { gm_f(x as f32 / 127.0) }
fn gm_f(x: f32) -> f32 { x * x }

struct Raw { ch: u8, key: u8, start: u64, end: u64, amp: f32, prog: u8, pan: u8 }



/// The performance: channels split by voice later; channel `c` is score channels `16c..` (one per program it
/// plays) and drum group `g` is `16 * 9 + g`, so the tracks come out in channel order.
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
                c.on[key as usize] = Some((t, gm(vel) * gm_f(c.vol) * gm_f(c.expr), c.prog, c.pan));
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

    // Each program a channel plays is a score channel of its own (`16c + i`, by first use), with its own gain.
    let mut progs: Vec<Vec<u8>> = vec![vec![]; 16];
    for n in &raw { if n.ch != DRUMS && !progs[n.ch as usize].contains(&n.prog) { progs[n.ch as usize].push(n.prog) } }
    let slot = |ch: u8, prog: u8| progs[ch as usize].iter().position(|&p| p == prog).unwrap_or(0).min(15) as u32;
    let (mut notes, mut tables) = (vec![], [false; 5]);
    for n in &raw {
        let pan = n.pan as i32 - 64;
        if n.ch == DRUMS {
            for d in drum(n.key) {
                if let Some(t) = d.ptab { tables[t as usize] = true }
                let mut note = Note::new(16 * DRUMS as u32 + d.group as u32, n.start, n.start + 1, d.note, n.amp * d.gain, d.instr.into());
                let [dr, sl, sr] = d.env;
                note.env = if tonal(d.group) { format!("@{},63,{dr},{sr},{sr},{sl}", d.tone) } else { format!("@{}s{dr}", d.tone) };
                (note.pan, note.ptab, note.drum) = (pan, d.ptab, Some(!tonal(d.group)));
                notes.push(note);
            }
            continue;
        }
        let b = &bends[n.ch as usize];
        let i = b.partition_point(|x| x.0 <= n.start);
        let (instr, env) = instrument(n.prog);
        let mut note = Note::new(16 * n.ch as u32 + slot(n.ch, n.prog), n.start, n.end, n.key, n.amp, instr.into());
        (note.pan, note.env) = (pan, env);
        note.k0 = if i > 0 { b[i - 1].1 } else { 0 };
        note.bends = b[i..].iter().take_while(|x| x.0 < n.end).copied().collect();
        notes.push(note);
    }
    let header = DRUM_TABLES.iter().enumerate().filter(|(i, _)| tables[*i]).map(|(i, t)| format!("#TABLE{i}{{{t}}}")).collect();
    let mix = raw.iter().map(|n| (n.start, n.end, n.amp * if n.ch == DRUMS { 1.0 } else { prog_gain(n.prog) })).collect();
    let (mut names, mut gains) = (vec![String::new(); 16 * 16], vec![1.0; 16 * 16]);
    for (c, ps) in progs.iter().enumerate() {
        names[16 * c] = format!("channel {}", c + 1);
        for (i, &p) in ps.iter().enumerate().take(16) { (names[16 * c + i], gains[16 * c + i]) = (format!("channel {}", c + 1), prog_gain(p)) }
    }
    for (g, n) in ["kick", "snare", "hi-hat", "cymbals", "toms", "percussion", "snare body", "kick click"].iter().enumerate() {
        names[16 * DRUMS as usize + g] = format!("drums: {n}");
    }
    let mut sc = Score { ppq: smf.ppq, tempos, header, notes, mix, end: 0, lp: None, fine: false, db: false, kmilli: false, names, gains, ..Score::default() };
    sc.set_bars(&bars);
    sc
}
