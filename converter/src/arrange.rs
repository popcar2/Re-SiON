//! A `Score` → monophonic SiON tracks: tick scaling, tempo, volume, voice allocation; then each track as a
//! token stream.

use crate::mml::{Lengths, Tok};
use crate::score::Score;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Snap note times to this note division (e.g. 64 = 1/64 notes). `None` keeps timing exact.
    pub grid: Option<u32>,
    /// Volume scale; `None` picks one that keeps chords from clipping.
    pub gain: Option<f32>,
    /// Tracker modules: approximate each sample with a wavetable or noise instead of embedding it (`#PCMB`).
    pub wavetables: bool,
    /// VGM: capture samples exactly as the chips output them, frame by frame, instead of storing each once at its
    /// own rate (much larger).
    pub exact_pcm: bool,
    /// NSF / NSFE: which song of the file to convert (from 1). `None`: the one the file starts with.
    pub track: Option<usize>,
}

pub struct Arranged {
    /// System commands (without the trailing `;`), e.g. `#TABLE0{...}`.
    pub header: Vec<String>,
    pub tracks: Vec<Track>,
    pub notes: usize,
}

impl Arranged {
    /// The master volume (`x`) every track starts with.
    pub fn master(&self) -> i32 { self.tracks.first().and_then(|t| t.x).unwrap_or(128) }
    pub fn set_master(&mut self, x: i32) { for t in &mut self.tracks { t.x = (x < 128).then_some(x.clamp(1, 128)) } }
}

pub struct Track {
    notes: Vec<Note>,
    /// Drum track: every hit lasts until the next one (see `score::Note::drum`).
    drum: Option<bool>,
    /// Master volume (`x`), when below full.
    x: Option<i32>,
    /// Tempo commands carried by this track: (tick, `t...`).
    tempo: Vec<(u64, String)>,
    /// Rest until this tick after the last note (the song's end).
    end: u64,
    /// Loop point (`$`): with one, every track plays to the same `end`.
    lp: Option<u64>,
    /// Loudest `v` (16, or 256 with `#VMODE{%v0,0}`).
    vmax: u16,
    /// Detunes are in thousandths of `k` (written with a fraction).
    kmilli: bool,
}

struct Note {
    start: u64,
    /// Key-off tick.
    end: u64,
    key: u8,
    v: u16,
    instr: String,
    /// Tone and envelope set after the module (`@tone,ar,dr,sr,rr,sl`), empty when `instr` says it all.
    env: String,
    pan: i32,
    /// Pitch envelope table (`np`).
    ptab: Option<u8>,
    /// Detune (`k`, 1/64 semitone) at the start, then changes while the note sounds.
    k0: i32,
    bends: Vec<(u64, i32)>,
    /// Volume (`v`) and pan (`@p`) changes while the note sounds.
    vols: Vec<(u64, u16)>,
    pans: Vec<(u64, i32)>,
    /// LFO depths (`mp`, `ma`) at the start and changes while the note sounds.
    lfo0: (i32, i32),
    lfos: Vec<(u64, (i32, i32))>,
    /// Waveform changes (the module's `@n`, as the whole `instr`) while the note sounds.
    waves: Vec<(u64, String)>,
    /// Key-off pitch table (`_np`), and the tick its release is silenced (`v0`).
    rtab: Option<usize>,
    cut: Option<u64>,
    /// PCM start frame (`@ph`, to 1/1000 frame).
    ofs: f64,
    /// PCM frames to play from there (0: all).
    plen: f64,
}

fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } }

