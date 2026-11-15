//! Readable MML: the song laid out the way it would be written by hand, instead of minified.
//!
//! - Each track is cut at the score's sections (a tracker's patterns, as the order list plays them) and each
//!   section is one line: it starts with an absolute octave and restates whatever state (volume, pan, tone...)
//!   changes in the track, so a line means the same wherever it is played.
//! - Sections that follow each other again are loops (`[A B]2`, `[A B|C]2`); a section that comes back
//!   elsewhere is a macro (`#A=...;`, per track, defined just before it).
//! - Within a line: one default length (`l`) per track, relative octaves, notes grouped by beat, short
//!   repeated figures as loops (`[v11c&v13c&]4`). Black keys are spelled as flats in flat keys.
//! - Wavetables and samples go at the end, where the long data doesn't get in the way of reading the music.

use crate::mml::{Lengths, Tok};
use std::collections::HashMap;

const WHOLE: u32 = 1920;
const BEAT: u64 = 480;

/// State that a command sets, by kind; `None` for commands that are not state (tempo, `$`).
fn kind(c: &str) -> Option<&'static str> {
    Some(match c {
        _ if c.starts_with('%') => "instr",
        _ if c.starts_with("@ph") => "ph",
        _ if c.starts_with("@q") => "qa",
        _ if c.starts_with("@p") => "pan",
        _ if c.starts_with("@lfo") => "lfo",
        _ if c.starts_with("@v") => "fv",
        _ if c.starts_with('@') => "env",
        _ if c.starts_with("_np") => "rtab",
        _ if c.starts_with("_na") => "ctab",
        _ if c.starts_with("np") => "ptab",
        _ if c.starts_with("na") => "vtab",
        _ if c.starts_with("mp") => "mp",
        _ if c.starts_with("ma") => "ma",
        _ if c.starts_with('v') => "v",
        _ if c.starts_with('k') => "k",
        _ if c.starts_with('q') => "q",
        _ if c.starts_with('x') => "x",
        _ => return None,
    })
}

/// Restated in this order (a module change `%` must come before its tone).
/// The tone: set where it changes, not restated.
const TONE: [&str; 6] = ["instr", "env", "lfo", "ph", "mp", "ma"];

const KINDS: [&str; 17] = ["x", "q", "qa", "instr", "env", "lfo", "fv", "ph", "mp", "ma", "rtab", "ptab", "vtab", "ctab", "v", "pan", "k"];

