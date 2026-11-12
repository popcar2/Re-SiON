//! MML text output. `plain` writes tokens one by one; `minify` writes the same performance as short as it
//! can: default lengths, relative octaves, key signatures, `[...]` loops and `#A=...;` macros.
//!
//! Both rely on how SiON reads MML: octave and default length are resolved in text order (so loops and macros
//! replay absolute notes), lengths count 1920 ticks per whole note, `n` means `1920/n` ticks, each dot adds
//! half of the previous part, and `^` adds further parts.

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tok {
    /// `slur`: followed by `&` (the next note continues without a new attack).
    Note { key: u8, len: u32, slur: bool },
    Rest(u32),
    /// Any other command, written as is.
    Cmd(String),
}

const WHOLE: u32 = 1920;
const MAXDP: usize = WHOLE as usize * 8;
/// Default length (`l4`).
const DEF_L: u32 = 480;

/// Shortest text for every tick count: sums of length terms (`8`, `4.`, `3`...) joined by `^`.
pub struct Lengths {
    terms: Vec<(u32, String)>,
    f0: Vec<u32>,
    c0: Vec<u16>,
    /// Per default length `l`: costs and choices that may also use the empty term (`l`) and its dotted forms.
    by_l: HashMap<u32, (Vec<u32>, Vec<u16>, Vec<(u32, String)>)>,
}

impl Lengths {
    pub fn new() -> Lengths {
        let mut best: HashMap<u32, String> = HashMap::new();
        for n in 1..=WHOLE {
            let (base, mut s) = (WHOLE / n, n.to_string());
            let (mut d, mut h) = (base, base);
            loop {
                let e = best.entry(d).or_insert_with(|| s.clone());
                if s.len() < e.len() { *e = s.clone() }
                h /= 2;
                if h == 0 { break }
                d += h;
                s.push('.');
            }
        }
        let mut terms: Vec<_> = best.into_iter().collect();
        terms.sort();
        // f[t] = least total of (term length + 1) over terms summing to t; the text costs f[t] - 1.
        let (mut f0, mut c0) = (vec![u32::MAX; MAXDP + 1], vec![0u16; MAXDP + 1]);
        f0[0] = 0;
        for t in 1..=MAXDP {
            for (i, (v, s)) in terms.iter().enumerate() {
                let v = *v as usize;
                if v > t { break }
                let c = f0[t - v].saturating_add(s.len() as u32 + 1);
                if c < f0[t] { (f0[t], c0[t]) = (c, i as u16) }
            }
        }
        Lengths { terms, f0, c0, by_l: HashMap::new() }
    }

    /// Make default length `l` available to `cost`/`text`.
    pub fn prepare(&mut self, l: u32) {
        if l == 0 || l as usize > MAXDP || self.by_l.contains_key(&l) { return }
        let (mut lt, mut s, mut d, mut h) = (vec![], String::new(), l, l);
        loop {
            lt.push((d, s.clone()));
            h /= 2;
            if h == 0 { break }
            d += h;
            s.push('.');
        }
        let (mut f, mut c) = (self.f0.clone(), vec![u16::MAX; MAXDP + 1]);
        for t in 1..=MAXDP {
            for (j, (v, s)) in lt.iter().enumerate() {
                let v = *v as usize;
                if v > t { continue }
                let x = f[t - v].saturating_add(s.len() as u32 + 1);
                if x < f[t] { (f[t], c[t]) = (x, j as u16) }
            }
        }
        self.by_l.insert(l, (f, c, lt));
    }

    fn split(len: u32) -> (usize, usize) {
        if len as usize <= MAXDP { return (len as usize, 0) }
        let k = (len as usize - MAXDP).div_ceil(WHOLE as usize);
        (len as usize - k * WHOLE as usize, k)
    }

    /// Characters needed to write `len` ticks when the default length is `l` (0: none usable).
    pub fn cost(&self, len: u32, l: u32) -> u32 {
        let (t, k) = Self::split(len);
        let f = match self.by_l.get(&l) { Some(x) => x.0[t], None => self.f0[t] };
        f - 1 + 2 * k as u32
    }