pub fn build(sc: &Score, opt: &Options) -> Arranged {
    // Ticks: SiON counts 480 per quarter note. Keep timing exact: rescale when the source resolution allows,
    // otherwise count in source ticks and scale the tempo instead.
    let ppq = sc.ppq.max(1) as u64;
    let mut g = 0;
    for n in &sc.notes {
        g = gcd(gcd(g, n.start), n.end);
        for &(t, _) in &n.bends { g = gcd(g, t) }
        for &(t, _) in &n.vols { g = gcd(g, t) }
    }
    for &(s, e, _) in &sc.mix { g = gcd(gcd(g, s), e) }
    for &(t, _) in &sc.tempos { g = gcd(g, t) }
    if g == 0 { g = ppq }
    let max_bpm = sc.tempos.iter().map(|t| t.1).fold(120.0, f64::max);
    let min_bpm = sc.tempos.iter().map(|t| t.1).fold(120.0, f64::min);
    let g480 = gcd(ppq, 480);
    let odd = (ppq as f64 / g as f64) / 480.0;
    let (scale, grid, ts): ((u64, u64), u64, f64) = match opt.grid {
        Some(n) => ((480, ppq), (1920 / n.clamp(1, 1920)) as u64, 1.0),
        None if g % (ppq / g480) == 0 => ((480 / g480, ppq / g480), 1, 1.0),
        None if max_bpm * odd <= 4000.0 && min_bpm * odd >= 1.0 => ((1, g), 1, odd),
        None => ((480, ppq), 1, 1.0),
    };
    let map = |t: u64| -> u64 {
        let (a, b) = scale;
        if grid == 1 && (t * a) % b == 0 { t * a / b } else { ((t * a) as f64 / b as f64 / grid as f64).round() as u64 * grid }
    };
    let span = |s: u64, e: u64| { let (s, e) = (map(s), map(e)); (s, if e <= s { s + grid } else { e }) };

    // Volume: relative to the loudest moment, in 16 steps (`v1`..`v16`); a master volume (`x`, and the drum
    // envelope peak) keeps the busiest chord from clipping.
    let mut mix: Vec<(u64, u64, f32)> = sc.mix.iter().map(|&(s, e, a)| { let (s, e) = span(s, e); (s, e, a) }).collect();
    if mix.is_empty() { mix = sc.notes.iter().map(|n| { let (s, e) = span(n.start, n.end); (s, e, n.amp) }).collect() }
    let mut amax = mix.iter().map(|m| m.2).fold(0.0, f32::max).max(1e-6);
    if sc.mix.is_empty() { amax = sc.notes.iter().flat_map(|n| n.vols.iter().map(|v| v.1)).fold(amax, f32::max) }
    let mut edges: Vec<(u64, bool, f32)> = mix.iter().flat_map(|&(s, e, a)| [(s, true, a), (e, false, a)]).collect();
    edges.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let (mut sum, mut smax) = (0f32, 1e-6f32);
    for (_, on, a) in edges { sum += if on { a } else { -a }; smax = smax.max(sum) }
    let master = opt.gain.map_or((5.0 * amax / smax).min(1.0), |g| (g * amax).min(1.0));
    let x = ((128.0 * master).round() as i32).clamp(1, 128);
    let vmax: u16 = if sc.db { 128 } else if sc.fine { 256 } else { 16 };
    let vol = |amp: f32, min: i32| {
        let v = if !sc.db { vmax as f32 * amp / amax } else if amp <= 0.0 { 0.0 } else { 128.0 + 20.0 * (amp / amax).log10() / 0.75 };
        (v.round() as i32).clamp(min, vmax as i32) as u16
    };

    // Release pitch sweeps: one `#TABLE` each (in the numbers the score's own leave free), shared by equal sweeps.
    let mut header_tabs: Vec<String> = vec![];
    let taken: Vec<usize> = sc.header.iter().filter_map(|h| h.strip_prefix("#TABLE")?.split('{').next()?.parse().ok()).collect();
    let mut free = (0..255).filter(|i| !taken.contains(i));
    let mut tab_ids: std::collections::HashMap<&[i32], Option<usize>> = Default::default();

    // Split each channel into monophonic voices (one track each).
    let step = if sc.kmilli { 1 } else { (480.0 * ts / 32.0).max(1.0) as u64 };
    let mut order: Vec<usize> = (0..sc.notes.len()).collect();
    order.sort_by_key(|&i| (sc.notes[i].ch, sc.notes[i].start));
    let mut tracks = vec![];
    let mut i = 0;
    while i < order.len() {
        let ch = sc.notes[order[i]].ch;
        let mut voices: Vec<(u64, Vec<Note>, Option<bool>)> = vec![];
        while i < order.len() && sc.notes[order[i]].ch == ch {
            let n = &sc.notes[order[i]];
            i += 1;
            let (start, end) = span(n.start, n.end);
            let (k0, bends) = thin(n.k0, n.bends.iter().map(|&(t, k)| (map(t), k)), start, end, step);
            // (PCM notes may start silent: the sample runs from its key-on.)
            let vmin = if sc.fine && !sc.db { 0 } else { 1 };
            let (mut v, mut vols) = (vol(n.amp, vmin), vec![]);
            for &(t, a) in &n.vols {
                let (t, nv) = (map(t), vol(a, 0));
                if t <= start { v = nv.max(vmin as u16); continue }
                if t >= end { break }
                let last = vols.last().map_or(v, |x: &(u64, u16)| x.1);
                if nv == last { continue }
                if vols.last().is_some_and(|x| x.0 == t) { vols.pop(); }
                vols.push((t, nv));
            }
            let mut pans: Vec<(u64, i32)> = vec![];
            for &(t, p) in &n.pans {
                let t = map(t);
                if t <= start || t >= end { continue }
                if pans.last().is_some_and(|x| x.0 == t) { pans.pop(); }
                pans.push((t, p));
            }
            let mut lfos: Vec<(u64, (i32, i32))> = vec![];
            let mut lfo0 = n.lfo0;
            for &(t, l) in &n.lfos {
                let t = map(t);
                if t <= start { lfo0 = l; continue }
                if t >= end { break }
                if lfos.last().is_some_and(|x| x.0 == t) { lfos.pop(); }
                if lfos.last().map_or(lfo0, |x| x.1) != l { lfos.push((t, l)) }
            }
            let mut waves: Vec<(u64, String)> = vec![];
            let mut instr = n.instr.clone();
            for (t, w) in &n.waves {
                let t = map(*t);
                if t <= start { instr = w.clone(); continue }
                if t >= end { break }
                if waves.last().is_some_and(|x| x.0 == t) { waves.pop(); }
                if waves.last().map_or(&instr, |x| &x.1) != w { waves.push((t, w.clone())) }
            }
            // (SiON has 255 tables; sweeps past that are left out.)
            let rtab = (!n.rel.is_empty()).then(|| {
                *tab_ids.entry(&n.rel[..]).or_insert_with(|| {
                    let i = free.next()?;
                    header_tabs.push(format!("#TABLE{i}{{{}}}", n.rel.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(",")));
                    Some(i)
                })
            }).flatten();
            let cut = n.cut.map(map).filter(|&c| c >= end);
            let note = Note { start, end, key: n.key, v, instr, env: n.env.clone(), pan: n.pan, ptab: n.ptab, k0, bends, vols, pans, lfo0, lfos, waves, rtab, cut, ofs: n.ofs, plen: n.plen };
            // Drum hits ring until the next hit, so a voice is free as soon as its last hit started.
            let slot = match n.drum { Some(_) => voices.iter_mut().find(|v| v.0 < start), None => voices.iter_mut().find(|v| v.0 <= start) };
            let busy = if n.drum.is_some() { start } else { end };
            match slot { Some(v) => { v.0 = busy; v.1.push(note) } None => voices.push((busy, vec![note], n.drum)) }
        }
        tracks.extend(voices.into_iter().map(|(_, notes, drum)| Track { notes, drum, x: None, tempo: vec![], end: 0, lp: None, vmax, kmilli: sc.kmilli }));
    }

    // Tempo: one `t` command per change, carried by whichever track has a convenient spot for it.
    let song_end = tracks.iter().filter_map(|t| t.notes.last()).map(|n| n.end).max().unwrap_or(0);
    let mut tm: Vec<(u64, f64)> = vec![];
    if sc.tempos.first().is_none_or(|t| t.0 > 0) { tm.push((0, 120.0 * ts)) }
    for &(t, bpm) in &sc.tempos {
        let (t, bpm) = (map(t), bpm * ts);
        if tm.last().is_some_and(|l| l.0 == t) { tm.pop(); }
        if tm.last().is_none_or(|l| l.1 != bpm) && t < song_end { tm.push((t, bpm)) }
    }
    let unit = [1.0, 10.0, 100.0, 1000.0].into_iter()
        .find(|u| tm.iter().all(|&(_, b)| ((b * u).round() / u - b).abs() / b < 1e-5)).unwrap_or(1000.0);
    let mut header = vec![];
    if unit > 1.0 { header.push(format!("#TMODE{{unit={unit}}}")) }
    if sc.db { header.push("#VMODE{%v1,1}".into()) } else if sc.fine { header.push("#VMODE{%v0,0}".into()) }
    header.extend(sc.header.iter().cloned());
    header.extend(header_tabs);
    if x < 128 { for t in &mut tracks { t.x = Some(x) } }
    for (t, bpm) in tm {
        let val = (bpm * unit).round() as i64;
        if t == 0 && val as f64 == 120.0 * unit { continue }
        let spot = |tr: &Track| {
            let i = tr.notes.partition_point(|n| n.start <= t);
            match i.checked_sub(1).map(|j| &tr.notes[j]) {
                None => 1,
                Some(n) if n.start == t || n.end == t => 0,
                Some(n) if t < n.end => 3,
                Some(_) if i == tr.notes.len() => 2,
                Some(_) => 1,
            }
        };
        if let Some(tr) = tracks.iter_mut().min_by_key(|tr| spot(tr)) { tr.tempo.push((t, format!("t{val}"))) }
    }
    // Keep trailing silence: the first non-drum track rests until the song's end.
    let end = map(sc.end);
    if end > song_end { if let Some(t) = tracks.iter_mut().find(|t| t.drum.is_none()) { t.end = end } }
    // A looping song: every track plays to the end, then goes back to the loop point together.
    let end = end.max(song_end);
    if let Some(lp) = sc.lp.map(map).filter(|&l| l < end) { for t in &mut tracks { (t.end, t.lp) = (end, Some(lp)) } }
    let notes = tracks.iter().map(|t| t.notes.len()).sum();
    Arranged { header, tracks, notes }
}