#[derive(Clone, Default, PartialEq)]
struct State(HashMap<&'static str, String>);

impl State {
    fn set(&mut self, k: &'static str, c: &str) {
        // (A module change resets the tone, its LFO and the sample position.)
        if k == "instr" { for r in ["env", "lfo", "ph", "mp", "ma"] { self.0.remove(r); } }
        // (And an LFO setting, alone or in a tone, its depths.)
        if k == "lfo" || (k == "env" && c.contains("@lfo")) { for r in ["mp", "ma"] { self.0.remove(r); } }
        self.0.insert(k, c.to_string());
    }
}

/// Split a track's tokens at the boundaries (ticks): notes and rests crossing one are split there (a note
/// slurred into its continuation). Returns the pieces and which piece the loop point (`$`) starts.
/// Notes are split only where that sounds the same: not under a pitch table (a slurred note starts at its own
/// pitch, not where the table has taken it) or a gate other than `q8` and no `@q` (each piece would be keyed off
/// at its own gate); those run on into the next section. `st`: the state set before the tokens.
fn split(toks: &[Tok], bounds: &[u64], st: &State) -> (Vec<Vec<Tok>>, Option<usize>) {
    let (mut pieces, mut cur, mut t, mut bi, mut lp) = (vec![], vec![], 0u64, 0, None);
    let mut ptab = st.0.get("ptab").is_some_and(|c| c != "np");
    let (mut q, mut qa) = (st.0.get("q").is_none_or(|c| c != "q8"), st.0.get("qa").is_some_and(|c| c != "@q0"));
    for tok in toks {
        while bi < bounds.len() && bounds[bi] <= t { pieces.push(std::mem::take(&mut cur)); bi += 1 }
        match tok {
            Tok::Cmd(c) if c == "$" => lp = Some(pieces.len()),
            Tok::Cmd(c) if kind(c) == Some("ptab") => { ptab = c != "np"; cur.push(tok.clone()) }
            Tok::Cmd(c) if kind(c) == Some("q") => { q = c != "q8"; cur.push(tok.clone()) }
            Tok::Cmd(c) if kind(c) == Some("qa") => { qa = c != "@q0"; cur.push(tok.clone()) }
            Tok::Note { key, len, slur } if ptab || q || qa => {
                cur.push(Tok::Note { key: *key, len: *len, slur: *slur });
                t += *len as u64;
                while bi < bounds.len() && bounds[bi] < t { bi += 1 }
            }
            Tok::Note { key, len, slur } => {
                let mut len = *len as u64;
                while bi < bounds.len() && t + len > bounds[bi] {
                    let part = bounds[bi] - t;
                    cur.push(Tok::Note { key: *key, len: part as u32, slur: true });
                    pieces.push(std::mem::take(&mut cur));
                    (t, len, bi) = (bounds[bi], len - part, bi + 1);
                }
                cur.push(Tok::Note { key: *key, len: len as u32, slur: *slur });
                t += len;
            }
            Tok::Rest(len) => {
                let mut len = *len as u64;
                while bi < bounds.len() && t + len > bounds[bi] {
                    let part = bounds[bi] - t;
                    cur.push(Tok::Rest(part as u32));
                    pieces.push(std::mem::take(&mut cur));
                    (t, len, bi) = (bounds[bi], len - part, bi + 1);
                }
                cur.push(Tok::Rest(len as u32));
                t += len;
            }
            Tok::Cmd(_) => cur.push(tok.clone()),
        }
    }
    pieces.push(cur);
    // (Commands at the very end, restoring the tone for the loop, go on the last line.)
    if pieces.len() > 1 && pieces.last().unwrap().iter().all(|t| matches!(t, Tok::Cmd(_))) && lp != Some(pieces.len() - 1) {
        let tail = pieces.pop().unwrap();
        pieces.last_mut().unwrap().extend(tail);
    }
    (pieces, lp)
}

/// Text of a length of `len` ticks under default length `l`: a single note value where there is one (`4`,
/// `8.`, `12`), otherwise tied values from the longest (`4^16`).
fn len_text(len: u32, l: u32, lens: &Lengths) -> String {
    if len == l { return String::new() }
    let single = |x: u32| -> Option<String> {
        if x > 0 && WHOLE % x == 0 { return Some((WHOLE / x).to_string()) }
        if x >= 3 && x % 3 == 0 && WHOLE % (x / 3 * 2) == 0 && (WHOLE / (x / 3 * 2)).is_power_of_two() { return Some(format!("{}.", WHOLE / (x / 3 * 2))) }
        None
    };
    let (mut rem, mut parts) = (len, vec![]);
    while rem > 0 {
        if let Some(s) = single(rem).filter(|_| rem <= WHOLE) { parts.push(s); break }
        let Some(p) = [1920u32, 1440, 960, 720, 480, 360, 240, 180, 120, 90, 60, 30].into_iter().find(|&p| p <= rem) else { parts.push(lens.text(rem, 0)); break };
        parts.push(single(p).unwrap());
        rem -= p;
    }
    parts.join("^")
}

const FLAT: [&str; 12] = ["c", "d-", "d", "e-", "e", "f", "g-", "g", "a-", "a", "b-", "b"];

/// Spell black keys as flats: when the notes fit a flat key better than a sharp one.
fn flats<'a>(tracks: impl Iterator<Item = &'a [Tok]>) -> bool {
    let mut n = [0usize; 12];
    for t in tracks { for tok in t { if let Tok::Note { key, .. } = tok { n[*key as usize % 12] += 1 } } }
    // Notes outside the major scale of each key; the best sharp key against the best flat one.
    let off = |root: usize| (0..12).filter(|i| ![0, 2, 4, 5, 7, 9, 11].contains(&((i + 12 - root) % 12))).map(|i| n[i]).sum::<usize>();
    let sharp = [0, 7, 2, 9, 4, 11].into_iter().map(off).min().unwrap();
    let flat = [5, 10, 3, 8, 1, 6].into_iter().map(off).min().unwrap();
    flat < sharp
}

