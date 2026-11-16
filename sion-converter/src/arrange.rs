//! A `Score` → monophonic SiON tracks: tick scaling, tempo, volume, voice allocation; then each track as a
//! token stream.

use std::collections::HashMap;

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
    /// Tracker modules: convert exactly, tick by tick (every volume, pan and pitch change, sample positions), as
    /// minified MML. Otherwise the song is written the way it would be by hand: per row, laid out by its patterns.
    pub exact: bool,
    /// VGM: capture samples exactly as the chips output them, frame by frame, instead of storing each once at its
    /// own rate (much larger).
    pub exact_pcm: bool,
    /// NSF / GBS: keep the console's own frame rate (the NES's 60.1 Hz, the Game Boy's 59.73 Hz) instead of
    /// rounding the frame to a whole number of samples (60 Hz; smaller output, the song 0.16% / 0.45% off speed).
    pub exact_rate: bool,
    /// NSF / NSFE: which song of the file to convert (from 1). `None`: the one the file starts with.
    pub track: Option<usize>,
}

pub struct Arranged {
    /// System commands (without the trailing `;`), e.g. `#TABLE0{...}`.
    pub header: Vec<String>,
    pub tracks: Vec<Track>,
    pub notes: usize,
    /// The score's sections and loop point, in output ticks.
    pub sections: Vec<u64>,
    pub lp: Option<u64>,
    /// The master volume (`x`, before each track's own gain).
    x: i32,
    /// Ticks per envelope-table frame and frames per second (`#FPS`), when every frame starts on a whole sample
    /// and the song keeps one tempo: per-frame volume and detune runs can then be written as `na` / `np` tables
    /// (`Arranged::envelopes`).
    frame: Option<(u64, u32)>,
    /// `#TABLE` numbers nobody else uses.
    free: Vec<usize>,
}

impl Arranged {
    /// The master volume (`x`) of the loudest tracks.
    pub fn master(&self) -> i32 { self.x }
    /// Ticks per envelope frame (see `frame`).
    pub fn frame(&self) -> Option<u64> { self.frame.map(|f| f.0) }
    pub fn set_master(&mut self, x: i32) {
        self.x = x.clamp(1, 128);
        for t in &mut self.tracks { t.set_x(self.x) }
    }
}

impl Track {
    fn set_x(&mut self, x: i32) {
        // (A track playing into a pipe keeps its absolute level: the modulation's depth.)
        if self.piped { return }
        let x = ((x as f32 * self.rel).round() as i32).clamp(1, 128);
        // (In decibels, `#VMODE{%x1}`: rounded down, so the level never comes out louder than measured.)
        let x = if self.xdb { (128.0 + 20.0 * (x as f64 / 128.0).log10() / DB_STEP).floor().max(1.0) as i32 } else { x };
        self.x = (x < 128).then_some(x);
    }

    /// The tick note `i`'s release is silenced at, where `tokens` silences it: before the next note, or after the
    /// last one within the song.
    fn cut(&self, i: usize) -> Option<u64> {
        let c = self.notes[i].cut?;
        self.notes.get(i + 1).map_or(c <= self.end, |n| c < n.start).then_some(c)
    }

    /// The loop point, when note `n` sounds through it.
    fn through(&self, n: &Note) -> Option<u64> { self.lp.filter(|&l| n.start < l && l < n.end) }

    /// The loop point (`$`) falls in `a..=b`.
    fn lp_in(&self, a: u64, b: u64) -> bool { self.lp.is_some_and(|l| a <= l && l <= b) }

    /// Its notes' levels can be in `x` (see `Envelopes`), as volume tables where they fit: levels `x` can hold,
    /// and no release silenced across the loop point (`$`: the `x0` after it would cut whatever releases there on
    /// the way back).
    fn vol_tables(&self) -> bool {
        self.drum.is_none() && !self.piped && self.notes.iter().any(|n| !n.vols.is_empty()) && self.notes.iter().enumerate().all(|(i, n)| {
            n.v <= 128 && n.vols.iter().all(|v| v.1 <= 128) && self.cut(i).is_none_or(|c| !self.lp_in(n.end, c))
        })
    }

    /// Note `n`'s levels fit a volume table stepping every `f` ticks: its changes on that grid from its key-on, and
    /// the loop point too if it sounds through it (keyed on afresh there on the way back, it plays a second table,
    /// from the loop point on).
    fn vol_table(&self, n: &Note, f: u64) -> bool {
        n.vols.iter().all(|v| (v.0 - n.start) % f == 0) && self.through(n).is_none_or(|l| (l - n.start) % f == 0)
    }
}

pub struct Track {
    /// The score channel it plays.
    pub ch: u32,
    notes: Vec<Note>,
    /// Drum track: every hit lasts until the next one (see `score::Note::drum`).
    drum: Option<bool>,
    /// Master volume (`x`), when below full.
    x: Option<i32>,
    /// Its channel's gain (`Score::gains`) relative to the loudest channel's: scales the master volume.
    rel: f32,
    /// It plays into a pipe (`Score::pipes`): full volume, whatever the master's.
    piped: bool,
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
    /// `x` is in decibels, as `v` (`#VMODE{%v1,1 %x1}`), so a volume table can hold the track's `v` steps.
    xdb: bool,
}

/// One step of `v` and `x` in decibels (`%v1`, `%x1`): an eighth of an octave.
const DB_STEP: f64 = 0.75257498916;

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