/// Detune at the note's start, then thinned changes (at least `step` ticks apart) while it sounds.
fn thin(k0: i32, b: impl Iterator<Item = (u64, i32)>, s: u64, e: u64, step: u64) -> (i32, Vec<(u64, i32)>) {
    let mut b = b.peekable();
    let mut k0 = k0;
    while let Some((_, k)) = b.next_if(|x| x.0 <= s) { k0 = k }
    let (mut last, mut next, mut cur, mut cur_t, mut out) = (k0, s + step, k0, s, vec![]);
    for (t, k) in b {
        if t >= e { break }
        (cur, cur_t) = (k, t);
        if t >= next && k != last { out.push((t, k)); (last, next) = (k, t + step) }
    }
    let t = next.max(cur_t);
    if cur != last && t < e { out.push((t, cur)) }
    (k0, out)
}

/// The track as tokens. With `gate` the key-off times are encoded through `q` where shorter
/// (choosing `q` states by dynamic programming); otherwise every gap is an explicit rest under `q8`.
pub fn tokens(tr: &Track, gate: bool, lens: &Lengths) -> Vec<Tok> {
    let mut out = vec![];
    match tr.drum { Some(true) => out.push(Tok::Cmd("q0".into())), Some(false) => out.push(Tok::Cmd("q8".into())), None if !gate => out.push(Tok::Cmd("q8".into())), None => {} }
    if let Some(x) = tr.x { out.push(Tok::Cmd(format!("x{x}"))) }
    let (mut t, mut tempo) = (0, tr.tempo.iter().peekable());
    let (mut instr, mut v, mut pan, mut ptab, mut env, mut k, mut ph) = ("", tr.vmax, 0, None, "", 0, (0.0, 0.0));
    let (mut lfo, mut rtab, mut cut): ((i32, i32), Option<usize>, Option<u64>) = ((0, 0), None, None);
    let lfo_cmds = |out: &mut Vec<Tok>, cur: &mut (i32, i32), to: (i32, i32)| {
        if to.0 != cur.0 { out.push(Tok::Cmd(format!("mp{}", to.0))) }
        if to.1 != cur.1 { out.push(Tok::Cmd(format!("ma{}", to.1))) }
        *cur = to;
    };
    let kcmd = |k: i32| if tr.kmilli { format!("k{}", k as f64 / 1000.0) } else { format!("k{k}") };
    let mut gates = vec![]; // (index of the note's first piece, index of its last piece)
    let rest_to = |out: &mut Vec<Tok>, t: &mut u64, until: u64, tempo: &mut std::iter::Peekable<std::slice::Iter<(u64, String)>>| {
        while let Some((tt, s)) = tempo.next_if(|x| x.0 <= until) {
            if *tt > *t { out.push(Tok::Rest((*tt - *t) as u32)); *t = *tt }
            out.push(Tok::Cmd(s.clone()));
        }
        if until > *t { out.push(Tok::Rest((until - *t) as u32)); *t = until }
    };
    // The loop point (`$`), and the channel state there, which the end of the track restores before going back.
    struct Snap<'a> { instr: &'a str, env: &'a str, v: u16, pan: i32, ptab: Option<u8>, k: i32, ph: (f64, f64), lfo: (i32, i32), rtab: Option<usize> }
    let mut mark: Option<Snap> = None;
    macro_rules! loop_point { () => {{
        out.push(Tok::Cmd("$".into()));
        mark = Some(Snap { instr, env, v, pan, ptab, k, ph, lfo, rtab });
    }} }
    // Rest until `until`, putting the loop point on the way.
    macro_rules! advance { ($until:expr) => {{
        let until = $until;
        if let Some(l) = tr.lp.filter(|&l| mark.is_none() && l >= t && l <= until) { rest_to(&mut out, &mut t, l, &mut tempo); loop_point!() }
        rest_to(&mut out, &mut t, until, &mut tempo);
    }} }
    for n in &tr.notes {
        // The previous note's release silenced (on the chip, by its driver) before this note.
        if let Some(c) = cut.take().filter(|&c| c < n.start) { advance!(c); out.push(Tok::Cmd("v0".into())); v = 0 }
        advance!(n.start);
        // A module change (`%`) resets the tone and envelope; so going back to the module's defaults needs one.
        // (Both also reset the LFO: `%` the whole channel, `@lfo` in a tone its depths.)
        if n.instr != instr || (n.env.is_empty() && !env.is_empty()) { out.push(Tok::Cmd(n.instr.clone())); (instr, env, ph, lfo) = (&n.instr, "", (0.0, 0.0), (0, 0)) }
        if n.env != env {
            // (An LFO rate the channel already has needn't be set again: that would only reset its depths.)
            let ((_, ra), (b, rb)) = (split_lfo(env), split_lfo(&n.env));
            if ra != rb { out.push(Tok::Cmd(n.env.clone())); lfo = (0, 0) } else if !b.is_empty() { out.push(Tok::Cmd(b.into())) }
            env = &n.env;
        }
        lfo_cmds(&mut out, &mut lfo, n.lfo0);
        if n.rtab != rtab { out.push(Tok::Cmd(n.rtab.map_or("_np".into(), |i| format!("_np{i}")))); rtab = n.rtab }
        if n.v != v { out.push(Tok::Cmd(format!("v{}", n.v))); v = n.v }
        if n.pan != pan { out.push(Tok::Cmd(format!("@p{}", n.pan))); pan = n.pan }
        if n.ptab != ptab { out.push(Tok::Cmd(n.ptab.map_or("np".into(), |t| format!("np{t}")))); ptab = n.ptab }
        if n.k0 != k { out.push(Tok::Cmd(kcmd(n.k0))); k = n.k0 }
        if (n.ofs, n.plen) != ph {
            out.push(Tok::Cmd(if n.plen > 0.0 { format!("@ph{},{}", n.ofs, n.plen) } else { format!("@ph{}", n.ofs) }));
            ph = (n.ofs, n.plen);
        }
        // Pieces joined by slurs where the pitch bend or volume changes or a tempo change falls mid-note.
        let first = out.len();
        let (mut bend, mut vols, mut pans) = (n.bends.iter().peekable(), n.vols.iter().peekable(), n.pans.iter().peekable());
        let (mut lfos, mut waves) = (n.lfos.iter().peekable(), n.waves.iter().peekable());
        let mut at = n.start;
        // A bend of a semitone or more moves the slur to the nearer note (`c&d`) instead of a large `k`.
        let unit = if tr.kmilli { 64000 } else { 64 };
        let mut key = n.key as i32;
        loop {
            let nb = bend.peek().map(|b| b.0);
            let nv = vols.peek().map(|b| b.0);
            let np = pans.peek().map(|b| b.0);
            let nl = lfos.peek().map(|b| b.0);
            let nw = waves.peek().map(|b| b.0);
            let nt = tempo.peek().map(|x| x.0).filter(|&x| x > n.start && x < n.end);
            // (A note that sounds through the loop point is split there, slurred: `c&$c`.)
            let nlp = tr.lp.filter(|&l| mark.is_none() && l > n.start && l < n.end);
            let Some(split) = [nb, nv, np, nl, nw, nt, nlp].into_iter().flatten().min() else { break };
            if split > at { out.push(Tok::Note { key: key as u8, len: (split - at) as u32, slur: true }); at = split }
            if nlp == Some(split) { loop_point!() }
            while let Some((_, s)) = tempo.next_if(|x| x.0 == split) { out.push(Tok::Cmd(s.clone())) }
            while let Some(&(_, nk)) = bend.next_if(|b| b.0 == split) {
                let total = n.key as i32 * unit + nk;
                if (total - key * unit).abs() >= unit { key = ((total as f64 / unit as f64).round() as i32).clamp(0, 127) }
                let nk = total - key * unit;
                if nk != k { out.push(Tok::Cmd(kcmd(nk))); k = nk }
            }
            while let Some(&(_, nv)) = vols.next_if(|b| b.0 == split) {
                if nv != v { out.push(Tok::Cmd(format!("v{nv}"))); v = nv }
            }
            while let Some(&(_, np)) = pans.next_if(|b| b.0 == split) {
                if np != pan { out.push(Tok::Cmd(format!("@p{np}"))); pan = np }
            }
            while let Some(&(_, nl)) = lfos.next_if(|b| b.0 == split) { lfo_cmds(&mut out, &mut lfo, nl) }
            // (Same module: `@n` changes the waveform and leaves the note, envelope and LFO as they are.)
            while let Some((_, w)) = waves.next_if(|b| b.0 == split) {
                if *w != instr { out.push(Tok::Cmd(format!("@{}", w.split_once('@').unwrap().1))); instr = w }
            }
        }
        out.push(Tok::Note { key: key as u8, len: (n.end - at) as u32, slur: false });
        gates.push((first, out.len() - 1));
        t = n.end;
        cut = n.cut;
    }
    // (After the last note, only within the song.)
    if let Some(c) = cut.filter(|&c| c <= tr.end) { advance!(c); out.push(Tok::Cmd("v0".into())); v = 0 }
    let rest: Vec<u64> = tempo.clone().map(|x| x.0).collect();
    if let Some(&end) = rest.last() { advance!(end) }
    if tr.end > t || tr.lp.is_some() { advance!(tr.end) }
    // Back to the loop point: set the channel as it was there. (Before the first note there is no tone to go
    // back to: the first note sets its own.)
    if let Some(s) = mark.take() {
        if !s.instr.is_empty() {
            if s.instr != instr || (s.env.is_empty() && !env.is_empty()) { out.push(Tok::Cmd(s.instr.into())); (env, ph, lfo) = ("", (0.0, 0.0), (0, 0)) }
            if s.env != env { out.push(Tok::Cmd(s.env.into())); lfo = (0, 0) }
            lfo_cmds(&mut out, &mut lfo, s.lfo);
            if s.ph != ph { out.push(Tok::Cmd(if s.ph.1 > 0.0 { format!("@ph{},{}", s.ph.0, s.ph.1) } else { format!("@ph{}", s.ph.0) })) }
        }
        if s.rtab != rtab { out.push(Tok::Cmd(s.rtab.map_or("_np".into(), |i| format!("_np{i}")))) }
        if s.v != v { out.push(Tok::Cmd(format!("v{}", s.v))) }
        if s.pan != pan { out.push(Tok::Cmd(format!("@p{}", s.pan))) }
        if s.ptab != ptab { out.push(Tok::Cmd(s.ptab.map_or("np".into(), |t| format!("np{t}")))) }
        if s.k != k { out.push(Tok::Cmd(kcmd(s.k))) }
    }
    if tr.drum.is_some() {
        // Each hit is held until the next (its envelope has faded by then), so no rests are needed.
        let mut merged: Vec<Tok> = vec![];
        for tok in out {
            match (merged.last_mut(), tok) {
                (Some(Tok::Note { len, .. }), Tok::Rest(r)) => *len += r,
                (_, tok) => merged.push(tok),
            }
        }
        if let Some(Tok::Note { len, .. }) = merged.last_mut().filter(|_| tr.lp.is_none()) { *len = (*len).max(480) }
        return merged;
    }
    if gate { gate_dp(&mut out, &gates, lens) }
    out
}