struct Writer<'a> { lens: &'a Lengths, l: u32, names: &'a [&'a str; 12] }

impl Writer<'_> {
    /// One section as text: restated state, then the notes in beat groups, repeated figures as loops.
    fn piece(&self, toks: &[Tok], st: &mut State, restate: &[&'static str]) -> String {
        // Commands before the first note or rest belong to the state restated at the start.
        let first = toks.iter().position(|t| !matches!(t, Tok::Cmd(_))).unwrap_or(toks.len());
        let mut head = String::new();
        let mut others = String::new();
        // (The tone is never restated: `%` resets the channel, so it's written only where it changes.)
        let entry = st.clone();
        for t in &toks[..first] {
            let Tok::Cmd(c) = t else { unreachable!() };
            match kind(c) { Some(k) => st.set(k, c), None => others += c }
        }
        let sounds = toks.iter().any(|t| matches!(t, Tok::Note { .. }));
        let reset = toks[..first].iter().any(|t| matches!(t, Tok::Cmd(x) if x.starts_with('%')));
        // (A tone with an LFO rate, or the rate alone, resets the LFO's depths: written again after it.)
        let mut lfo_reset = false;
        for k in KINDS {
            let Some(c) = st.0.get(k) else { continue };
            let changed = entry.0.get(k) != Some(c);
            if TONE.contains(&k) {
                if changed || reset || (lfo_reset && matches!(k, "mp" | "ma")) {
                    head += c;
                    lfo_reset |= k == "lfo" || (k == "env" && c.contains("@lfo"));
                }
            }
            // (A line of rests only needn't restate anything: the next line does.)
            else if (restate.contains(&k) && sounds) || changed { head += c }
        }
        head += &others;
        // Beat groups: (start tick within the piece, text), and the octave before and after each.
        let (mut groups, mut pend, mut t, mut oct): (Vec<(u64, String)>, String, u64, Option<usize>) = (vec![], String::new(), 0, None);
        let mut octs: Vec<(Option<usize>, Option<usize>)> = vec![];
        for tok in &toks[first..] {
            match tok {
                Tok::Cmd(c) => match kind(c) {
                    Some(k) => {
                        if st.0.get(k).map(String::as_str) != Some(c.as_str()) || k == "instr" { pend += c }
                        st.set(k, c);
                    }
                    None => pend += c,
                },
                Tok::Note { key, len, slur } => {
                    let o = *key as usize / 12;
                    octs.push((oct, Some(o)));
                    match oct {
                        None => pend += &format!("o{o}"),
                        Some(p) if p + 1 == o => pend.push('<'),
                        Some(p) if o + 1 == p => pend.push('>'),
                        Some(p) if p != o => pend += &format!("o{o}"),
                        _ => {}
                    }
                    oct = Some(o);
                    pend += self.names[*key as usize % 12];
                    pend += &len_text(*len, self.l, self.lens);
                    if *slur { pend.push('&') }
                    groups.push((t, std::mem::take(&mut pend)));
                    t += *len as u64;
                }
                Tok::Rest(len) => {
                    octs.push((oct, oct));
                    pend += &format!("r{}", len_text(*len, self.l, self.lens));
                    groups.push((t, std::mem::take(&mut pend)));
                    t += *len as u64;
                }
            }
        }
        if !pend.is_empty() { groups.push((t, pend)); octs.push((oct, oct)) }
        let body = join(&fold(&groups, &octs));
        match (head.is_empty(), body.is_empty()) { (true, _) => body, (_, true) => head, _ => format!("{head} {body}") }
    }
}