pub(crate) fn gcd(a: u64, b: u64) -> u64 { if b == 0 { a } else { gcd(b, a % b) } }

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
    // (Each channel's gain goes in its tracks' `x`: the `v`s stay relative to the loudest note, the gains count
    // only toward how loud the busiest moment gets. `Score::mix` has them already.)
    let gain = |ch: u32| sc.gains.get(ch as usize).copied().unwrap_or(1.0).max(0.0);
    let gmax = sc.notes.iter().map(|n| gain(n.ch)).fold(0.0, f32::max);
    let rel = |ch: u32| if gmax > 0.0 { gain(ch) / gmax } else { 1.0 };
    let mut mix: Vec<(u64, u64, f32)> = sc.mix.iter().map(|&(s, e, a)| { let (s, e) = span(s, e); (s, e, a / gmax.max(1e-6)) }).collect();
    // (Channels playing into a pipe aren't heard: left out.)
    let heard = |n: &&crate::score::Note| !sc.pipes.contains(&n.ch);
    if mix.is_empty() { mix = sc.notes.iter().filter(heard).map(|n| { let (s, e) = span(n.start, n.end); (s, e, n.amp * rel(n.ch)) }).collect() }
    let mut amax = sc.notes.iter().filter(heard).map(|n| n.amp).fold(0.0, f32::max).max(1e-6);
    if sc.mix.is_empty() { amax = sc.notes.iter().filter(heard).flat_map(|n| n.vols.iter().map(|v| v.1)).fold(amax, f32::max) }
    let mut edges: Vec<(u64, bool, f32)> = mix.iter().flat_map(|&(s, e, a)| [(s, true, a), (e, false, a)]).collect();
    edges.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let (mut sum, mut smax) = (0f32, 1e-6f32);
    for (_, on, a) in edges { sum += if on { a } else { -a }; smax = smax.max(sum) }
    let master = opt.gain.map_or((5.0 * amax / smax).min(1.0), |g| (g * amax).min(1.0));
    let x = ((128.0 * master).round() as i32).clamp(1, 128);
    let vmax = sc.vmax();
    let vol = |amp: f32, min: i32, piped: bool| {
        let amax = if piped { 1.0 } else { amax };
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
        let piped = sc.pipes.contains(&ch);
        let mut voices: Vec<(u64, Vec<Note>, Option<bool>)> = vec![];
        while i < order.len() && sc.notes[order[i]].ch == ch {
            let n = &sc.notes[order[i]];
            i += 1;
            let (start, end) = span(n.start, n.end);
            let (k0, bends) = thin(n.k0, n.bends.iter().map(|&(t, k)| (map(t), k)), start, end, step);
            // (PCM notes may start silent: the sample runs from its key-on.)
            let vmin = if (sc.fine && !sc.db) || piped { 0 } else { 1 };
            let (mut v, mut vols) = (vol(n.amp, vmin, piped), vec![]);
            for &(t, a) in &n.vols {
                let (t, nv) = (map(t), vol(a, 0, piped));
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
            let (mut lfo0, mut instr) = (n.lfo0, n.instr.clone());
            let lfos = changes(&mut lfo0, n.lfos.iter().map(|&(t, l)| (map(t), l)), start, end);
            let waves = changes(&mut instr, n.waves.iter().map(|(t, w)| (map(*t), w.clone())), start, end);
            // (SiON has 255 tables; sweeps past that are left out.)
            let rtab = (!n.rel.is_empty()).then(|| {
                *tab_ids.entry(&n.rel[..]).or_insert_with(|| {
                    let i = free.next()?;
                    header_tabs.push(format!("#TABLE{i}{{{}}}", table_body(&n.rel, None)));
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
        tracks.extend(voices.into_iter().map(|(_, notes, drum)| Track { ch, notes, drum, x: None, rel: rel(ch), piped, tempo: vec![], end: 0, lp: None, vmax, kmilli: sc.kmilli, xdb: false }));
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
    // A looping song goes back to the tempo it had at the loop point, not the one it ended with: restate it there.
    let tm_lp = sc.lp.map(map).filter(|&l| l < song_end.max(map(sc.end)));
    if let (Some(l), Some(&(_, last))) = (tm_lp, tm.last()) {
        let i = tm.partition_point(|e| e.0 <= l);
        let at = tm[i - 1].1;
        if at != last && tm[i - 1].0 != l { tm.insert(i, (l, at)) }
    }
    let unit = [1.0, 10.0, 100.0, 1000.0].into_iter()
        .find(|u| tm.iter().all(|&(_, b)| ((b * u).round() / u - b).abs() / b < 1e-5)).unwrap_or(1000.0);
    // Envelope frames: the most common spacing of the notes' volume and detune changes (from their starts), when
    // that is a whole number of samples (`sion::RATE` / an integer `#FPS`, as SiON counts them) under one tempo.
    // (Release tables and pitch tables step at 1/60 s: only then can the frame be another.)
    let frame = match &tm[..] {
        [(_, bpm)] => {
            let bpm_w = (bpm * unit).round() / unit;
            let mut freq: HashMap<u64, usize> = HashMap::new();
            for n in tracks.iter().filter(|t| t.drum.is_none()).flat_map(|t| &t.notes) {
                let g = n.vols.iter().map(|v| v.0).chain(n.bends.iter().map(|b| b.0)).fold(0, |g, t| gcd(g, t - n.start));
                if g > 0 { *freq.entry(g).or_default() += 1 }
            }
            let sixty = tracks.iter().flat_map(|t| &t.notes).any(|n| n.rtab.is_some() || n.ptab.is_some());
            let rate = sion::RATE as f64;
            // (Or the longest part of it that is: a tracker's tick, where its rows are too long.)
            let valid = |g: u64| (1..=g).rev().filter(|d| g % d == 0).find_map(|f| {
                let samples = f as f64 * rate * 60.0 / (bpm_w * 480.0);
                let s = samples.round();
                let ok = (samples - s).abs() < 1e-6 && s >= 1.0 && rate % s == 0.0 && if sixty { s * 60.0 == rate } else { rate / s <= 1000.0 };
                ok.then_some((f, (rate / s) as u32))
            });
            // Of the common spacings' frames, the one most notes fit (a vibrato stepping every other frame fits the
            // frame too), the longest of those. (Not a rare spacing's: a few odd notes would make every table
            // longer.)
            let top = freq.values().copied().max().unwrap_or(0);
            freq.iter().filter(|&(_, &c)| c * 10 >= top).filter_map(|(&g, _)| valid(g)).max_by_key(|&(f, _)| (freq.iter().filter(|&(g, _)| g % f == 0).map(|x| x.1).sum::<usize>(), f))
        }
        _ => None,
    };
    // Keep trailing silence: the first non-drum track rests until the song's end.
    let end = map(sc.end);
    if end > song_end { if let Some(t) = tracks.iter_mut().find(|t| t.drum.is_none()) { t.end = end } }
    // A looping song: every track plays to the end, then goes back to the loop point together.
    let end = end.max(song_end);
    let lp = sc.lp.map(map).filter(|&l| l < end);
    if let Some(lp) = lp { for t in &mut tracks { (t.end, t.lp) = (end, Some(lp)) } }
    // (`x` in decibels only for volume tables: they hold the levels of `v`.)
    let xdb = sc.db && frame.is_some() && tracks.iter().any(|t| t.vol_tables());
    let mut header = vec![];
    if unit > 1.0 { header.push(format!("#TMODE{{unit={unit}}}")) }
    if xdb { header.push("#VMODE{%v1,1 %x1}".into()) } else if sc.db { header.push("#VMODE{%v1,1}".into()) } else if sc.fine { header.push("#VMODE{%v0,0}".into()) }

    header.extend(sc.header.iter().cloned());
    header.extend(header_tabs);
    for t in &mut tracks { t.xdb = xdb; t.set_x(x) }
    for (t, bpm) in tm {
        let val = (bpm * unit).round() as i64;
        if t == 0 && val as f64 == 120.0 * unit && tm_lp != Some(0) { continue }
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
    let notes = tracks.iter().map(|t| t.notes.len()).sum();
    let mut sections: Vec<u64> = sc.sections.iter().map(|&t| map(t)).filter(|&t| t < end).collect();
    sections.dedup();
    Arranged { header, tracks, notes, sections, lp, x, frame, free: free.collect() }
}

/// Per-frame volume and detune runs written as envelope tables: each note's run of values, one per envelope frame
/// from its key-on (where SiON's tables start), becomes a reference to a `#TABLE` instead of a slur per frame.
/// Notes share a table when it plays the same values for as long as they sound, and a run that settles into a
/// repeating figure (vibrato) loops it (`|`).
/// - Volume (`na`, absolute: the table sets `x` each frame): `x` and `v` multiply (or, in decibels, add), so
///   the track's master volume moves to its `v` and the table plays the note's `v` levels in `x`: the same
///   product. (`x` has 128 steps: only for tracks whose levels fit, and in decibels only with `%x1`.) Only for
///   tracks whose every note can have one: `x` stays where a table left it until the next table restarts, and
///   `v` never changes (a `v` sets `x` back to the track's, under a note still releasing). So a release the driver
///   silences (`Note::cut`) does it from a key-off table (`_na`: the level held, then 0) instead of `v0`.
/// - Detune (`np`): the pitch offset from the note's start, in 1/64 semitones.
#[derive(Default)]
pub struct Envelopes {
    /// `#TABLE` definitions.
    pub header: Vec<String>,
    /// (track, note) → its volume table, its key-off volume table (silencing its release), its pitch table.
    vol: HashMap<(usize, usize), usize>,
    cut: HashMap<(usize, usize), usize>,
    pitch: HashMap<(usize, usize), usize>,
    /// Notes sounding through the loop point → the volume and pitch tables of their part from there, which a key-on
    /// at the `$` plays on the way back. (Set after the `$`, mid-slur: a table only takes effect at a key-on.)
    vtail: HashMap<(usize, usize), usize>,
    ptail: HashMap<(usize, usize), usize>,
    /// Tracks with volume tables → their `v` (the master volume).
    master: HashMap<usize, u16>,
}

/// A table's values and its loop point (the values from there repeat; without one the last value holds).
type Table = (Vec<i32>, Option<usize>);

/// What table `t` plays at frame `i` (as SiON steps it).
fn tval(t: &Table, i: usize) -> i32 {
    let n = t.0.len();
    if i < n { t.0[i] } else { match t.1 { Some(l) if l < n => t.0[l + (i - l) % (n - l)], _ => t.0[n - 1] } }
}

/// What a value changing at `changes` (tick, value) from `init` is while note `n` sounds, at each of its first
/// `frames` envelope frames (`f` ticks) from its key-on.
fn per_frame<T: Copy>(n: &Note, init: T, changes: &[(u64, T)], f: u64, frames: usize) -> Vec<T> {
    let (mut v, mut it) = (init, changes.iter().peekable());
    (0..frames).map(|j| { while let Some(c) = it.next_if(|c| c.0 <= n.start + j as u64 * f) { v = c.1 } v }).collect()
}

/// Table `t` plays `run` (from its first frame on).
fn plays(t: &Table, run: &[i32]) -> bool { run.iter().enumerate().all(|(i, &x)| tval(t, i) == x) }

/// The shortest table that plays `s` for its length: a prefix, then a loop of the figure it settles into.
fn shortest(s: &[i32]) -> Table {
    let n = s.len();
    // (A held last value needs no loop.)
    let mut hold = n;
    while hold > 1 && s[hold - 1] == s[hold - 2] { hold -= 1 }
    let mut best: Table = (s[..hold].to_vec(), None);
    for p in 2..=(n / 2).min(256) {
        // The earliest point from which `s` repeats with period `p` to its end.
        let mut from = n;
        while from > p && s[from - 1] == s[from - 1 - p] { from -= 1 }
        let l = from - p;
        if l + p < best.0.len() { best = (s[..l + p].to_vec(), Some(l)) }
    }
    best
}

impl Arranged {
    /// The envelope tables for the tracks' per-frame runs (see `Envelopes`), in the table numbers left free.
    /// `lines`: a note keeps the table before it only within a line (see below).
    pub fn envelopes(&self, lines: bool) -> Envelopes {
        let mut env = Envelopes::default();
        let Some((f, fps)) = self.frame else { return env };
        let frames = |n: &Note| (n.end - n.start).div_ceil(f) as usize;
        let at = |n: &Note, t: u64| (t - n.start) % f == 0;
        // (Output frames per envelope frame: `f` ticks.)
        let spf = (sion::RATE as f64 / fps as f64).round() as u64;
        // Runs: (kind, values, track, note); kinds: 0 volume, 1 key-off volume, 2 volume from the loop point (of a
        // note sounding through it), 3 pitch, 4 pitch from the loop point.
        let mut runs: Vec<(u8, Vec<i32>, usize, usize)> = vec![];
        for (ti, tr) in self.tracks.iter().enumerate().filter(|(_, t)| t.drum.is_none()) {
            // Volume: the notes' `v` levels, in `x`. (Linear `v` in 256 steps fits `x` up to 128; with `v` in
            // decibels `x` has to be too.)
            let scale = if tr.vmax == 256 { !tr.kmilli } else { tr.xdb };
            // (A note whose levels don't fit a table has them as slurs, in `x`.)
            if scale && tr.vol_tables() {
                for (ni, n) in tr.notes.iter().enumerate() {
                    let last = n.vols.last().map_or(n.v, |v| v.1) as i32;
                    // (From the key-off: the level the note ended at until the cut, then silence.)
                    if let Some(c) = tr.cut(ni).filter(|c| (c - n.end) % f == 0) { let mut r = vec![last; ((c - n.end) / f) as usize]; r.push(0); runs.push((1, r, ti, ni)) }
                    if !tr.vol_table(n, f) { continue }
                    let run: Vec<i32> = per_frame(n, n.v, &n.vols, f, frames(n)).into_iter().map(i32::from).collect();
                    if let Some(l) = tr.through(n) { runs.push((2, run[((l - n.start) / f) as usize..].to_vec(), ti, ni)) }
                    runs.push((0, run, ti, ni));
                }
                env.master.insert(ti, tr.x.unwrap_or(128) as u16);
            }
            for (ni, n) in tr.notes.iter().enumerate() {
                // (Not for a note that sounds through the loop point off its frames (keyed on afresh there on the way
                // back, it plays a table from there), nor one split off the frames by its other changes (a slurred note
                // starts from its own pitch, until the next frame), nor a sample keyed on between output frames
                // (a command's pitch change takes effect partway into one, a table's at its start).)
                let splits = n.vols.iter().map(|v| v.0).chain(n.pans.iter().map(|p| p.0)).chain(n.lfos.iter().map(|l| l.0)).chain(n.waves.iter().map(|w| w.0));
                let pcm = (n.instr.starts_with("%7") || n.instr.starts_with("%10")) && n.start * spf % f != 0;
                let spans = tr.through(n).is_some_and(|l| !at(n, l));
                if tr.kmilli || pcm || spans || n.bends.is_empty() || n.ptab.is_some() || !n.bends.iter().map(|b| b.0).chain(splits).all(|t| at(n, t)) { continue }
                let run: Vec<i32> = per_frame(n, n.k0, &n.bends, f, frames(n)).into_iter().map(|k| k - n.k0).collect();
                if let Some(l) = tr.through(n) { runs.push((4, run[((l - n.start) / f) as usize..].to_vec(), ti, ni)) }
                runs.push((3, run, ti, ni));
            }
        }
        // Volume first (a track has all its volume tables or none: they get the numbers before pitch tables, which
        // can do without), and longest first, so shorter runs find a table that starts as they do.
        runs.sort_by_key(|r| (r.0 > 2, std::cmp::Reverse(r.1.len())));
        // (Key-off volume tables are volume tables too: they share.)
        let mut tables: Vec<(bool, Table)> = vec![];
        for (kind, run, ti, ni) in &runs {
            let vol = *kind <= 2;
            let found = tables.iter().position(|t| t.0 == vol && plays(&t.1, run));
            let id = found.or_else(|| {
                // (A table is for an envelope, not a whole part: past `MAX_TABLE` values the note keeps its slurs. A
                // key-off table's held level counts once: however long the release, it is one run, `(v)n`.)
                let t = shortest(run);
                let size = if *kind == 1 { t.0.chunk_by(|a, b| a == b).count() } else { t.0.len() };
                if size > MAX_TABLE || tables.len() >= self.free.len() { return None }
                tables.push((vol, t));
                Some(tables.len() - 1)
            });
            match (id, kind) {
                (Some(id), 0) => { env.vol.insert((*ti, *ni), id); }
                (Some(id), 1) => { env.cut.insert((*ti, *ni), id); }
                (Some(id), 2) => { env.vtail.insert((*ti, *ni), id); }
                (Some(id), 3) => { env.pitch.insert((*ti, *ni), id); }
                (Some(id), _) => { env.ptail.insert((*ti, *ni), id); }
                // (Or the note keeps its slurs.)
                (None, _) => {}
            }
        }
        // (A note's pitch table only with the one from the loop point, where it sounds through it.)
        env.pitch.retain(|k, _| { let t = &self.tracks[k.0]; t.through(&t.notes[k.1]).is_none() || env.ptail.contains_key(k) });
        // (Likewise a volume table through the loop point.)
        env.vol.retain(|k, _| { let t = &self.tracks[k.0]; t.through(&t.notes[k.1]).is_none() || env.vtail.contains_key(k) });
        env.vtail.retain(|k, _| env.vol.contains_key(k));
        // A pitch table has to be shorter to write than the slurs it replaces (about `SLUR` characters a change),
        // with a reference (`np12`) per note that uses it.
        let mut gain: HashMap<usize, i64> = HashMap::new();
        for (&(ti, ni), &id) in &env.pitch { *gain.entry(id).or_default() += SLUR * self.tracks[ti].notes[ni].bends.len() as i64 - 5 }
        for (id, (_, t)) in tables.iter().enumerate().filter(|t| !t.1.0) {
            if gain.get(&id).is_some_and(|&g| g < 12 + text_len(t)) { env.pitch.retain(|_, v| *v != id) }
        }
        env.ptail.retain(|k, _| env.pitch.contains_key(k));
        // A note keeps the table the note before it had wherever that plays its values too (a note without
        // vibrato under a vibrato that starts later, an envelope cut short): fewer table changes to read. (With
        // `lines`, within a line only: a line that comes back then starts the same wherever it is, and stays a loop
        // or macro.)
        let mut bounds: Vec<u64> = if lines { self.sections.iter().copied().chain(self.lp).collect() } else { vec![] };
        bounds.sort_unstable();
        let plays = |id: usize, run: &[i32]| plays(&tables[id].1, run);
        let run_of: HashMap<(u8, usize, usize), &[i32]> = runs.iter().map(|r| ((r.0, r.2, r.3), &r.1[..])).collect();
        for (ti, tr) in self.tracks.iter().enumerate() {
            let (mut cv, mut cc, mut cp): (Option<usize>, Option<usize>, Option<usize>) = (None, None, None);
            for (ni, n) in tr.notes.iter().enumerate() {
                let prev = ni.checked_sub(1).map_or(0, |p| tr.notes[p].start);
                if ni == 0 || bounds[bounds.partition_point(|&b| b <= prev)..].first().is_some_and(|&b| b <= n.start) { (cv, cc, cp) = (None, None, None) }
                // (A note sounding through the loop point only with its tables from there.)
                let through = tr.through(n).is_some();
                if env.master.contains_key(&ti) {
                    if let (Some(c), Some(run)) = (cv, run_of.get(&(0, ti, ni)).filter(|_| !through || env.vtail.contains_key(&(ti, ni)))) { if plays(c, run) { env.vol.insert((ti, ni), c); } }
                    cv = env.vol.get(&(ti, ni)).copied();
                    if let (Some(c), Some(run)) = (cc, run_of.get(&(1, ti, ni))) { if plays(c, run) { env.cut.insert((ti, ni), c); } }
                    cc = env.cut.get(&(ti, ni)).copied();
                }
                let zeros = vec![0; frames(n)];
                let run = run_of.get(&(3, ti, ni)).copied().or((n.bends.is_empty() && n.ptab.is_none()).then_some(&zeros[..])).filter(|_| !through || env.ptail.contains_key(&(ti, ni)));
                if let (Some(c), Some(run)) = (cp, run) { if plays(c, run) { env.pitch.insert((ti, ni), c); } }
                cp = env.pitch.get(&(ti, ni)).copied();
            }
        }
        // Numbered last, the tables used by the most notes first: the shortest numbers where they are written most.
        let mut uses: HashMap<usize, usize> = HashMap::new();
        for &id in env.vol.values().chain(env.cut.values()).chain(env.pitch.values()).chain(env.vtail.values()).chain(env.ptail.values()) { *uses.entry(id).or_default() += 1 }
        let mut used: Vec<(usize, usize)> = uses.into_iter().collect();
        used.sort_by_key(|&(id, n)| (std::cmp::Reverse(n), id));
        let num: HashMap<usize, usize> = used.iter().zip(&self.free).map(|(&(id, _), &k)| (id, k)).collect();
        for m in [&mut env.vol, &mut env.cut, &mut env.pitch, &mut env.vtail, &mut env.ptail] { for id in m.values_mut() { *id = num[id] } }
        // (`free` ascends, so this is in number order.)
        for (&(id, _), &k) in used.iter().zip(&self.free) { let t = &tables[id].1; env.header.push(format!("#TABLE{k}{{{}}}", table_body(&t.0, t.1))) }
        // (Only the tables step at the frame rate: without them it needn't be said.)
        if fps != 60 && !env.header.is_empty() { env.header.insert(0, format!("#FPS{{{fps}}}")) }
        env
    }
}

/// Most values a volume or pitch table holds (before its loop repeats).
const MAX_TABLE: usize = 256;
/// What a change written as a slur costs, about (`&k5o6d24`).
const SLUR: i64 = 8;

/// A `#TABLE`'s numbers, the loop point marked. (SiON reads a value as `(int)(v + 0.5)`: a negative one is
/// written a step lower.)
fn table_body(vals: &[i32], lp: Option<usize>) -> String {
    let text = |v: i32| if v < 0 { (v - 1).to_string() } else { v.to_string() };
    vals.iter().enumerate().map(|(i, &v)| if Some(i) == lp { format!("|{}", text(v)) } else { text(v) }).collect::<Vec<_>>().join(",")
}

/// Length of a table's definition as written (runs compacted).
fn text_len(t: &Table) -> i64 { crate::mml::compact(&format!("#TABLE0{{{}}}", table_body(&t.0, t.1))).len() as i64 }

/// A value's changes while a note sounds (from `start` to `end`): the last at or before `start` becomes `init`,
/// and of changes on the same tick only the last counts; ones that repeat the value before them are left out.
fn changes<T: PartialEq>(init: &mut T, src: impl Iterator<Item = (u64, T)>, start: u64, end: u64) -> Vec<(u64, T)> {
    let mut out: Vec<(u64, T)> = vec![];
    for (t, x) in src {
        if t <= start { *init = x; continue }
        if t >= end { break }
        if out.last().is_some_and(|o| o.0 == t) { out.pop(); }
        if out.last().map_or(&*init, |o| &o.1) != &x { out.push((t, x)) }
    }
    out
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

/// The track as tokens. With `gate` the key-off times are encoded through `q` and `@q` where shorter (choosing
/// their states by dynamic programming, each change of state costing that many characters more: a readable
/// track keeps its gate); otherwise every gap is an explicit rest under `q8`. With `envs` (the song's tables and
/// this track's number) the notes' per-frame runs that have a table play from it instead of slurs; or, without
/// `tables`, as slurs with the levels the tables would hold in `x` (the plain MML for the same arithmetic).
pub fn tokens(tr: &Track, gate: Option<u32>, lens: &Lengths, envs: Option<(&Envelopes, usize)>, tables: bool) -> Vec<Tok> {
    let mut out = vec![];
    match tr.drum { Some(true) => out.push(Tok::Cmd("q0".into())), Some(false) => out.push(Tok::Cmd("q8".into())), None if gate.is_none() => out.push(Tok::Cmd("q8".into())), None => {} }
    // (A track with volume tables has its master volume in `v`: see `Envelopes`.)
    let master = envs.and_then(|(e, ti)| e.master.get(&ti).copied());
    if let Some(x) = tr.x.filter(|_| master.is_none()) { out.push(Tok::Cmd(format!("x{x}"))) }
    // (Set before anything else, so it's the track's one `v`: the loop point has it too.)
    let mut v = tr.vmax;
    if let Some(m) = master.filter(|&m| m != tr.vmax) { out.push(Tok::Cmd(format!("v{m}"))); v = m }
    // (Levels in `x`: written as `x` commands where a note has no table.)
    let (xform, mut xl) = (master.is_some(), 128u16);
    let (mut t, mut tempo) = (0, tr.tempo.iter().peekable());
    let (mut instr, mut pan, mut ptab, mut env, mut k, mut ph) = ("", 0, None, "", 0, (0.0, 0.0));
    let (mut lfo, mut rtab, mut cut): ((i32, i32), Option<usize>, Option<u64>) = ((0, 0), None, None);
    let (mut vtab, mut ctab): (Option<usize>, Option<usize>) = (None, None);
    let vtab_cmd = |t: Option<usize>| t.map_or("na".into(), |t| format!("na{t}"));
    let ctab_cmd = |t: Option<usize>| t.map_or("_na".into(), |t| format!("_na{t}"));
    let lfo_cmds = |out: &mut Vec<Tok>, cur: &mut (i32, i32), to: (i32, i32)| {
        if to.0 != cur.0 { out.push(Tok::Cmd(format!("mp{}", to.0))) }
        if to.1 != cur.1 { out.push(Tok::Cmd(format!("ma{}", to.1))) }
        *cur = to;
    };
    let kcmd = |k: i32| if tr.kmilli { format!("k{}", k as f64 / 1000.0) } else { format!("k{k}") };
    let ph_cmd = |(ofs, len): (f64, f64)| if len > 0.0 { format!("@ph{ofs},{len}") } else { format!("@ph{ofs}") };
    let rtab_cmd = |r: Option<usize>| r.map_or("_np".into(), |i| format!("_np{i}"));
    let ptab_cmd = |p: Option<u8>| p.map_or("np".into(), |t| format!("np{t}"));
    let mut gates = vec![]; // (index of the note's first piece, index of its last piece)
    // Rest until `until`, putting the tempo changes on the way (and one at `until` itself with `incl`).
    let rest_to = |out: &mut Vec<Tok>, t: &mut u64, until: u64, incl: bool, tempo: &mut std::iter::Peekable<std::slice::Iter<(u64, String)>>| {
        while let Some((tt, s)) = tempo.next_if(|x| x.0 < until || (incl && x.0 == until)) {
            if *tt > *t { out.push(Tok::Rest((*tt - *t) as u32)); *t = *tt }
            out.push(Tok::Cmd(s.clone()));
        }
        if until > *t { out.push(Tok::Rest((until - *t) as u32)); *t = until }
    };
    // The loop point (`$`), and the channel state there, which the end of the track restores before going back.
    struct Snap<'a> { instr: &'a str, env: &'a str, v: u16, pan: i32, ptab: Option<u8>, k: i32, ph: (f64, f64), lfo: (i32, i32), rtab: Option<usize>, vtab: Option<usize>, ctab: Option<usize> }
    let mut mark: Option<Snap> = None;
    macro_rules! loop_point { () => {{
        out.push(Tok::Cmd("$".into()));
        mark = Some(Snap { instr, env, v, pan, ptab, k, ph, lfo, rtab, vtab, ctab });
        // (Levels in `x`: the next note sets its own, as a table does, rather than the end restoring the level
        // here under a release still sounding as it goes back. No note of a track with tables sounds through it.)
        #[allow(unused_assignments)]
        if xform { xl = u16::MAX };
    }} }
    // Rest until `until`, putting the loop point on the way.
    macro_rules! advance { ($until:expr) => {{
        let until = $until;
        if let Some(l) = tr.lp.filter(|&l| mark.is_none() && l >= t && l <= until) {
            // (A tempo change at the loop point goes after the `$`, so going back plays it again.)
            rest_to(&mut out, &mut t, l, false, &mut tempo);
            loop_point!()
        }
        rest_to(&mut out, &mut t, until, true, &mut tempo);
    }} }
    // A release silenced (on the chip, by its driver) at `c`.
    macro_rules! silence { ($c:expr) => {{
        advance!($c);
        #[allow(unused_assignments)]
        if xform { out.push(Tok::Cmd("x0".into())); xl = 0 } else { out.push(Tok::Cmd("v0".into())); v = 0 }
    }} }
    for (ni, n) in tr.notes.iter().enumerate() {
        let get = |m: fn(&Envelopes) -> &HashMap<(usize, usize), usize>| envs.filter(|_| tables).and_then(|(e, ti)| m(e).get(&(ti, ni)).copied());
        let (ntab, npt, nctab, tails) = (get(|e| &e.vol), get(|e| &e.pitch), get(|e| &e.cut), (get(|e| &e.vtail), get(|e| &e.ptail)));
        let nv = master.unwrap_or(n.v);
        let nptab = npt.map(|t| t as u8).or(n.ptab);
        // The previous note's release silenced (on the chip, by its driver) before this note.
        if let Some(c) = cut.take() { silence!(c) }
        advance!(n.start);
        // A module change (`%`) resets the tone and envelope; so going back to the module's defaults needs one.
        // (Both also reset the LFO: `%` the whole channel, `@lfo` in a tone its depths.)
        // (Only the waveform of a wavetable module, under the same envelope and no LFO: `@n` swaps it.)
        let wave = n.instr != instr && n.env == env && lfo == (0, 0) && crate::score::same_wave(instr, &n.instr);
        if wave { out.push(Tok::Cmd(crate::score::wave_cmd(&n.instr))); instr = &n.instr }
        else if n.instr != instr || (n.env.is_empty() && !env.is_empty()) { out.push(Tok::Cmd(n.instr.clone())); (instr, env, ph, lfo) = (&n.instr, "", (0.0, 0.0), (0, 0)) }
        if n.env != env {
            // (An LFO rate the channel already has needn't be set again: that would only reset its depths.)
            let ((_, ra), (b, rb)) = (split_lfo(env), split_lfo(&n.env));
            if ra != rb { out.push(Tok::Cmd(n.env.clone())); lfo = (0, 0) } else if !b.is_empty() { out.push(Tok::Cmd(b.into())) }
            env = &n.env;
        }
        lfo_cmds(&mut out, &mut lfo, n.lfo0);
        if n.rtab != rtab { out.push(Tok::Cmd(rtab_cmd(n.rtab))); rtab = n.rtab }
        if ntab != vtab { out.push(Tok::Cmd(vtab_cmd(ntab))); vtab = ntab }
        if master.is_some() && nctab != ctab { out.push(Tok::Cmd(ctab_cmd(nctab))); ctab = nctab }
        if nv != v { out.push(Tok::Cmd(format!("v{nv}"))); v = nv }
        if xform && ntab.is_none() && n.v != xl { out.push(Tok::Cmd(format!("x{}", n.v))); xl = n.v }
        if n.pan != pan { out.push(Tok::Cmd(format!("@p{}", n.pan))); pan = n.pan }
        if nptab != ptab { out.push(Tok::Cmd(ptab_cmd(nptab))); ptab = nptab }
        if n.k0 != k { out.push(Tok::Cmd(kcmd(n.k0))); k = n.k0 }
        if (n.ofs, n.plen) != ph { ph = (n.ofs, n.plen); out.push(Tok::Cmd(ph_cmd(ph))) }
        // Pieces joined by slurs where the pitch bend or volume changes or a tempo change falls mid-note.
        let first = out.len();
        let (bends, vls) = (if npt.is_some() { &[][..] } else { &n.bends[..] }, if ntab.is_some() { &[][..] } else { &n.vols[..] });
        let (mut bend, mut vols, mut pans) = (bends.iter().peekable(), vls.iter().peekable(), n.pans.iter().peekable());
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
            let nlp = tr.through(n).filter(|_| mark.is_none());
            let Some(split) = [nb, nv, np, nl, nw, nt, nlp].into_iter().flatten().min() else { break };
            if split > at { out.push(Tok::Note { key: key as u8, len: (split - at) as u32, slur: true }); at = split }
            if nlp == Some(split) {
                let level = xl;
                loop_point!();
                // (On the way back the note is keyed on afresh here: from its tables from here on, or the level it
                // is at.)
                if let Some(t) = tails.0 { out.push(Tok::Cmd(vtab_cmd(Some(t)))); vtab = Some(t) }
                if let Some(t) = tails.1 { out.push(Tok::Cmd(ptab_cmd(Some(t as u8)))); ptab = Some(t as u8) }
                if let Some(m) = &mut mark { (m.vtab, m.ptab) = (vtab, ptab) }
                if xform && ntab.is_none() && !vols.peek().is_some_and(|b| b.0 == split) { out.push(Tok::Cmd(format!("x{level}"))); xl = level }
            }
            while let Some((_, s)) = tempo.next_if(|x| x.0 == split) { out.push(Tok::Cmd(s.clone())) }
            while let Some(&(_, nk)) = bend.next_if(|b| b.0 == split) {
                let total = n.key as i32 * unit + nk;
                if (total - key * unit).abs() >= unit { key = ((total as f64 / unit as f64).round() as i32).clamp(0, 127) }
                let nk = total - key * unit;
                if nk != k { out.push(Tok::Cmd(kcmd(nk))); k = nk }
            }
            while let Some(&(_, nv)) = vols.next_if(|b| b.0 == split) {
                if xform { if nv != xl { out.push(Tok::Cmd(format!("x{nv}"))); xl = nv } }
                else if nv != v { out.push(Tok::Cmd(format!("v{nv}"))); v = nv }
            }
            while let Some(&(_, np)) = pans.next_if(|b| b.0 == split) {
                if np != pan { out.push(Tok::Cmd(format!("@p{np}"))); pan = np }
            }
            while let Some(&(_, nl)) = lfos.next_if(|b| b.0 == split) { lfo_cmds(&mut out, &mut lfo, nl) }
            // (Same module: `@n` changes the waveform and leaves the note, envelope and LFO as they are.)
            while let Some((_, w)) = waves.next_if(|b| b.0 == split) {
                if *w != instr { out.push(Tok::Cmd(crate::score::wave_cmd(w))); instr = w }
            }
        }
        out.push(Tok::Note { key: key as u8, len: (n.end - at) as u32, slur: false });
        gates.push((first, out.len() - 1));
        t = n.end;
        // (A volume-table track silences the release by its key-off table.)
        cut = tr.cut(ni).filter(|_| master.is_none() || nctab.is_none());
        // (A table leaves `x` where it ends.)
        if ntab.is_some() || nctab.is_some() { xl = u16::MAX }
    }
    // (After the last note, only within the song.)
    if let Some(c) = cut { silence!(c) }
    let rest: Vec<u64> = tempo.clone().map(|x| x.0).collect();
    if let Some(&end) = rest.last() { advance!(end) }
    if tr.end > t || tr.lp.is_some() { advance!(tr.end) }
    // Back to the loop point: set the channel as it was there. (Before the first note there is no tone to go
    // back to: the first note sets its own.)
    if let Some(s) = mark.take() {
        if !s.instr.is_empty() {
            // (A tone (an FM voice) sets the channel's volume from `x`, which tables leave at full: first `x` is set to
            // the level the last note's release is at, so that carries on as it was.)
            if envs.is_some() && master.is_some() && tables && (s.instr != instr || s.env != env) {
                let n = &tr.notes[tr.notes.len() - 1];
                out.push(Tok::Cmd(format!("x{}", if ctab.is_some() { 0 } else { n.vols.last().map_or(n.v, |c| c.1) })));
            }
            if s.instr != instr || (s.env.is_empty() && !env.is_empty()) { out.push(Tok::Cmd(s.instr.into())); (env, ph, lfo) = ("", (0.0, 0.0), (0, 0)) }
            if s.env != env { out.push(Tok::Cmd(s.env.into())); lfo = (0, 0) }
            lfo_cmds(&mut out, &mut lfo, s.lfo);
            if s.ph != ph { out.push(Tok::Cmd(ph_cmd(s.ph))) }
        }
        if s.rtab != rtab { out.push(Tok::Cmd(rtab_cmd(s.rtab))) }
        if s.vtab != vtab { out.push(Tok::Cmd(vtab_cmd(s.vtab))) }
        if s.ctab != ctab { out.push(Tok::Cmd(ctab_cmd(s.ctab))) }
        if s.v != v { out.push(Tok::Cmd(format!("v{}", s.v))) }
        if s.pan != pan { out.push(Tok::Cmd(format!("@p{}", s.pan))) }
        if s.ptab != ptab { out.push(Tok::Cmd(ptab_cmd(s.ptab))) }
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
    if let Some(steady) = gate { gate_dp(&mut out, &gates, lens, steady) }
    out
}

/// A tone's envelope commands split before its LFO rate (`@3@lfo9` → `@3`, `@lfo9`).
fn split_lfo(e: &str) -> (&str, &str) { e.find("@lfo").map_or((e, ""), |i| e.split_at(i)) }

/// Choose a gate per note so that "note + rest" is written shortest: `q` (q/8 of the written length) and `@q`
/// (that much earlier: the key-off a fixed time before the next note, as many drivers do), as states chosen by
/// dynamic programming. `q8` and an explicit rest always works; e.g. under `q4` a 1/8 note followed by a 1/8
/// rest is just `c4`, and under `@q20` a note keyed off 1/96 before the next one needs no rest either.
fn gate_dp(out: &mut Vec<Tok>, gates: &[(usize, usize)], lens: &Lengths, steady: u32) {
    const INF: u32 = u32::MAX / 4;
    let note_rest = |gi: usize| -> (u32, u32) {
        let Tok::Note { len: g, .. } = out[gi] else { unreachable!() };
        (g, match out.get(gi + 1) { Some(Tok::Rest(r)) => *r, _ => 0 })
    };
    // `@q` candidates: the gaps (in SiON's 1/192 notes, so multiples of 10 here) that follow notes most often.
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for &(_, gi) in gates { let (g, r) = note_rest(gi); if r > 0 && r % 10 == 0 && r < g { *freq.entry(r).or_default() += 1 } }
    let mut abs: Vec<(u32, usize)> = freq.into_iter().filter(|&(_, n)| n >= 4).collect();
    abs.sort_by_key(|&(r, n)| (std::cmp::Reverse(n), r));
    let abs: Vec<u32> = std::iter::once(0).chain(abs.into_iter().take(3).map(|x| x.0)).collect();
    let na = abs.len();
    // State `q * na + a`: gate q/8 of the length, minus `abs[a]`.
    let ns = 9 * na;
    let opts: Vec<Vec<Option<(u32, u32, u32)>>> = gates.iter().map(|&(_, gi)| {
        let (g, r) = note_rest(gi);
        let mut o = vec![None; ns];
        for q in 1..9u32 {
            for (ai, &a) in abs.iter().enumerate() {
                if (8 * (g + a)) % q != 0 { continue }
                let l = 8 * (g + a) / q;
                if l > g + r { continue }
                let rr = g + r - l;
                o[q as usize * na + ai] = Some((lens.cost(l, 0) + if rr > 0 { 1 + lens.cost(rr, 0) } else { 0 }, l, rr));
            }
        }
        o
    }).collect();
    let cmd_cost: Vec<u32> = abs.iter().map(|a| 2 + (a / 10).to_string().len() as u32).collect();
    let switch = |p: usize, s: usize| (if p / na != s / na { 2 + steady } else { 0 }) + if p % na != s % na { cmd_cost[s % na] + steady } else { 0 };
    let mut cost = vec![INF; ns];
    cost[6 * na] = 0; // SiON's default is q6 and no `@q`
    let mut back = vec![vec![0u16; ns]; gates.len()];
    for (i, o) in opts.iter().enumerate() {
        let mut nc = vec![INF; ns];
        for s in na..ns {
            let Some((c, ..)) = o[s] else { continue };
            for p in na..ns {
                if cost[p] >= INF { continue }
                let x = cost[p] + c + switch(p, s);
                if x < nc[s] { nc[s] = x; back[i][s] = p as u16 }
            }
        }
        cost = nc;
    }
    let mut st = (na..ns).min_by_key(|&s| cost[s]).unwrap_or(8 * na);
    let mut pick = vec![0; gates.len()];
    for i in (0..gates.len()).rev() { pick[i] = st; st = back[i][st] as usize }
    let used_abs = pick.iter().any(|&s| s % na != 0);
    let cmds = |res: &mut Vec<Tok>, from: Option<usize>, to: usize| {
        if from.is_none_or(|f| f / na != to / na) { res.push(Tok::Cmd(format!("q{}", to / na))) }
        if from.map_or(used_abs, |f| f % na != to % na) { res.push(Tok::Cmd(format!("@q{}", abs[to % na] / 10))) }
    };
    let mut res = Vec::with_capacity(out.len() + 16);
    let (mut gi, mut prev) = (0, Some(6 * na));
    let mut skip_rest = false;
    for (i, tok) in out.drain(..).enumerate() {
        if skip_rest { skip_rest = false; if matches!(tok, Tok::Rest(_)) { continue } }
        // After the loop point the gate is whatever the track's end left it at: set it again.
        if matches!(&tok, Tok::Cmd(c) if c == "$") {
            res.push(tok);
            if gi < gates.len() { cmds(&mut res, None, pick[gi]); prev = Some(pick[gi]) }
            continue;
        }
        if gi < gates.len() && i == gates[gi].0 && prev != Some(pick[gi]) { cmds(&mut res, prev, pick[gi]); prev = Some(pick[gi]) }
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

#[cfg(test)]
mod table_tests {
    use super::{shortest, tval};

    #[test]
    fn runs_become_the_shortest_table_that_plays_them() {
        // A vibrato after a delay loops its figure; a decay that settles holds its last value; each plays the run.
        let vib: Vec<i32> = [0, 0, 0].into_iter().chain([4, 8, 4, 0, -4, -8, -4, 0].repeat(5)).take(39).collect();
        let decay = vec![20, 16, 13, 10, 10, 10, 10];
        for run in [vib, decay, vec![5], vec![1, 2, 1, 2, 1]] {
            let t = shortest(&run);
            assert!(run.iter().enumerate().all(|(i, &x)| tval(&t, i) == x), "{run:?} → {t:?}");
        }
        assert_eq!(shortest(&[20, 16, 13, 10, 10, 10, 10]), (vec![20, 16, 13, 10], None));
        assert_eq!(shortest(&[0, 0, 0, 4, 8, 4, 0, -4, -8, -4, 0, 4, 8, 4, 0, -4, -8, -4, 0, 4]).1, Some(2));
    }
}