/// A tone's envelope commands split before its LFO rate (`@3@lfo9` → `@3`, `@lfo9`).
fn split_lfo(e: &str) -> (&str, &str) { e.find("@lfo").map_or((e, ""), |i| e.split_at(i)) }

/// Choose a `q` (gate = q/8 of the written length) per note so that "note + rest" is written shortest.
/// `q8` and an explicit rest always works; e.g. under `q4` a 1/8 note followed by a 1/8 rest is just `c4`.
fn gate_dp(out: &mut Vec<Tok>, gates: &[(usize, usize)], lens: &Lengths) {
    const INF: u32 = u32::MAX / 4;
    let opts: Vec<[Option<(u32, u32, u32)>; 9]> = gates.iter().map(|&(_, gi)| {
        let Tok::Note { len: g, .. } = out[gi] else { unreachable!() };
        let r = match out.get(gi + 1) { Some(Tok::Rest(r)) => *r, _ => 0 };
        let mut o = [None; 9];
        for (q, slot) in o.iter_mut().enumerate().skip(1) {
            if (8 * g) % q as u32 != 0 { continue }
            let l = 8 * g / q as u32;
            if l > g + r { continue }
            let rr = g + r - l;
            *slot = Some((lens.cost(l, 0) + if rr > 0 { 1 + lens.cost(rr, 0) } else { 0 }, l, rr));
        }
        o
    }).collect();
    let mut cost = [INF; 9];
    cost[6] = 0; // SiON's default is q6
    let mut back = vec![[0u8; 9]; gates.len()];
    for (i, o) in opts.iter().enumerate() {
        let mut nc = [INF; 9];
        for q in 1..9 {
            let Some((c, ..)) = o[q] else { continue };
            for p in 1..9 {
                let x = cost[p] + c + if p == q { 0 } else { 2 };
                if x < nc[q] { nc[q] = x; back[i][q] = p as u8 }
            }
        }
        cost = nc;
    }
    let mut q = (1..9).min_by_key(|&q| cost[q]).unwrap_or(8);
    let mut pick = vec![0; gates.len()];
    for i in (0..gates.len()).rev() { pick[i] = q; q = back[i][q] as usize }
    let mut res = Vec::with_capacity(out.len() + 16);
    let (mut gi, mut prev) = (0, 6);
    let mut skip_rest = false;
    for (i, tok) in out.drain(..).enumerate() {
        if skip_rest { skip_rest = false; if matches!(tok, Tok::Rest(_)) { continue } }
        // After the loop point `q` is whatever the track's end left it at: set it again.
        if matches!(&tok, Tok::Cmd(c) if c == "$") {
            res.push(tok);
            if gi < gates.len() { res.push(Tok::Cmd(format!("q{}", pick[gi]))); prev = pick[gi] }
            continue;
        }
        if gi < gates.len() && i == gates[gi].0 && pick[gi] != prev { res.push(Tok::Cmd(format!("q{}", pick[gi]))); prev = pick[gi] }
        if gi < gates.len() && i == gates[gi].1 {
            let (_, l, rr) = opts[gi][pick[gi]].unwrap();
            let Tok::Note { key, .. } = tok else { unreachable!() };
            res.push(Tok::Note { key, len: l, slur: false });
            if rr > 0 { res.push(Tok::Rest(rr)) }
            skip_rest = true;
            gi += 1;
            continue;
        }
        res.push(tok);
    }
    *out = res;
}