/// Runs of a repeated figure (up to 8 groups) as `[figure]n`, where that saves enough to be worth reading.
/// `octs`: each group's octave before and after it.
fn fold(groups: &[(u64, String)], octs: &[(Option<usize>, Option<usize>)]) -> Vec<(u64, String)> {
    let (mut out, mut i) = (vec![], 0);
    while i < groups.len() {
        let mut best: Option<(usize, usize, usize)> = None; // period, count, saving
        for p in 1..=8.min((groups.len() - i) / 2) {
            let mut k = 1;
            while i + (k + 1) * p <= groups.len() && groups[i..i + p].iter().zip(&groups[i + k * p..i + (k + 1) * p]).all(|(a, b)| a.1 == b.1) { k += 1 }
            if k < 2 { continue }
            // (SiON plays every repeat at the octave the loop starts at, so a figure that ends at another octave
            // than it starts, which its text is the same but an octave on each time, can't be a loop.)
            if octs[i].0 != octs[i + p - 1].1 { continue }
            let unit: usize = groups[i..i + p].iter().map(|g| g.1.len()).sum();
            let sav = (k - 1) * unit;
            let cost = 2 + k.to_string().len();
            if sav >= cost + 4 && best.is_none_or(|b| sav - cost > b.2) { best = Some((p, k, sav - cost)) }
        }
        match best {
            Some((p, k, _)) => {
                let inner = groups[i..i + p].iter().map(|g| g.1.as_str()).collect::<String>();
                out.push((groups[i].0, format!("[{inner}]{k}")));
                i += p * k;
            }
            None => { out.push(groups[i].clone()); i += 1 }
        }
    }
    out
}

/// Groups joined, with a space before each that starts on a beat.
fn join(groups: &[(u64, String)]) -> String {
    let mut s = String::new();
    for (j, (t, g)) in groups.iter().enumerate() {
        if j > 0 && t % BEAT == 0 { s.push(' ') }
        s += g;
    }
    s
}

/// A track's sections as (loop or single section) items: `[a b|c]n` = a b c a b.
enum Item { One(usize), Loop { body: Vec<usize>, brk: Option<usize>, n: usize } }

/// Repeats of section sequences (by text id) as loops, longest coverage first, never across the loop point.
fn structure(ids: &[usize], lp: Option<usize>) -> Vec<Item> {
    let (mut out, mut i) = (vec![], 0);
    while i < ids.len() {
        let limit = match lp { Some(l) if l > i => l, _ => ids.len() };
        let mut best: Option<(usize, usize, usize, usize)> = None; // period, count, break, covered
        for p in 1..=16.min((limit - i) / 2) {
            let mut k = 1;
            while i + (k + 1) * p <= limit && ids[i..i + p] == ids[i + k * p..i + (k + 1) * p] { k += 1 }
            if k < 2 { continue }
            // The run may go on partway into one more repeat: `[a|b]k+1`.
            let mut m = 0;
            while m < p - 1 && i + k * p + m < limit && ids[i + m] == ids[i + k * p + m] { m += 1 }
            let cov = k * p + m;
            if best.is_none_or(|b| cov - p > b.3 - b.0) { best = Some((p, k, m, cov)) }
        }
        match best {
            Some((p, k, m, cov)) if cov > p => {
                let body = ids[i..i + p].to_vec();
                if m > 0 { out.push(Item::Loop { body, brk: Some(m), n: k + 1 }) } else { out.push(Item::Loop { body, brk: None, n: k }) }
                i += cov;
            }
            _ => { out.push(Item::One(ids[i])); i += 1 }
        }
    }
    out
}