    pub fn text(&self, len: u32, l: u32) -> String {
        let (mut t, k) = Self::split(len);
        let mut parts: Vec<&str> = vec![];
        let by = self.by_l.get(&l);
        while t > 0 {
            match by.map(|b| b.1[t]).filter(|&c| c != u16::MAX) {
                Some(c) => { let (v, s) = &by.unwrap().2[c as usize]; parts.push(s); t -= *v as usize }
                None => { let (v, s) = &self.terms[self.c0[t] as usize]; parts.push(s); t -= *v as usize }
            }
        }
        // Put the empty (default length) term first, so the text never starts with a bare `^`.
        parts.sort_by_key(|s| !s.is_empty());
        let mut out = parts.join("^");
        for _ in 0..k { out.push_str("^1") }
        out
    }
}

const NAMES: [&str; 12] = ["c", "c+", "d", "d+", "e", "f", "f+", "g", "g+", "a", "a+", "b"];

/// One command per token, absolute octave on every note, explicit lengths: the reference the minified
/// output must sound identical to.
pub fn plain(header: &[String], tracks: &[Vec<Tok>], lens: &Lengths) -> String {
    let mut parts: Vec<String> = header.to_vec();
    for t in tracks {
        let mut s = String::new();
        for tok in t {
            match tok {
                Tok::Note { key, len, slur } => {
                    s += &format!("o{}{}{}", key / 12, NAMES[*key as usize % 12], lens.text(*len, 0));
                    if *slur { s.push('&') }
                }
                Tok::Rest(len) => s += &format!("r{}", lens.text(*len, 0)),
                Tok::Cmd(c) => s += c,
            }
        }
        parts.push(s);
    }
    parts.join(";\n")
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Atom {
    // `l`: the default length in force where a note or rest is written (they only match within one `l`).
    Note { key: u8, len: u32, slur: bool, l: u32 },
    Rest { len: u32, l: u32 },
    Cmd(String),
    /// `[body]n`, with `|` (leave on the last pass) before `body[brk]`.
    Loop { body: Vec<u32>, brk: Option<usize>, n: u32 },
    /// Reference to macro number `i`.
    Mac(usize),
}

/// Interned atoms with their estimated text cost and (for notes and rests) the length text.
#[derive(Default)]
struct Pool {
    atoms: Vec<Atom>, ids: HashMap<Atom, u32>, cost: Vec<i64>, text: Vec<String>,
    /// Contains an `l` command (directly or inside a loop or macro).
    has_l: Vec<bool>,
}

fn count_text(n: u32) -> String { if n == 2 { String::new() } else { n.to_string() } }

/// Rest text: `r` + length, or for long rests a loop such as `[r1]12`.
fn rest_text(lens: &Lengths, len: u32, l: u32) -> String {
    let mut best = format!("r{}", lens.text(len, l));
    for chunk in [WHOLE, WHOLE * 2, l] {
        if chunk == 0 || len / chunk < 3 { continue }
        let (k, rem) = (len / chunk, len % chunk);
        let mut s = format!("[r{}]{}", lens.text(chunk, l), k);
        if rem > 0 { s += &format!("r{}", lens.text(rem, l)) }
        if s.len() < best.len() { best = s }
    }
    best
}

fn black(key: u8) -> i64 { matches!(key % 12, 1 | 3 | 6 | 8 | 10) as i64 }

impl Pool {
    fn add(&mut self, a: Atom, lens: &Lengths) -> u32 {
        if let Some(&id) = self.ids.get(&a) { return id }
        let (cost, text) = match &a {
            Atom::Note { key, len, slur, l } => {
                let s = lens.text(*len, *l) + if *slur { "&" } else { "" };
                (1 + black(*key) + s.len() as i64, s)
            }
            Atom::Rest { len, l } => { let s = rest_text(lens, *len, *l); (s.len() as i64, s) }
            Atom::Cmd(s) => (s.len() as i64, s.clone()),
            Atom::Loop { body, brk, n } => {
                (2 + brk.is_some() as i64 + count_text(*n).len() as i64 + body.iter().map(|&b| self.cost[b as usize]).sum::<i64>(), String::new())
            }
            Atom::Mac(_) => (1, String::new()),
        };
        let has_l = match &a {
            Atom::Cmd(s) => s.starts_with('l'),
            Atom::Loop { body, .. } => body.iter().any(|&b| self.has_l[b as usize]),
            _ => false,
        };
        self.has_l.push(has_l);
        let id = self.atoms.len() as u32;
        self.atoms.push(a.clone());
        self.ids.insert(a, id);
        self.cost.push(cost);
        self.text.push(text);
        id
    }
}

fn dig(n: usize) -> i64 { count_text(n as u32).len() as i64 }

/// Longest period considered for a repeat inside one sequence (longer repeats are left to macros).
const PMAX: usize = 384;

/// Replace runs of a repeated unit with `[unit]n` (or `[a|b]n` when the run ends partway through the unit),
/// biggest saving first, nesting inside the extracted units.
fn loops(pool: &mut Pool, seq: &[u32], lens: &Lengths) -> Vec<u32> {
    let mut s = seq.to_vec();
    loop {
        let n = s.len();
        if n < 2 { break }
        let mut pc = vec![0i64; n + 1];
        let mut hl = vec![0u32; n + 1];
        for i in 0..n { pc[i + 1] = pc[i] + pool.cost[s[i] as usize]; hl[i + 1] = hl[i] + pool.has_l[s[i] as usize] as u32 }
        let mut best = (0i64, 0, 0, 0, 0); // saving, start, period, count, prefix length before `|`
        for p in 1..n.min(PMAX + 1) {
            let mut j = 0;
            while j + p < n {
                if s[j] != s[j + p] { j += 1; continue }
                let a = j;
                while j + p < n && s[j] == s[j + p] { j += 1 }
                let r = j - a;
                let (k, m, cu) = (r / p + 1, r % p, pc[a + p] - pc[a]);
                if k >= 2 {
                    let sav = (k as i64 - 1) * cu - 2 - dig(k);
                    if sav > best.0 { best = (sav, a, p, k, 0) }
                }
                // `[a|b]n` ends at runtime after `a` but in the text after `b`; SiON resolves `l` in text order,
                // so `b` must not change it.
                if m > 0 && hl[a + p] == hl[a + m] {
                    let sav = (k as i64 - 1) * cu + (pc[a + m] - pc[a]) - 3 - dig(k + 1);
                    if sav > best.0 { best = (sav, a, p, k + 1, m) }
                }
            }
        }
        let (sav, a, p, k, m) = best;
        if sav <= 0 { break }
        let unit = s[a..a + p].to_vec();
        let (body, brk, region) = if m > 0 {
            let mut head = loops(pool, &unit[..m], lens);
            let at = head.len();
            head.extend(loops(pool, &unit[m..], lens));
            (head, Some(at), (k - 1) * p + m)
        } else {
            (loops(pool, &unit, lens), None, k * p)
        };
        let id = pool.add(Atom::Loop { body, brk, n: k as u32 }, lens);
        s.splice(a..a + region, [id]);
    }
    s
}

fn suffix_array(s: &[u32]) -> Vec<usize> {
    let n = s.len();
    let mut sa: Vec<usize> = (0..n).collect();
    let mut rank: Vec<u64> = s.iter().map(|&x| x as u64).collect();
    let mut tmp = vec![0u64; n];
    let mut k = 1;
    while n > 1 {
        {
            let r = &rank;
            let key = |i: usize| (r[i], if i + k < n { r[i + k] + 1 } else { 0 });
            sa.sort_unstable_by_key(|&i| key(i));
            tmp[sa[0]] = 0;
            for w in 1..n { tmp[sa[w]] = tmp[sa[w - 1]] + (key(sa[w - 1]) != key(sa[w])) as u64 }
        }
        std::mem::swap(&mut rank, &mut tmp);
        if rank[sa[n - 1]] as usize == n - 1 { break }
        k *= 2;
    }
    sa
}

fn lcp_array(s: &[u32], sa: &[usize]) -> Vec<usize> {
    let n = s.len();
    let mut rank = vec![0; n];
    for (i, &p) in sa.iter().enumerate() { rank[p] = i }
    let (mut lcp, mut h) = (vec![0; n], 0);
    for i in 0..n {
        if rank[i] == 0 { h = 0; continue }
        let j = sa[rank[i] - 1];
        while i + h < n && j + h < n && s[i + h] == s[j + h] { h += 1 }
        lcp[rank[i]] = h;
        h = h.saturating_sub(1);
    }
    lcp
}

/// Greedily turn the repeated atom sequence that saves the most text into a macro, up to 26 (`A`..`Z`).
/// Macro bodies are appended to `seqs` (after the tracks) and are searched too, so macros can nest.
fn macros(pool: &mut Pool, seqs: &mut Vec<Vec<u32>>, ntracks: usize, lens: &Lengths) {
    while seqs.len() - ntracks < 26 {
        let (mut cat, mut starts) = (vec![], vec![]);
        for (i, s) in seqs.iter().enumerate() {
            starts.push(cat.len());
            cat.extend(s);
            cat.push(u32::MAX - i as u32);
        }
        let n = cat.len();
        let sa = suffix_array(&cat);
        let lcp = lcp_array(&cat, &sa);
        let mut pc = vec![0i64; n + 1];
        for i in 0..n { pc[i + 1] = pc[i] + pool.cost.get(cat[i] as usize).copied().unwrap_or(0) }
        let mut best: (i64, usize, Vec<usize>) = (0, 0, vec![]);
        let mut consider = |h: usize, lb: usize, rb: usize| {
            let (cnt, p0) = ((rb - lb + 1) as i64, sa[lb]);
            let c = pc[p0 + h] - pc[p0];
            if (cnt - 1) * c - cnt - 4 <= best.0 { return }
            let mut ps = sa[lb..=rb].to_vec();
            ps.sort_unstable();
            let (mut occ, mut next) = (vec![], 0);
            for p in ps { if p >= next { occ.push(p); next = p + h } }
            let k = occ.len() as i64;
            let sav = (k - 1) * c - k - 4;
            if sav > best.0 { best = (sav, h, occ) }
        };
        let mut stack = vec![(0usize, 0usize)];
        for i in 1..=n {
            let l = if i < n { lcp[i] } else { 0 };
            let mut lb = i - 1;
            while l < stack.last().unwrap().0 {
                let (h, b) = stack.pop().unwrap();
                consider(h, b, i - 1);
                lb = b;
            }
            if l > stack.last().unwrap().0 { stack.push((l, lb)) }
        }
        let (sav, h, occ) = best;
        if sav <= 0 { break }
        let body = cat[occ[0]..occ[0] + h].to_vec();
        let id = pool.add(Atom::Mac(seqs.len() - ntracks), lens);
        pool.has_l[id as usize] = body.iter().any(|&b| pool.has_l[b as usize]);
        for &p in occ.iter().rev() {
            let si = starts.partition_point(|&st| st <= p) - 1;
            let off = p - starts[si];
            seqs[si].splice(off..off + h, [id]);
        }
        seqs.push(body);
    }
}

/// Inline macros that no longer pay for their definition (e.g. used once after a bigger macro took over).
fn prune(pool: &Pool, seqs: &mut [Vec<u32>], ntracks: usize) -> Vec<bool> {
    let nm = seqs.len() - ntracks;
    let mut live = vec![true; nm];
    let mac_of = |id: u32| match pool.atoms[id as usize] { Atom::Mac(i) => Some(i), _ => None };
    loop {
        let mut refs = vec![0i64; nm];
        for (si, s) in seqs.iter().enumerate() {
            if si >= ntracks && !live[si - ntracks] { continue }
            for &a in s { if let Some(i) = mac_of(a) { refs[i] += 1 } }
        }
        let cost = |i: usize| seqs[ntracks + i].iter().map(|&a| pool.cost[a as usize]).sum::<i64>();
        let Some(dead) = (0..nm).find(|&i| live[i] && (refs[i] - 1) * cost(i) <= refs[i] + 4) else { break };
        live[dead] = false;
        let body = seqs[ntracks + dead].clone();
        for s in seqs.iter_mut() {
            if s.iter().any(|&a| mac_of(a) == Some(dead)) {
                *s = s.iter().flat_map(|&a| if mac_of(a) == Some(dead) { body.clone() } else { vec![a] }).collect();
            }
        }
    }
    live
}

/// Key signatures for `#SIGN`: name, number of sharps (+) or flats (-).
const SIGNS: [(&str, i32); 15] = [
    ("", 0), ("G", 1), ("D", 2), ("A", 3), ("E", 4), ("B", 5), ("F+", 6), ("C+", 7),
    ("F", -1), ("B-", -2), ("E-", -3), ("A-", -4), ("D-", -5), ("G-", -6), ("C-", -7),
];

fn sign_shift(n: i32) -> [i32; 7] {
    let mut k = [0; 7];
    let order = if n > 0 { "fcgdaeb" } else { "beadgcf" };
    for c in order.bytes().take(n.unsigned_abs() as usize) { k[(c - b'a') as usize] = n.signum() }
    k
}

/// Ways to write note `key`: (octave, letter + accidental), shortest first, one per octave.
fn spellings(key: u8, sign: &[i32; 7]) -> Vec<(usize, String)> {
    const BASE: [i32; 7] = [9, 11, 0, 2, 4, 5, 7];
    let mut v: Vec<(usize, String)> = vec![];
    for l in 0..7 {
        for (acc, s) in [(0, ""), (1, "+"), (-1, "-")] {
            let d = key as i32 - (BASE[l] + sign[l] + acc);
            if d.rem_euclid(12) == 0 && (0..=10).contains(&(d / 12)) {
                v.push(((d / 12) as usize, format!("{}{}", (b'a' + l as u8) as char, s)));
            }
        }
    }
    v.sort_by_key(|x| x.1.len());
    let mut seen = [false; 11];
    v.retain(|x| !std::mem::replace(&mut seen[x.0], true));
    v
}

fn trans(a: usize, b: usize) -> String {
    let d = b as i32 - a as i32;
    match d {
        0 => String::new(),
        1 => "<".into(),
        -1 => ">".into(),
        2 => "<<".into(),
        -2 => ">>".into(),
        _ => format!("o{b}"),
    }
}

enum Item<'a> {
    Fixed(&'a str),
    Note { sp: Vec<(usize, String)>, suffix: &'a str },
    /// Macro letter with the octave it expects on entry and leaves on exit (None: no notes inside).
    Mac { io: Option<(usize, usize)>, c: char },
    /// The loop point (`$`): the octave there is whatever the end of the track left, so it's set (`o`) again.
    Loop,
}

/// Pick octave commands and note spellings with the fewest characters (Viterbi over the octave state).
/// `start`: the octave on entry, or None for a macro body, whose entry octave is then chosen freely.
fn write(items: &[Item], start: Option<usize>) -> (String, Option<(usize, usize)>) {
    const NO: usize = usize::MAX / 4;
    let tc: Vec<Vec<usize>> = (0..11).map(|a| (0..11).map(|b| trans(a, b).len()).collect()).collect();
    let mut cost = [NO; 11];
    match start { Some(s) => cost[s] = 0, None => cost = [0; 11] }
    let mut back: Vec<[(u8, u8); 11]> = Vec::with_capacity(items.len());
    for it in items {
        let (mut nc, mut bk) = ([NO; 11], [(0u8, 0u8); 11]);
        let keep = |add: usize, nc: &mut [usize; 11], bk: &mut [(u8, u8); 11]| for s in 0..11 { if cost[s] < NO { nc[s] = cost[s] + add; bk[s] = (s as u8, 0) } };
        match it {
            Item::Fixed(s) => keep(s.len(), &mut nc, &mut bk),
            Item::Loop => {
                let s = (0..11).min_by_key(|&s| cost[s]).unwrap();
                for o in 0..11 { if cost[s] < NO { nc[o] = cost[s] + 1 + format!("o{o}").len(); bk[o] = (s as u8, 0) } }
            }
            Item::Mac { io: None, .. } => keep(1, &mut nc, &mut bk),
            Item::Mac { io: Some((e, x)), .. } => {
                for s in 0..11 {
                    let c = cost[s] + tc[s][*e] + 1;
                    if cost[s] < NO && c < nc[*x] { nc[*x] = c; bk[*x] = (s as u8, 0) }
                }
            }
            Item::Note { sp, suffix } => {
                for (ci, (o, t)) in sp.iter().enumerate() {
                    for s in 0..11 {
                        let c = cost[s] + tc[s][*o] + t.len() + suffix.len();
                        if cost[s] < NO && c < nc[*o] { nc[*o] = c; bk[*o] = (s as u8, ci as u8) }
                    }
                }
            }
        }
        cost = nc;
        back.push(bk);
    }
    let mut st = (0..11).min_by_key(|&s| cost[s]).unwrap();
    let (mut pick, mut after) = (vec![0u8; items.len()], vec![0usize; items.len()]);
    for i in (0..items.len()).rev() { let (p, ci) = back[i][st]; (pick[i], after[i]) = (ci, st); st = p as usize }
    let (entry, mut cur, mut any, mut out) = (st, st, false, String::new());
    for (i, it) in items.iter().enumerate() {
        match it {
            Item::Fixed(s) => out += s,
            Item::Loop => { out += &format!("$o{}", after[i]); (cur, any) = (after[i], true) }
            Item::Mac { io: None, c } => out.push(*c),
            Item::Mac { io: Some((e, x)), c } => { out += &trans(cur, *e); out.push(*c); (cur, any) = (*x, true) }
            Item::Note { sp, suffix } => {
                let (o, t) = &sp[pick[i] as usize];
                out += &trans(cur, *o);
                out += t;
                out += suffix;
                (cur, any) = (*o, true);
            }
        }
    }
    (out, any.then_some((entry, cur)))
}

struct Render<'a> { pool: &'a Pool, sign: [i32; 7], io: Vec<Option<(usize, usize)>>, letter: Vec<char>, brackets: &'a [String] }

impl<'a> Render<'a> {
    fn items(&self, seq: &[u32], out: &mut Vec<Item<'a>>) {
        for &id in seq {
            let pool = self.pool;
            match &pool.atoms[id as usize] {
                Atom::Note { key, .. } => out.push(Item::Note { sp: spellings(*key, &self.sign), suffix: &pool.text[id as usize] }),
                Atom::Cmd(c) if c == "$" => out.push(Item::Loop),
                Atom::Rest { .. } | Atom::Cmd(_) => out.push(Item::Fixed(&pool.text[id as usize])),
                Atom::Loop { body, brk, n } => {
                    out.push(Item::Fixed("["));
                    for (j, &b) in body.iter().enumerate() {
                        if *brk == Some(j) { out.push(Item::Fixed("|")) }
                        self.items(&[b], out);
                    }
                    let b: &'a [String] = self.brackets;
                    out.push(Item::Fixed(&b[(*n as usize).min(b.len() - 1)]));
                }
                Atom::Mac(i) => out.push(Item::Mac { io: self.io[*i], c: self.letter[*i] }),
            }
        }
    }
}

fn macro_deps(pool: &Pool, seq: &[u32], out: &mut Vec<usize>) {
    for &id in seq {
        match &pool.atoms[id as usize] {
            Atom::Mac(i) => out.push(*i),
            Atom::Loop { body, .. } => macro_deps(pool, body, out),
            _ => {}
        }
    }
}

fn render(pool: &Pool, header: &[String], seqs: &[Vec<u32>], ntracks: usize, sign: usize) -> String {
    // Macros in dependency order (a macro must be defined before one that uses it).
    let nm = seqs.len() - ntracks;
    let (mut order, mut state) = (vec![], vec![0u8; nm]);
    fn visit(i: usize, pool: &Pool, seqs: &[Vec<u32>], nt: usize, state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 { return }
        state[i] = 1;
        let mut deps = vec![];
        macro_deps(pool, &seqs[nt + i], &mut deps);
        for d in deps { visit(d, pool, seqs, nt, state, order) }
        order.push(i);
    }
    for t in &seqs[..ntracks] {
        let mut deps = vec![];
        macro_deps(pool, t, &mut deps);
        for d in deps { visit(d, pool, seqs, ntracks, &mut state, &mut order) }
    }
    let max_n = pool.atoms.iter().filter_map(|a| if let Atom::Loop { n, .. } = a { Some(*n as usize) } else { None }).max().unwrap_or(2);
    let brackets: Vec<String> = (0..=max_n).map(|n| format!("]{}", count_text(n as u32))).collect();
    let mut r = Render { pool, sign: sign_shift(SIGNS[sign].1), io: vec![None; nm], letter: vec!['?'; nm], brackets: &brackets };
    let mut parts: Vec<String> = header.to_vec();
    if sign > 0 { parts.push(format!("#SIGN{{{}}}", SIGNS[sign].0)) }
    for (k, &i) in order.iter().enumerate() {
        r.letter[i] = (b'A' + k as u8) as char;
        let mut items = vec![];
        r.items(&seqs[ntracks + i], &mut items);
        let (text, io) = write(&items, None);
        r.io[i] = io;
        parts.push(format!("#{}={}", r.letter[i], text));
    }
    for t in &seqs[..ntracks] {
        let mut items = vec![];
        r.items(t, &mut items);
        parts.push(write(&items, Some(5)).0);
    }
    parts.join(";")
}

/// Default lengths (`l`) through a track, by Viterbi over the candidate lengths: returns the tokens with
/// `l` commands inserted and the default length in force at each token. `penalty` is added per change on top
/// of its text, which keeps sections uniform (better for repeats).
fn assign_l(toks: &[Tok], lens: &mut Lengths, penalty: u32) -> (Vec<Tok>, Vec<u32>) {
    let mut freq: HashMap<u32, usize> = HashMap::new();
    for t in toks { if let Tok::Note { len, .. } | Tok::Rest(len) = t { *freq.entry(*len).or_default() += 1 } }
    let mut cands: Vec<(usize, u32)> = freq.iter().filter(|(&l, _)| l <= 2 * WHOLE).map(|(&l, &c)| (c, l)).collect();
    cands.sort_unstable_by(|a, b| b.cmp(a));
    let mut cands: Vec<u32> = cands.into_iter().take(12).map(|x| x.1).collect();
    if !cands.contains(&DEF_L) { cands.push(DEF_L) }
    for &c in &cands { lens.prepare(c) }
    let k = cands.len();
    let switch: Vec<u32> = cands.iter().map(|&c| 1 + lens.cost(c, 0) + penalty).collect();
    const INF: u32 = u32::MAX / 4;
    let mut cost: Vec<u32> = cands.iter().map(|&c| if c == DEF_L { 0 } else { switch[cands.iter().position(|&x| x == c).unwrap()] }).collect();
    let mut back: Vec<Vec<u8>> = vec![];
    let lt: Vec<usize> = (0..toks.len()).filter(|&i| matches!(toks[i], Tok::Note { .. } | Tok::Rest(_))).collect();
    // After the loop point (`$`) the default length is whatever the end of the track left: the first length
    // there gets an `l` of its own.
    let reset = toks.iter().position(|t| matches!(t, Tok::Cmd(c) if c == "$")).and_then(|d| lt.iter().position(|&i| i > d));
    for (x, &i) in lt.iter().enumerate() {
        let (Tok::Note { len, .. } | Tok::Rest(len)) = &toks[i] else { unreachable!() };
        let (bi, bc) = (0..k).map(|j| (j, cost[j])).min_by_key(|x| x.1).unwrap();
        let mut nb = vec![0u8; k];
        let mut nc = vec![INF; k];
        for j in 0..k {
            let (from, c) = if bc + switch[j] < cost[j] || reset == Some(x) { (bi, bc + switch[j]) } else { (j, cost[j]) };
            nc[j] = c + lens.cost(*len, cands[j]);
            nb[j] = from as u8;
        }
        cost = nc;
        back.push(nb);
    }
    let mut st = (0..k).min_by_key(|&j| cost[j]).unwrap();
    let mut pick = vec![0; lt.len()];
    for x in (0..lt.len()).rev() { pick[x] = st; st = back[x][st] as usize }
    // `st` is now the state before the first length: the start of the track.
    let (mut out, mut ls, mut cur, mut li) = (vec![], vec![], DEF_L, 0);
    for (i, t) in toks.iter().enumerate() {
        let forced = li < lt.len() && lt[li] == i && reset == Some(li);
        let want = if li < lt.len() && lt[li] == i { let w = cands[pick[li]]; li += 1; Some(w) } else { None };
        if let Some(w) = want.filter(|&w| w != cur || forced) {
            out.push(Tok::Cmd(format!("l{}", lens.text(w, 0))));
            ls.push(cur);
            cur = w;
        }
        out.push(t.clone());
        ls.push(cur);
    }
    (out, ls)
}

/// The shortest MML found for these tracks.
pub fn minify(header: &[String], tracks: &[Vec<Tok>], lens: &mut Lengths) -> String {
    let mut best: Option<String> = None;
    for penalty in [0, 3, 8, 1000] {
        let s = minify_with(header, tracks, lens, penalty);
        if best.as_ref().is_none_or(|b| s.len() < b.len()) { best = Some(s) }
    }
    best.unwrap_or_default()
}

fn minify_with(header: &[String], tracks: &[Vec<Tok>], lens: &mut Lengths, penalty: u32) -> String {
    let tracks: Vec<(Vec<Tok>, Vec<u32>)> = tracks.iter().map(|t| assign_l(t, lens, penalty)).collect();
    let lens = &*lens;
    let mut pool = Pool::default();
    let raw: Vec<Vec<u32>> = tracks.iter().map(|(t, ls)| t.iter().zip(ls).map(|(tok, &l)| {
        let a = match tok {
            Tok::Note { key, len, slur } => Atom::Note { key: *key, len: *len, slur: *slur, l },
            Tok::Rest(len) => Atom::Rest { len: *len, l },
            Tok::Cmd(s) => Atom::Cmd(s.clone()),
        };
        pool.add(a, lens)
    }).collect()).collect();
    let nt = raw.len();
    let looped: Vec<Vec<u32>> = raw.iter().map(|s| loops(&mut pool, s, lens)).collect();

    let mut with_macros = looped.clone();
    macros(&mut pool, &mut with_macros, nt, lens);
    let live = prune(&pool, &mut with_macros, nt);
    // Drop dead macro bodies, renumbering the live ones.
    let (mut map, mut kept) = (HashMap::new(), with_macros[..nt].to_vec());
    for (i, &l) in live.iter().enumerate() {
        if !l { continue }
        let new = pool.add(Atom::Mac(kept.len() - nt), lens);
        pool.has_l[new as usize] = with_macros[nt + i].iter().any(|&b| pool.has_l[b as usize]);
        map.insert(pool.ids[&Atom::Mac(i)], new);
        kept.push(with_macros[nt + i].clone());
    }
    let kept: Vec<Vec<u32>> = kept.into_iter().map(|s| s.into_iter().map(|a| *map.get(&a).unwrap_or(&a)).collect()).collect();
    let kept: Vec<Vec<u32>> = kept.iter().map(|s| loops(&mut pool, s, lens)).collect();

    let mut best: Option<String> = None;
    for seqs in [&looped, &kept] {
        for sign in 0..SIGNS.len() {
            let s = render(&pool, header, seqs, nt, sign);
            if best.as_ref().is_none_or(|b| s.len() < b.len()) { best = Some(s) }
        }
    }
    best.unwrap_or_default()
}