/// Readable MML for these tracks: `tracks` as (comment, tokens), cut at `sections` (ticks), looping to `lp`.
/// `vmax`: the loudest `v`, a track's volume before it sets one.
pub fn write(title: &str, header: &[String], tracks: &[(String, &[Tok])], sections: &[u64], lp: Option<u64>, vmax: u16, lens: &mut Lengths) -> String {
    let mut out = String::new();
    if !title.is_empty() { out += &format!("#TITLE{{{}}};\n", title.replace(['{', '}', ';'], "")) }
    let is_data = |h: &str| h.starts_with("#WAV") || h.starts_with("#PCMB");
    for h in header.iter().filter(|h| !is_data(h)) { out += h; out += ";\n" }
    let mut bounds: Vec<u64> = sections.iter().copied().chain(lp).filter(|&t| t > 0).collect();
    bounds.sort_unstable();
    bounds.dedup();
    let names = if flats(tracks.iter().map(|t| t.1)) { &FLAT } else { &crate::mml::NAMES };
    for (label, toks) in tracks {
        // Commands after the last note restore the state for the loop. They stay: a line restates its state only
        // where it has notes, and the last note's release still sounds as the loop starts again, under them.
        // (Without a loop there is nothing to restore: the tone only.)
        let last = toks.iter().rposition(|t| !matches!(t, Tok::Cmd(_))).map_or(0, |i| i + 1);
        let looped = toks.iter().any(|t| matches!(t, Tok::Cmd(c) if c == "$"));
        let toks: Vec<Tok> = toks.iter().enumerate().filter(|(i, t)| *i < last || looped || matches!(t, Tok::Cmd(c) if kind(c).is_some_and(|k| TONE.contains(&k)))).map(|x| x.1.clone()).collect();
        let toks = &toks[..];
        // Default length: the most common note or rest length.
        let mut freq: HashMap<u32, usize> = HashMap::new();
        for t in toks { if let Tok::Note { len, .. } | Tok::Rest(len) = t { *freq.entry(*len).or_default() += 1 } }
        let l = freq.iter().filter(|(&len, _)| len > 0 && WHOLE % len == 0).max_by_key(|(&len, &c)| (c, len)).map_or(480, |x| *x.0);
        lens.prepare(l);
        // State kinds that are set once, before the first note, go on the track's first line; the others are
        // restated on every line.
        let first_note = toks.iter().position(|t| matches!(t, Tok::Note { .. })).unwrap_or(toks.len());
        let mut vals: HashMap<&'static str, (Vec<&str>, usize)> = HashMap::new();
        for (i, t) in toks.iter().enumerate() {
            if let Tok::Cmd(c) = t { if let Some(k) = kind(c) {
                let e = vals.entry(k).or_insert((vec![], i));
                if !e.0.contains(&c.as_str()) { e.0.push(c) }
            } }
        }
        // (A module change resets the tone: the tone is restated whenever the module is.)
        let restate: Vec<&'static str> = vals.iter().filter(|(k, (v, at))| !TONE.contains(k) && (v.len() > 1 || *at > first_note)).map(|(k, _)| *k).collect();
        // (The tone, when it changes, goes on the line it changes on; a constant one on the first line.)
        let tone_init: Vec<&'static str> = vals.iter().filter(|(k, (v, at))| TONE.contains(k) && v.len() == 1 && *at < first_note).map(|(k, _)| *k).collect();
        let mut init: Vec<String> = vec![];
        // (Restated state starts at SiON's defaults, so the first line states it too.)
        let mut st = State::default();
        for (k, d) in [("v", format!("v{vmax}")), ("pan", "@p0".into()), ("k", "k0".into()), ("mp", "mp0".into()), ("ma", "ma0".into()), ("ptab", "np".into()), ("vtab", "na".into()), ("ctab", "_na".into()), ("rtab", "_np".into()), ("qa", "@q0".into())] {
            if restate.contains(&k) { st.0.insert(k, d); }
        }
        let mut rest = vec![];
        // (The first line goes before the loop point. A tone set after it is set again on every pass, where `%`
        // resets the channel, cutting a release still sounding at the loop's end: it stays there.)
        // (Nor does a tone command once one before it stays: an `@lfo` resets the `mp` after it.)
        let (mut looped, mut kept) = (false, false);
        for (i, t) in toks.iter().enumerate() {
            looped |= matches!(t, Tok::Cmd(c) if c == "$");
            match t {
                Tok::Cmd(c) if i < first_note && c != "$" => match kind(c) {
                    Some(k) if !restate.contains(&k) && (!TONE.contains(&k) || (tone_init.contains(&k) && !looped && !kept)) => { init.push(c.clone()); st.set(k, c) }
                    Some(k) if TONE.contains(&k) => { kept = true; rest.push(t.clone()) }
                    // (Tempo at the start goes on the first line too.)
                    None if c.starts_with('t') => init.insert(0, c.clone()),
                    _ => rest.push(t.clone()),
                },
                _ => rest.push(t.clone()),
            }
        }
        if l != 480 { init.push(format!("l{}", len_text(l, 0, lens))) }
        // (A tone and its envelope read as one: `%4@1s63`.)
        let init = init.iter().fold(String::new(), |s, c| if s.is_empty() || (c.starts_with('@') && !c.starts_with("@p")) { s + c } else { s + " " + c });
        let (pieces, lpi) = split(&rest, &bounds, &st);
        let w = Writer { lens, l, names };
        // Sections by text and the tone they start with: equal, the same music.
        let keys: Vec<(String, String)> = pieces.iter().map(|p| {
            let tone: String = TONE.iter().filter_map(|k| st.0.get(k).cloned()).collect::<Vec<_>>().join("\0");
            (w.piece(p, &mut st, &restate), tone)
        }).collect();
        let mut uniq_k: Vec<&(String, String)> = vec![];
        let ids: Vec<usize> = keys.iter().map(|t| uniq_k.iter().position(|u| *u == t).unwrap_or_else(|| { uniq_k.push(t); uniq_k.len() - 1 })).collect();
        let uniq: Vec<&str> = uniq_k.iter().map(|k| k.0.as_str()).collect();
        let mut items = vec![];
        let (pre, post) = ids.split_at(lpi.unwrap_or(ids.len()));
        items.extend(structure(pre, None));
        let mark = items.len();
        items.extend(structure(post, None));
        // Sections heard in more than one place (outside a single loop) become macros.
        let mut uses = vec![0usize; uniq.len()];
        for it in &items {
            match it { Item::One(i) => uses[*i] += 1, Item::Loop { body, .. } => for i in body { uses[*i] += 1 } }
        }
        let mut letter: HashMap<usize, char> = HashMap::new();
        let mut defs = String::new();
        for it in &items {
            let ids: Vec<usize> = match it { Item::One(i) => vec![*i], Item::Loop { body, .. } => body.clone() };
            for i in ids {
                if uses[i] > 1 && uniq[i].len() >= 12 && !letter.contains_key(&i) && letter.len() < 26 {
                    let c = (b'A' + letter.len() as u8) as char;
                    letter.insert(i, c);
                    defs += &format!("#{c}={};\n", uniq[i]);
                }
            }
        }
        let txt = |i: usize| letter.get(&i).map_or(uniq[i].to_string(), |c| c.to_string());
        out += &format!("\n// {label}\n{defs}");
        let mut lines = vec![];
        if !init.is_empty() { lines.push(init) }
        for (j, it) in items.iter().enumerate() {
            if j == mark && lpi.is_some() { lines.push("$".into()) }
            match it {
                Item::One(i) => lines.push(txt(*i)),
                Item::Loop { body, brk, n } => {
                    let parts: Vec<String> = body.iter().enumerate().map(|(k, &i)| format!("{}{}", if *brk == Some(k) { "| " } else { "" }, txt(i))).collect();
                    let one = parts.join(" ");
                    if one.len() <= 72 { lines.push(format!("[{one}]{}", if *n == 2 { String::new() } else { n.to_string() })) } else {
                        lines.push(format!("[{}", parts[0]));
                        for p in &parts[1..] { lines.push(format!(" {p}")) }
                        lines.push(format!("]{}", if *n == 2 { String::new() } else { n.to_string() }));
                    }
                }
            }
        }
        if lpi.is_some() && mark == items.len() { lines.push("$".into()) }
        out += &lines.join("\n");
        out += ";\n";
    }
    let data: Vec<&String> = header.iter().filter(|h| is_data(h)).collect();
    if !data.is_empty() {
        out += "\n// Waveforms and samples\n";
        for h in data { out += h; out += ";\n" }
    }
    out
}
