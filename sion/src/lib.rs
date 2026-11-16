//! Player for SiON MML: `Song::new(mml)`, then `render` interleaved stereo f32 samples at `RATE` Hz.
//!
//! PCM samples (`%7`, `%10`) come from `#PCMB` definitions in the MML or from `Song::set_sample`; `#PCMWAVE`,
//! `#PCMVOICE` and `#SAMPLER` build instruments and drum kits out of them.

#![no_std]
// The wasm and C interfaces (`wasm` and `ffi` features) are the only unsafe code.
#![cfg_attr(not(any(feature = "wasm", feature = "ffi")), forbid(unsafe_code))]
#![cfg_attr(any(feature = "wasm", feature = "ffi"), deny(unsafe_code))]
extern crate alloc;

mod fx;
mod m;
mod synth;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[allow(unsafe_code)]
mod wasm;
#[cfg(all(feature = "ffi", not(target_arch = "wasm32")))]
#[allow(unsafe_code)]
mod ffi;

use alloc::{sync::Arc, vec, vec::Vec};
use core::f32::consts::{PI, TAU};
use fx::Fx;
use m::M;
pub use synth::{Sample, PIPE_DEPTH, RATE};
use synth::*;

const INF: f64 = f64::INFINITY;

/// Index just past the next `pat` at or after `i`.
fn find(s: &[u8], i: usize, pat: &[u8]) -> usize {
    let i = i.min(s.len());
    s[i..].windows(pat.len()).position(|w| w == pat).map_or(s.len(), |p| i + p + pat.len())
}

/// Whether `pat` occurs in `s`.
fn has(s: &[u8], pat: &[u8]) -> bool { s.windows(pat.len()).any(|w| w == pat) }

fn ws(s: &[u8], i: &mut usize) { while s.get(*i).is_some_and(u8::is_ascii_whitespace) { *i += 1 } }

fn fnum(s: &[u8], i: &mut usize) -> Option<f32> {
    ws(s, i);
    let st = *i;
    if s.get(*i) == Some(&b'-') { *i += 1 }
    let (mut v, mut sc, mut any) = (0f32, 0f32, false);
    while let Some(&c) = s.get(*i) {
        match c {
            b'0'..=b'9' => { any = true; if sc > 0.0 { sc *= 0.1; v += (c - b'0') as f32 * sc } else { v = v * 10.0 + (c - b'0') as f32 } }
            b'.' if sc == 0.0 => sc = 1.0,
            _ => break,
        }
        *i += 1
    }
    any.then(|| if s[st] == b'-' { -v } else { v })
}

fn num(s: &[u8], i: &mut usize) -> Option<i32> {
    ws(s, i);
    let st = *i;
    if s.get(*i) == Some(&b'-') { *i += 1 }
    let (mut v, mut any) = (0i32, false);
    while let Some(c) = s.get(*i).filter(|c| c.is_ascii_digit()) { v = v.saturating_mul(10).saturating_add((c - b'0') as i32); any = true; *i += 1 }
    any.then(|| if s[st] == b'-' { -v } else { v })
}

/// Note length in ticks (1920 per whole note): number, dots, and `^` ties.
fn len(s: &[u8], i: &mut usize, l: i32) -> i32 {
    let mut total = 0;
    loop {
        ws(s, i);
        let mut d = if s.get(*i) == Some(&2) { *i += 1; num(s, i).unwrap_or(l) } else { match num(s, i) { Some(n) if n > 0 => 1920 / n, Some(0) => 0, _ => l } };
        let mut h = d;
        ws(s, i);
        while s.get(*i) == Some(&b'.') { *i += 1; h /= 2; d += h }
        total += d;
        ws(s, i);
        if s.get(*i) != Some(&b'^') { return total }
        *i += 1;
    }
}

/// Base64url (`A-Z a-z 0-9 - _`) to bytes.
fn unb64(dat: &[u8]) -> Vec<u8> {
    let (mut bytes, mut acc, mut nb) = (vec![], 0u32, 0);
    for &c in dat {
        let d = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'-' => 62, b'_' => 63, _ => continue };
        (acc, nb) = ((acc << 6 | d as u32) & 0xffff, nb + 6);
        if nb >= 8 { nb -= 8; bytes.push((acc >> nb) as u8) }
    }
    bytes
}

/// `#PCMB n{data}rate,key,loop,flags,level`: base64url of signed little-endian frames, 8-bit or (flags 1) 16-bit;
/// played at `rate` (44100) at note `key` (60); looping from frame `loop` (none if omitted or negative), back and
/// forth with flags 2. With flags 4 the data is NES DPCM, decoded as the console's delta modulation channel plays
/// it: each bit, least significant first, moves a 7-bit level up (1) or down (0) by 2, staying within 0..127,
/// from `level` (0); the frames are the level / 128, frame 0 the starting level, so there is one more frame than
/// bits. A channel plays it the way the console's channel does (the level holds between samples; see
/// `Chan::dpcm`). `{=m}` instead of data: the bytes of DPCM sample `m` (an earlier `#PCMB`), so the same DPCM
/// sample started from another level is stored once. With flags 8 the data is SNES BRR blocks, decoded as the
/// S-DSP decodes them (`brr`) and played with its Gaussian interpolation (`Sample::gauss`).
fn pcm(bytes: &[u8], a: &[f32]) -> Sample {
    let flags = a.get(3).map_or(0, |f| *f as i32);
    let data = if flags & 4 != 0 {
        let mut l = a.get(4).map_or(0, |l| *l as i32).clamp(0, 127);
        let mut d = Vec::with_capacity(bytes.len() * 8 + 1);
        d.push(l as f32 / 128.0);
        for &b in bytes {
            for k in 0..8 {
                l = if b >> k & 1 != 0 { if l <= 125 { l + 2 } else { l } } else if l >= 2 { l - 2 } else { l };
                d.push(l as f32 / 128.0);
            }
        }
        d
    } else if flags & 8 != 0 { brr(bytes) } else if flags & 1 != 0 { bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect() }
        else { bytes.iter().map(|&b| b as i8 as f32 / 128.0).collect() };
    let lp = a.get(2).filter(|l| **l >= 0.0).map(|l| *l as usize);
    Sample { data, rate: a.first().copied().unwrap_or(RATE).max(0.0), key: a.get(1).map_or(60, |k| *k as i32), lp, bidi: flags & 2 != 0, gauss: flags & 8 != 0 }
}

/// SNES BRR blocks as frames in -1..1 (see `decode_brr`).
fn brr(bytes: &[u8]) -> Vec<f32> { decode_brr(bytes).into_iter().map(|s| s as f32 / 32768.0).collect() }

/// SNES BRR blocks (9 bytes: a header with the shift and prediction filter, then 16 4-bit frames) decoded as the
/// S-DSP decodes them, one block after another (the filter's history running on), kept to 15 bits: 16-bit frames.
pub fn decode_brr(bytes: &[u8]) -> Vec<i16> {
    let (mut out, mut p1, mut p2) = (Vec::with_capacity(bytes.len() / 9 * 16), 0i32, 0i32);
    for b in bytes.chunks_exact(9) {
        let (shift, filter) = (b[0] >> 4, b[0] >> 2 & 3);
        for k in 0..16 {
            let n = (((b[1 + k / 2] >> if k & 1 == 0 { 4 } else { 0 }) as i32) << 28) >> 28;
            let mut s = if shift <= 12 { (n << shift) >> 1 } else if n < 0 { -2048 } else { 0 };
            let q = p2 >> 1;
            match filter {
                1 => s += (p1 >> 1) + ((-p1) >> 5),
                2 => s += p1 - q + (q >> 4) + ((p1 * -3) >> 6),
                3 => s += p1 - q + ((p1 * -13) >> 7) + ((q * 3) >> 4),
                _ => {}
            }
            let s = (s.clamp(-32768, 32767) * 2) as i16 as i32;
            (p2, p1) = (p1, s);
            out.push(s as i16);
        }
    }
    out
}

/// Comma-separated arguments, each a number or left out (empty).
fn fields(s: &[u8]) -> Vec<Option<f32>> { s.split(|c| *c == b',').map(|f| fnum(f, &mut 0)).collect() }

/// Part of sample `s`, for `#PCMWAVE` and `#SAMPLER`. `a`, each of them optional: the first frame (0), the frame
/// to stop before (the end), the frame to loop from (the sample's own loop; -1: none; below -1: the last that
/// many frames), all counted in `s`; and the note it plays at `s`'s rate at (`s`'s own; fractions allowed).
fn cut(s: &Sample, a: &[Option<f32>; 4]) -> Sample {
    let n = s.data.len();
    let st = a[0].filter(|v| *v >= 0.0).map_or(0, |v| (v as usize).min(n));
    let en = a[1].filter(|v| *v >= 0.0).map_or(n, |v| (v as usize).min(n));
    let en = if en <= st { n } else { en };
    let lp = match a[2] { None => s.lp, Some(l) if l >= 0.0 => Some(l as usize), Some(l) if l < -1.0 => Some(en.saturating_sub(-l as usize)), _ => None };
    let lp = lp.map(|l| l.max(st) - st).filter(|&l| l < en - st);
    let (key, rate) = match a[3] { Some(k) => { let f = M::floor(k); (f as i32, s.rate * M::exp2((f - k) / 12.0)) } None => (s.key, s.rate) };
    Sample { data: s.data[st..en].to_vec(), rate, key, lp, bidi: s.bidi, gauss: s.gauss }
}

/// Sample number for `#PCMWAVE` / `#SAMPLER` argument `id` (a `#PCMB` number) cut by `a` (see `cut`): the
/// `#PCMB` itself if `a` changes nothing, else a cut of it (made once for the same arguments).
fn cut_of(sh: &mut Shared, id: Option<f32>, a: [Option<f32>; 4]) -> Option<usize> {
    let id = id.filter(|i| (0.0..256.0).contains(i))? as usize;
    if a.iter().all(Option::is_none) { return Some(id) }
    let k = sh.cuts.iter().position(|c| c.0 == id && c.1 == a).unwrap_or_else(|| { sh.cuts.push((id, a)); sh.cuts.len() - 1 });
    Some(256 + k)
}

/// Make the cut samples from the `#PCMB`s they are cut from (`only`: just those of that one).
fn cuts(sh: &mut Shared, only: Option<usize>) {
    sh.samples.resize(256 + sh.cuts.len(), None);
    sh.dpcm.resize(256 + sh.cuts.len(), false);
    for (k, (id, a)) in sh.cuts.iter().enumerate() {
        if only.is_some_and(|o| o != *id) { continue }
        sh.samples[256 + k] = sh.samples[*id].as_ref().map(|s| cut(s, a));
        sh.dpcm[256 + k] = sh.dpcm[*id];
    }
}

/// `#PCMVOICE`'s key scaling, as SiON's: below `center - range / 2` keys have `below`, then each one `step` more
/// up to `center + range / 2`, and `above` from there.
fn key_scale(center: f32, range: f32, below: f32, step: f32, above: f32) -> Vec<f32> {
    let (lo, hi, mut v) = ((center - range * 0.5) as i32, (center + range * 0.5) as i32, below);
    (0..128).map(|i| if i < lo { below } else if i < hi { v += step; v - step } else { above }).collect()
}

/// `#PCMWAVE` (`W`), `#PCMVOICE` (`V`) or `#SAMPLER` (`S`) number `k`. They only refer to `#PCMB`s by number, so
/// they can come before them: the cut samples are made once all are read (`cuts`). See the README for the
/// arguments.
fn pcm_table(sh: &mut Shared, kind: u8, k: usize, dat: &[u8], pfx: &[u8]) {
    let blank = || PcmVoice { keys: None, vol: vec![1.0; 128], pan: vec![0; 128], env: None, init: vec![] };
    let f = fields(dat);
    let a = |i: usize| f.get(i).copied().flatten();
    let at = |i: usize, d: f32| a(i).unwrap_or(d);
    match kind {
        // `#PCMWAVEn{id,note,from,to,channels,start,end,loop}`
        b'W' => {
            let Some(smp) = cut_of(sh, a(0), [a(5), a(6), a(7), a(1)]) else { return };
            let (from, to) = (at(2, 0.0).max(0.0) as usize, at(3, 127.0) as i32);
            let to = if to == -1 { from } else { to.min(127) as usize };
            let v = sh.pcmv[k & 255].get_or_insert_with(blank);
            let keys = v.keys.get_or_insert_with(|| vec![NO_SAMPLE; 128]);
            for key in keys.iter_mut().take(to + 1).skip(from) { *key = smp }
        }
        // `#PCMVOICEn{volume center,range,amount, pan center,range,width, ar,dr,sr,rr,sl}commands`
        b'V' => {
            let v = sh.pcmv[k & 255].get_or_insert_with(blank);
            let (vc, vr, vw) = (at(0, 64.0) as i32 as f32, at(1, 0.0), at(2, 0.0) / 128.0);
            let step = |w: f32, r: f32| if r == 0.0 { w } else { w / r };
            v.vol = key_scale(vc, vr, 1.0 - vw.max(0.0), step(vw, vr), 1.0 + vw.min(0.0));
            let (pc, pr, pw) = (at(3, 64.0) as i32 as f32, at(4, 0.0), at(5, 0.0));
            v.pan = key_scale(pc, pr, -pw * 0.5, step(pw, pr), pw * 0.5).iter().map(|p| *p as i32).collect();
            v.env = Some([at(6, 63.0), at(7, 0.0), at(8, 0.0), at(9, 63.0), at(10, 0.0)].map(|x| x as i32));
            v.init = pfx.to_vec();
        }
        // `#SAMPLERn{id,ignore key-off,pan,channels,start,end,loop}`: bank n / 128, key n % 128.
        _ => {
            let Some(smp) = cut_of(sh, a(0), [a(4), a(5), a(6), None]) else { return };
            let bank = sh.kit[(k >> 7) & 3].get_or_insert_with(|| vec![None; 128]);
            bank[k & 127] = Some(KitKey { smp, hold: at(1, 0.0) != 0.0, pan: at(2, 0.0) as i32 });
        }
    }
}

/// All numbers in a voice definition.
fn nums(s: &[u8]) -> Vec<f32> {
    let (mut v, mut i) = (vec![], 0);
    while i < s.len() { match fnum(s, &mut i) { Some(n) => v.push(n), None => i += 1 } }
    v
}

/// `#WAVCOLOR`: a 1024-sample wave adding sine harmonics 1,2,3,4,5,6,8 at the levels of hex digits 0..6
/// (lowest first, n/16 each), scaled so its peak is full scale. The top digit picks the base wave
/// (`%5@0`..`%5@15`, sine by default), as in SiON (GDSiON always uses a sine). Sampled like SiON:
/// sample `j` reads the base wave at phase `j + 63/64`; SiON's sine table is read at entry centers.
fn wave_color(sh: &Shared, c: u32) -> Vec<f32> {
    let pg = (c >> 28) as i32;
    let base = |p: u32| if pg == 0 { M::sin(((p & 65535) >> 6) as f32 * (TAU / 1024.0) + PI / 1024.0) }
        else { wave(sh, pg, (p & 65535) as f32 / 65536.0) };
    let out: Vec<f32> = (0..1024u32).map(|j| [1, 2, 3, 4, 5, 6, 8].iter().enumerate()
        .map(|(n, m)| base((j * 64 + 63) * m) * (c >> (n * 4) & 15) as f32 / 16.0).sum()).collect();
    let peak = out.iter().fold(0.0f32, |a, v| a.max(v.abs()));
    if peak == 0.0 { return out }
    out.iter().map(|v| v / peak).collect()
}

/// Parse `#TABLE` / `#WAV` numbers: values, `(a,b)n` interpolation, `[..]n` loops, `|` repeat point,
/// with a `n*scale+offset` postfix.
fn table(d: &[u8], pfx: &[u8]) -> Table {
    let mut j = 0;
    let rep = num(pfx, &mut j).filter(|n| *n > 0).unwrap_or(1);
    let r = if pfx.get(j) == Some(&b'*') { j += 1; fnum(pfx, &mut j).unwrap_or(1.0) } else { 1.0 };
    let o = match pfx.get(j) { Some(b'+') => fnum(pfx, &mut (j + 1)).unwrap_or(0.0), Some(b'-') => fnum(pfx, &mut j).unwrap_or(0.0), _ => 0.0 };
    let (mut out, mut lp, mut stack, mut i) = (vec![], None, vec![], 0);
    let put = |out: &mut Vec<i32>, v: f32| for _ in 0..rep { out.push((v * r + o + 0.5) as i32) };
    while i < d.len() && out.len() < 65536 {
        match d[i] {
            b'(' => {
                let e = find(d, i, b")");
                let mut j = e;
                while d.get(j).is_some_and(|c| c.is_ascii_whitespace() || *c == b',') { j += 1 }
                // Without a count after `)` it's not an interpolation: SiON reads the numbers inside as plain values.
                if !d.get(j).is_some_and(u8::is_ascii_digit) { i += 1; continue }
                let list = nums(&d[i + 1..e]);
                i = j;
                let n = num(d, &mut i).unwrap_or(0);
                for k in 0..n {
                    let t = if list.len() > 1 { k as f32 * (list.len() - 1) as f32 / n as f32 } else { 0.0 };
                    let (a, tr) = (t as usize, M::fract(t));
                    match (list.get(a), list.get(a + 1)) {
                        (Some(x), Some(y)) => put(&mut out, M::trunc(x * (1.0 - tr) + y * tr + 0.5)),
                        (Some(x), None) => put(&mut out, *x),
                        _ => {}
                    }
                }
            }
            b'-' | b'0'..=b'9' => match num(d, &mut i) { Some(v) => put(&mut out, v as f32), None => i += 1 },
            b'|' => { lp = Some(out.len()); i += 1 }
            b'[' => { stack.push(out.len()); i += 1 }
            b']' => {
                i += 1;
                let n = if d.get(i).is_some_and(u8::is_ascii_digit) { num(d, &mut i).unwrap_or(2) } else { 2 };
                if let Some(h) = stack.pop() { let e = out.len(); for _ in 0..n.max(1) { out.extend_from_within(h..e) } }
            }
            _ => i += 1,
        }
    }
    (out, lp)
}

/// Value `i` steps into a table, following its loop (or holding the last value).
fn tval(t: &Table, i: usize) -> i32 {
    let n = t.0.len();
    if i < n { t.0[i] } else { match t.1 { Some(l) if l < n => t.0[l + (i - l) % (n - l)], _ => t.0[n - 1] } }
}

/// Key signature (`#SIGN`) as a semitone shift per note letter a..g.
/// Named keys (`D`, `F#m`, `Bb`...) or a custom list such as `c+ f+ b-`. (Non-ASCII, such as full-width
/// spaces, counts as space.)
fn sign(raw: &[u8]) -> [i32; 7] {
    let sp = |c: &u8| c.is_ascii_whitespace() || *c >= 0x80;
    let (a, b) = (raw.iter().position(|c| !sp(c)).unwrap_or(raw.len()), raw.iter().rposition(|c| !sp(c)).map_or(0, |e| e + 1));
    let s: Vec<u8> = raw[a..b.max(a)].iter().enumerate().map(|(i, &c)| match c { b'#' => b'+', b'b' if i > 0 => b'-', c => c }).collect();
    let mut k = [0; 7];
    for (d, names, order) in [(1, &b"G Em D Bm A F+m E C+m B G+m F+ D+m C+ A+m"[..], b"fcgdaeb"), (-1, b"F Dm B- Gm E- Cm A- Fm D- B-m G- E-m C- A-m", b"beadgcf")] {
        if let Some(n) = names.split(|c| *c == b' ').position(|x| x == s) { for c in order.iter().take(n / 2 + 1) { k[(c - b'a') as usize] = d } return k }
    }
    for t in raw.split(|c| sp(c) || *c == b',') {
        if let Some(n @ b'a'..=b'g') = t.first().map(u8::to_ascii_lowercase) {
            k[(n - b'a') as usize] = match t.get(1) { Some(b'+' | b'#') => 1, Some(b'-' | b'b') => -1, _ => 0 };
        }
    }
    k
}

/// Build a voice from `#@`, `#OPM@`, `#OPN@`, `#OPL@`, `#OPX@`, `#MA@` or `#AL@` numbers.
fn voice(name: &[u8], d: &[f32], init: &[u8]) -> Option<Voice> {
    const OPM: [&[u8]; 4] = [&[0], &[0, 1, 1, 1, 1, 0, 1, 1], &[0, 1, 2, 3, 3, 4, 3, 5], &[0, 1, 2, 3, 4, 5, 6, 7]];
    const OPL: [&[u8]; 4] = [&[0], &[0, 1], &[0, 3, 2, 2], &[0, 4, 8, 9]];
    const MA3: [&[u8]; 4] = [&[0], &[0, 1, 1, 1, 0, 1, 1, 1], &[0, 0, 5, 2, 0, 3, 2, 2], &[0, 0, 7, 2, 0, 4, 8, 9]];
    const OPX: [&[u8]; 4] = [&[0], &[0, 16, 1, 2], &[0, 16, 1, 2, 3, 19, 5, 6], &[0, 16, 1, 2, 3, 19, 4, 20, 8, 11, 6, 22, 5, 9, 12, 7]];
    let (f, chn, fmt) = match name {
        b"@" => (0, 3, "WADSRLTKkM12aPF"), b"OPL@" => (1, 2, "WADRELTKkMa"), b"OPM@" => (2, 2, "ADSRLTKM12a"),
        b"OPN@" => (3, 2, "ADSRLTKM1a"), b"OPX@" => (4, 2, "WADSRLTKM12a"), b"MA@" => (5, 2, "WADSRLTKkM1a"), _ => (6, 9, ""),
    };
    let mut v = Voice { n: 5, alg: 0, fb: 0, fbc: 0, fratio: if f == 1 || f == 5 { 1.33 } else { 1.0 }, ops: vec![], init: init.to_vec() };
    if f == 6 {
        if d.len() != 9 { return None }
        let tl = |b: f32| if b <= 0.0 { 104 } else { (-8.0 * M::log2(b / 128.0) + 0.5) as i32 };
        let (bal, mut a, mut b) = (d[3].clamp(-64.0, 64.0), Op::new(false), Op::new(false));
        (a.pg, a.tl, a.ar, a.dr, a.sl, a.rr) = (d[1] as i32, tl(64.0 - bal), d[5] as i32 & 63, d[6] as i32 & 63, d[7] as i32 & 15, d[8] as i32 & 15);
        (b.pg, b.tl, b.det) = (d[2] as i32, tl(64.0 + bal), d[4] as i32);
        (a.pt, b.pt, v.alg) = (Op::pt_of(a.pg), Op::pt_of(b.pg), if (0.0..=2.0).contains(&d[0]) { d[0] as usize } else { 0 });
        v.ops = vec![a, b];
        return Some(v);
    }
    v.n = (1..5).find(|i| d.len() == chn + fmt.len() * i)?;
    let alg = match f { 0 => d[0] as u8, _ => *[OPL, OPM, OPM, OPX, MA3][f - 1][v.n - 1].get(d[0] as usize & 15).unwrap_or(&0) };
    (v.alg, v.fb, v.fbc) = ((alg & 15) as usize, d[1] as i32, if f == 0 { d[2] as usize } else { (alg >> 4) as usize });
    let (sh, mut k) = ([0, 2, 1, 1, 1, 2][f], chn);
    for _ in 0..v.n {
        let mut o = Op::new(false);
        for c in fmt.bytes() {
            let (x, g) = (d[k], d[k] as i32);
            k += 1;
            match c {
                b'W' => { o.pg = match f { 0 => g & 511, 4 if g >= 7 => 256 + g - 7, 4 => 32 + (g & 7), _ => 32 + (g & 31) }; o.pt = Op::pt_of(o.pg) }
                b'A' => o.ar = (g << sh) & 63,
                b'D' => o.dr = (g << sh) & 63,
                b'S' => o.sr = (g << sh) & 63,
                b'R' => o.rr = (if sh == 1 { (g << 2) + 2 } else { g << sh }) & 63,
                b'E' => o.sr = if g != 0 { 0 } else { o.rr },
                b'L' => o.sl = g & 15,
                b'T' => o.tl = g & if sh == 2 { 63 } else { 127 },
                b'K' => o.ksr = (if sh == 2 { g << 1 } else { g }) & 3,
                b'k' => o.ksl = g & 3,
                b'M' if f == 0 => o.fmul = if x == 0.0 { 64 } else { (x * 128.0) as i32 },
                b'M' => o.mul(if sh == 2 { match g & 15 { m @ (11 | 13) => m - 1, 14 => 15, m => m } } else { g }),
                b'1' => o.dt1 = g & 7,
                b'2' => o.det = if f == 2 { [0, 384, 500, 608][g as usize & 3] } else { g },
                b'a' => o.ams = g & 3,
                b'P' => o.phase = if g == -1 { -1 } else { g & 255 },
                _ => o.fix = (g & 127) << 6,
            }
        }
        v.ops.push(o);
    }
    Some(v)
}

/// Waveform and pitch table for tone `t` of module `md` (`%md @t`).
fn tone(md: i32, t: i32) -> (i32, u8) {
    let count = [3, 11, 16, 32, 256, 512, 0, 0, 32, 128, 0, 0, 11, 9, 12].get(md as usize).copied().unwrap_or(0);
    let t = if t < 0 || t >= count { matches!(md, 1 | 12 | 13 | 14) as i32 } else { t };
    match (md, t) {
        (0, 0) => (5, 2), (0, 1) => (17, 4), (0, _) => (24, 2),
        (1 | 12, 0..=7) => (64 + t * 2, 2), (13 | 14, 0..=7) => (64 + t, 2),
        (1, 8) => (3, 2), (1, 9) => (17, 5), (1, _) => (18, 5),
        (12, 8) => (256, 2), (12, 9) => (17, 6), (12, _) => (21, 6),
        (13, _) => (11, 2),
        (14, 8) => (4, 2), (14, 9) => (1, 2), (14, 10) => (11, 2), (14, _) => (17, 3),
        (m, _) => { let pg = [0, 0, 16, 32, 256, 0, 0, 0, 64, 128].get(m as usize).copied().unwrap_or(0) + t; (pg, Op::pt_of(pg)) }
    }
}

/// Multi-character commands, longest first (single characters are matched as themselves).
const CMD: [&str; 44] = [
    "@clock", "@mask", "!@kr", "!@ns", "@lfo", "@fps", "_@@", "_na", "_np", "_nt", "_nf", "!na", "@al", "@fb", "@rr", "@tl", "@ml", "@dt", "@ph", "@fx", "@se", "@er",
    "@@", "na", "np", "nt", "nf", "mp", "ma", "po", "kt", "%v", "%x", "%f", "%t", "%e", "@q", "@v", "@i", "@o", "@p", "@f", "@r", "&&",
];

/// Length of the command name at the start of `s` (non-empty).
fn name_len(s: &[u8]) -> usize { CMD.iter().find(|c| s.starts_with(c.as_bytes())).map_or(1, |c| c.len()) }

/// Envelope commands (`_`: for key-off), longest first.
const ENV: [&[u8]; 11] = [b"_@@", b"_na", b"_nt", b"_np", b"_nf", b"!na", b"@@", b"na", b"nt", b"np", b"nf"];

struct Track {
    s: Arc<[u8]>, i: usize, loops: Vec<(usize, i32)>, rep: Option<usize>, moved: bool, end: bool,
    /// Has reached the end of its sequence at least once (ended, or jumped back to its `$` loop point).
    once: bool,
    at: f64, koff: f64, kon: f64, q: f64, qc: f64, kd: f64,
    oct: i32, len: i32, ns: i32, vel: i32, vsh: i32, x: i32, kt: i32, k: i32, kf: f32, note: i32, slur: bool, md: i32, tone: i32,
    /// Envelope settings for [key-off, key-on] x [amplitude, tone, note, pitch, filter]: (table, frames per step).
    set: [[Option<(usize, i32)>; 5]; 2],
    xoff: [bool; 2], sweep: [i32; 2],
    /// Where the release sweep stops: 0 until `s`/`@rr` set it (then its limit, even for no sweep, as SiON does).
    swe0: f64, modt: [Option<Vec<i32>>; 2],
    env: [Option<(usize, usize, i32, i32)>; 5], menv: Option<usize>,
    swp: f64, sws: f64, swe: f64, pact: bool, xo: i32, res: f32, itv: f32,
    /// `@mask` bits: ignore volume 1, pan 2, quantize 4, operator 8, envelope 16, modulation 32, slur 64 commands.
    mask: i32,
    /// Tick of the last tempo command this track ran.
    tset: f64,
    /// How far into the frame just played (0..1) the command being run fell.
    late: f32,
    /// While skipping (see `Song::skip`): frames the channel has yet to be leapt along by, the frame up to
    /// which `res` has counted down, and the first frame the track may have something to do.
    lag: u64, seen: u64, next: u64,
    /// Byte offset in the source MML of the note playing now (-1: none), from `\x03<offset>` markers (see `spans`).
    src: i32,
    ch: Chan,
    dc: Declick,
}

/// Smooths over the jumps in a track's output that a key-on, volume or pan change makes mid-wave (a waveform
/// restarting at its first frame, a hard pan flip), as trackers ramp them: the output picks up where it would
/// have been without the change, and the difference fades out within about a millisecond. A key-on only
/// carries the old note's level over (so attacks stay sharp). Output that no such change touches is exactly
/// SiON's.
#[derive(Clone, Copy, Default)]
struct Declick {
    /// The last channel sample and pan: while nothing is fading, the last output is that sample at that pan.
    v: f32, pan: i32,
    /// A key-on since the last sample (that has a level to carry over: a plucked string's starts a noise burst).
    kon: bool,
    /// While an offset is fading (`left` samples more): it (left, right, mono: for a send without panning), what
    /// it drops by each sample, and the last output.
    left: u32, off: [f32; 3], step: [f32; 3], last: [f32; 3],
}

/// Samples a declick offset takes to fade out (about a millisecond).
const DECLICK: u32 = 48;

impl Declick {
    fn new(pan: i32) -> Declick { Declick { pan, ..Declick::default() } }

    fn fading(&self) -> bool { self.left > 0 }

    /// Nothing to do this sample (the usual case): no changes (`vj`: see `Chan::vj`), nothing fading, the pan as
    /// it was.
    #[inline]
    fn calm(&self, pan: i32, vj: f32) -> bool { !self.kon && vj == 1.0 && !self.fading() && pan == self.pan }

    /// The output for channel sample `v` at pan `pan` (gains `lr`; `old`: those of the last pan), after a key-on
    /// or volume changes since the last sample (`vjump`: see `Chan::vj`), or with an offset still fading.
    /// A key-on, or a volume change from silence, only carries the old level over; other changes ease all the
    /// way.
    #[cold]
    #[inline(never)]
    fn run(&mut self, v: f32, pan: i32, lr: [f32; 2], old: [f32; 2], vjump: f32) -> [f32; 3] {
        let kon = core::mem::take(&mut self.kon);
        let o = [v * lr[0], v * lr[1], v];
        let last = if self.fading() { self.last } else { [self.v * old[0], self.v * old[1], self.v] };
        if kon || vjump != 1.0 || pan != self.pan {
            // (Turned up from silence, or starting after it, is an attack too.)
            let attack = kon || vjump == 0.0 || last == [0.0; 3];
            for k in 0..3 {
                let l = last[k];
                self.off[k] = if attack { (l - o[k]).clamp(l.min(0.0), l.max(0.0)) } else {
                    // What it would have been: the old gains, plus what was still fading.
                    let was = if vjump.is_finite() { v * vjump * if k < 2 { old[k] } else { 1.0 } + self.off[k] } else { l };
                    was - o[k]
                };
                self.step[k] = self.off[k] / DECLICK as f32;
            }
            self.left = if self.off == [0.0; 3] { 0 } else { DECLICK };
        }
        let mut out = o;
        if self.fading() {
            for k in 0..3 { out[k] += self.off[k]; self.off[k] -= self.step[k] }
            self.left -= 1;
            if self.left == 0 { self.off = [0.0; 3] }
        }
        (self.v, self.pan, self.last) = (v, pan, out);
        out
    }
}

impl Track {
    fn new(s: Vec<u8>, sh: &Shared) -> Track {
        let mut t = Track {
            s: s.into(), i: 0, loops: vec![], rep: None, moved: false, end: false, once: false,
            at: 0.0, koff: INF, kon: INF, q: (sh.quant * 3 / 4) as f64 / sh.quant as f64, qc: 0.0, kd: 0.0,
            oct: 5, len: 480, ns: 0, vel: 256, vsh: sh.vmode.2, x: 128, kt: 0, k: 0, kf: 0.0, note: 0, slur: false, md: 0, tone: 0,
            set: [[None; 5]; 2], xoff: [false; 2], sweep: [0; 2], swe0: 0.0, modt: [None, None], env: [None; 5], menv: None,
            swp: 0.0, sws: 0.0, swe: 0.0, pact: false, xo: 0, res: 1.0, itv: RATE / sh.fps, mask: 0, tset: 0.0, late: 0.0, lag: 0, seen: 0, next: 0, src: -1,
            ch: Chan::new(), dc: Declick::new(64),
        };
        t.ch.vm = (sh.vmode.0, sh.vmode.1);
        t.module(sh, 0, MIN);
        t
    }

    /// `%` module change: reset the channel and pick the module's default tone.
    fn module(&mut self, sh: &Shared, md: i32, chan: i32) {
        self.md = match md { 0..=14 => md, 15..=20 => 6, _ => 5 };
        self.ch.init(self.md);
        self.tone = match (self.md, chan) {
            (0, 0..=3) => chan / 3, (1 | 12, 0..=3) => [4, 4, 8, 9][chan as usize], (13, 0..=2) => 7 + chan / 2, (14, 0..=2) => 7,
            (2..=11, 0..) if chan < [16, 32, 256, 512, 1, 0, 32, 128, 0, 3][self.md as usize - 2] => chan,
            _ => matches!(self.md, 1 | 12 | 13 | 14) as i32,
        };
        self.select(sh, self.tone);
        self.ch.volume(self.x, self.vel);
    }

    /// Select tone / voice number `v` for the current module; returns the voice's init sequence, if any.
    fn select<'a>(&mut self, sh: &'a Shared, v: i32) -> Option<&'a [u8]> {
        match self.md {
            6 => if let Some(voice) = sh.voice(v.max(0) as usize) {
                self.ch.set_voice(voice);
                self.ch.volume(self.x, self.vel);
                return Some(&voice.init);
            }
            7 | 10 => {
                (self.ch.smp, self.ch.stale) = (v.max(0) as usize, true);
                // A `#PCMVOICE`'s envelope and commands.
                if let Some(p) = sh.pcmv.get(self.ch.smp).and_then(|p| p.as_ref()).filter(|_| self.md == 7) {
                    if let Some(e) = p.env { self.ch.params(&e) }
                    return Some(&p.init);
                }
            }
            11 => {}
            _ => { let o = &mut self.ch.ops[self.ch.act]; (o.pg, o.pt) = tone(self.md, v) }
        }
        None
    }

    /// Activate the envelope set for key-on (1) or key-off (0).
    fn process(&mut self, sh: &Shared, on: usize) {
        for j in 0..5 { self.env[j] = self.set[on][j].filter(|(t, _)| sh.tables[*t].is_some()).map(|(t, s)| (t, 0, 1, s)) }
        self.menv = if on == 1 { Some(0) } else { None };
        (self.sws, self.swe) = if on == 1 { (0.0, 0.0) } else { (self.sweep[0] as f64, self.swe0) };
        self.swp = self.ch.pitch as f64;
        self.xo = if self.xoff[on] { self.x } else { 0 };
        self.pact = self.sweep[on] != 0 || self.set[on][2].is_some() || self.set[on][3].is_some();
        if self.env[4].is_some() { self.ch.f_on = true }
        self.res = 1.0;
    }

    fn key_on(&mut self, sh: &Shared) {
        // A PCM note's start or pitch change is placed at this event's exact time (in the frame it comes in).
        self.ch.slate = self.late;
        let (old, pi) = (self.ch.pitch, ((self.note + self.kt) << 6) + self.k);
        (self.ch.pitch, self.ch.fine) = (pi, self.kf);
        if !self.slur {
            if self.set.iter().flatten().any(|e| e.is_some()) {
                self.ch.volume(self.x, self.vel);
                self.ch.cut_off = 0;
                if self.set[0][1].or(self.set[1][1]).is_some() { self.select(sh, self.tone); }
            }
            if self.ch.on { self.ch.note_off() }
            self.process(sh, 1);
            self.ch.note_on();
            self.dc.kon = !self.ch.ks;
        } else if self.sweep[1] > 0 {
            self.ch.pitch = old;
            (self.sws, self.swe, self.swp) = ((pi - old) as f64 / self.sweep[1] as f64, pi as f64, old as f64);
        } else {
            self.swp = pi as f64;
        }
        self.slur = false;
    }

    fn key_off(&mut self, sh: &Shared) {
        self.ch.note_off();
        self.koff = INF;
        self.process(sh, 0);
    }

    /// Step envelope `k`; returns the value it was on if it advanced.
    fn adv(&mut self, sh: &Shared, k: usize) -> Option<i32> {
        let (t, pos, cnt, step) = self.env[k].as_mut()?;
        *cnt -= 1;
        if *cnt > 0 { return None }
        let v = tval(sh.tables[*t].as_ref()?, *pos);
        (*pos, *cnt) = (*pos + 1, *step);
        Some(v)
    }

    fn cur(&self, sh: &Shared, k: usize) -> i32 {
        self.env[k].and_then(|(t, pos, ..)| Some(tval(sh.tables[t].as_ref()?, pos))).unwrap_or(0)
    }

    /// One envelope frame (`#FPS` times per second).
    fn frame(&mut self, sh: &Shared) {
        if let Some(v) = self.adv(sh, 0) { self.ch.volume((self.xo + v).clamp(0, 128), self.vel) }
        if self.pact {
            self.ch.pitch = self.cur(sh, 3) + (self.cur(sh, 2) << 6) + self.swp as i32;
            self.adv(sh, 3);
            self.adv(sh, 2);
            self.swp += self.sws;
            if (self.sws > 0.0 && self.swp > self.swe) || (self.sws <= 0.0 && self.swp < self.swe) { (self.swp, self.sws) = (self.swe, 0.0) }
        }
        if let Some(v) = self.adv(sh, 4) { self.ch.cut_off = v - 128 }
        if let Some(v) = self.adv(sh, 1) { self.select(sh, v); }
        if let Some(i) = self.menv {
            for (k, t) in self.modt.iter().enumerate() {
                if let Some(v) = t.as_ref().and_then(|t| t.get(i)) { if k == 0 { self.ch.set_am(*v) } else { self.ch.set_pm(*v) } }
            }
            self.menv = Some(i + 1);
        }
    }

    /// No commands or key on/off left to run.
    fn done(&self) -> bool { self.end && self.koff == INF && self.kon == INF }

    /// Advance to tick `now`: run due commands, key on/off and envelope frames.
    #[inline]
    fn run(&mut self, sh: &Shared, now: f64, bpm: &mut f64) {
        // Events that fall exactly on this frame are due now, whichever way rounding in `now` went.
        let due = now + 1e-6;
        // (Most frames have nothing due but the countdown to the next envelope frame.)
        if (self.end || due < self.at) && due < self.kon && due < self.koff && self.res > 1.0 { self.res -= 1.0 } else { self.events(sh, now, bpm) }
    }

    /// `run` for a frame with something to do. How far into the previous frame an event fell lets a PCM note
    /// start, or change pitch, at its exact time.
    #[cfg_attr(feature = "small", optimize(speed))]
    fn events(&mut self, sh: &Shared, now: f64, bpm: &mut f64) {
        let due = now + 1e-6;
        let late = |at: f64, bpm: f64| ((now - at) * RATE as f64 / (bpm * 8.0)).clamp(0.0, 1.0) as f32;
        while !self.end && due >= self.at {
            self.ch.stale = true;
            self.late = late(self.at, *bpm);
            let s = self.s.clone();
            if self.i < s.len() {
                let mut i = self.i;
                self.exec(&s, &mut i, sh, bpm, true);
                self.i = i;
            } else if let (Some(r), true) = (self.rep, self.moved) {
                (self.i, self.moved, self.once, self.ch.rejoin) = (r, false, true, true);
            } else {
                (self.end, self.once, self.src) = (true, true, -1);
            }
        }
        if due >= self.kon { self.late = late(self.kon, *bpm); self.kon = INF; self.key_on(sh); self.ch.stale = true }
        if due >= self.koff { self.key_off(sh); self.ch.stale = true }
        self.res -= 1.0;
        if self.res <= 0.0 { self.frame(sh); self.res += self.itv; self.ch.stale = true }
    }

    /// Count down to the next envelope frame for the frames up to `pos` the track has been left alone for,
    /// and with `replay` run the envelope frames due in them (exactly as running each frame would).
    fn catch(&mut self, sh: &Shared, pos: u64, replay: bool) {
        let mut k = pos - self.seen;
        (self.lag, self.seen) = (self.lag + k, pos);
        while replay {
            let f = (-M::floor(-self.res)).max(1.0);
            if f > k as f32 { break }
            (self.res, k) = (self.res - f, k - f as u64);
            self.frame(sh);
            (self.res, self.ch.stale) = (self.res + self.itv, true);
        }
        self.res -= k as f32;
    }

    /// While skipping, whether the channel has to leap along at each envelope frame: they move its pitch,
    /// its LFO depth or its PCM sample's speed, or its LFO is on (and the pitch is taken up at each frame).
    /// Otherwise they're replayed when the track is next run.
    fn ticks(&self) -> bool {
        self.pact || self.ch.pcm >= 0 || self.ch.pmd != 0 || self.ch.amd > 0 || (self.menv.is_some() && self.modt.iter().any(Option::is_some))
    }

    /// The first frame from `pos` (whose tick is `now`) on that may have something for `run` to do (a
    /// command, key on/off, or with `ticks` an envelope frame due), given the ticks per frame. Errs early:
    /// a frame early just means running it the slow way.
    fn next(&self, pos: u64, now: f64, step: f64, ticks: bool) -> u64 {
        let until = |at: f64| (at - now) / step - 2.0;
        let cmd = if self.end { INF } else { until(self.at) };
        let res = if ticks { (self.res - (pos - self.seen) as f32) as f64 - 1.0 } else { INF };
        pos.saturating_add(res.min(until(self.kon)).min(until(self.koff)).min(cmd).max(0.0) as u64)
    }

    /// Execute the command at `s[*i]`. `main` is false for a voice's init sequence, where loops are ignored.
    fn exec(&mut self, s: &[u8], i: &mut usize, sh: &Shared, bpm: &mut f64, main: bool) {
        let (st, c) = (*i, s[*i]);
        *i += 1;
        match c {
            // A note: letter (voice init sequences) or `\x01<number>` from `statics`.
            b'a'..=b'g' | 1 => {
                self.note = if c == 1 { num(s, i).unwrap_or(60) } else {
                    let n = key(c, s.get(*i), self.oct, self.ns, &sh.sign);
                    if matches!(s.get(*i), Some(b'+' | b'#' | b'-')) { *i += 1 }
                    n
                }.clamp(0, 127);
                let d = len(s, i, self.len) as f64;
                if self.kd > 0.0 { self.key_off(sh); self.kon = self.at + self.kd } else { self.key_on(sh) }
                self.koff = self.at + self.kd + (d * self.q - self.qc - self.kd).max(0.01);
                match (s.get(*i), s.get(*i + 1)) {
                    (Some(b'&'), Some(b'&')) if self.mask & 64 != 0 => *i += 2,
                    (Some(b'&' | b'*'), _) if self.mask & 64 != 0 => *i += 1,
                    (Some(b'&'), Some(b'&')) => { *i += 2; self.koff = INF }
                    (Some(b'&'), _) => { *i += 1; (self.koff, self.slur) = (INF, true) }
                    (Some(b'*'), _) => {
                        *i += 1;
                        (self.koff, self.slur) = (INF, true);
                        // Pitch bend towards the next note over this note's length.
                        let (mut j, mut oct) = (*i, self.oct);
                        let next = loop {
                            ws(s, &mut j);
                            match s.get(j) {
                                Some(b'<') => { j += 1; oct += sh.rev.0 * num(s, &mut j).unwrap_or(1) }
                                Some(b'>') => { j += 1; oct -= sh.rev.0 * num(s, &mut j).unwrap_or(1) }
                                Some(b'o') => { j += 1; oct = num(s, &mut j).unwrap_or(5) }
                                Some(n @ b'a'..=b'g') => break Some(key(*n, s.get(j + 1), oct, self.ns, &sh.sign)),
                                Some(1) => { j += 1; break num(s, &mut j) }
                                Some(3) => { j += 1; num(s, &mut j); }
                                _ => break None,
                            }
                        };
                        if let Some(n) = next {
                            let (a, b) = (self.ch.pitch, ((n + self.kt) << 6) + self.k);
                            let frames = d / (*bpm * 8.0) * RATE as f64 / self.itv as f64;
                            if a != b {
                                (self.sws, self.swe, self.swp, self.pact) = ((b - a) as f64 / frames.max(1.0), b as f64, a as f64, true);
                                for k in 2..4 { self.env[k] = self.set[1][k].map(|(t, s)| (t, 0, 1, s)) }
                            }
                        }
                    }
                    _ => {}
                }
                (self.at, self.moved) = (self.at + d, true);
            }
            b'r' | b'^' => {
                (self.at, self.moved) = (self.at + len(s, i, self.len) as f64, true);
                self.slur &= c != b'r';
                if c == b'r' { self.src = -1 }
            }
            3 => self.src = num(s, i).unwrap_or(-1),
            b'l' => self.len = len(s, i, self.len),
            b'{' => *i = find(s, *i, b"}"),
            b'$' if main => (self.rep, self.moved) = (Some(*i), false),
            b'[' if main => { let n = num(s, i).unwrap_or(0); self.loops.push((*i, n)) }
            b'|' if main => if let Some(&(_, 1)) = self.loops.last() {
                let mut depth = 1;
                while depth > 0 && *i < s.len() { depth += match s[*i] { b'[' => 1, b']' => -1, _ => 0 }; *i += 1 }
                num(s, i);
                self.loops.pop();
            }
            b']' if main => {
                let n = num(s, i).unwrap_or(2);
                if let Some(t) = self.loops.last_mut() {
                    if t.1 == 0 { t.1 = n }
                    t.1 -= 1;
                    if t.1 > 0 { *i = t.0 } else { self.loops.pop(); }
                }
            }
            _ => {
                let name = &s[st..st + name_len(&s[st..])];
                *i = st + name.len();
                let (mut a, mut n) = ([MIN; 16], 0);
                loop {
                    a[n] = num(s, i).unwrap_or(MIN);
                    n += 1;
                    ws(s, i);
                    if n < 16 && s.get(*i) == Some(&b',') { *i += 1 } else { break }
                }
                let p = |k: usize, d: i32| if a[k] == MIN { d } else { a[k] };
                let (ch, env) = (&mut self.ch, ENV.contains(&name));
                let class = match name {
                    b"v" | b"(" | b")" | b"x" | b"%v" | b"%x" | b"@v" => 1,
                    b"p" | b"@p" => 2,
                    b"q" | b"@q" => 4,
                    b"@" | b"@al" | b"@fb" | b"@dt" | b"@er" | b"@fx" | b"@ml" | b"@ph" | b"@rr" | b"@se" | b"@tl" | b"i" | b"s" => 8,
                    _ if env => 16,
                    b"mp" | b"ma" => 32,
                    _ => 0,
                };
                match name {
                    _ if self.mask & class != 0 => {}
                    b"@mask" => self.mask = p(0, 0),
                    b"o" => self.oct = p(0, 5),
                    b"<" => self.oct += sh.rev.0 * p(0, 1),
                    b">" => self.oct -= sh.rev.0 * p(0, 1),
                    b"!@ns" => self.ns += p(0, 0),
                    b"k" => {
                        // Detune in 1/64 semitones; a fraction (`k-12.375`) is kept as a finer pitch.
                        let mut j = st + name.len();
                        let v = fnum(s, &mut j).unwrap_or(0.0);
                        *i = j.max(*i);
                        (self.k, self.kf) = (M::floor(v) as i32, v - M::floor(v));
                    }
                    b"!@kr" => self.k += p(0, 0),
                    b"kt" => self.kt = p(0, 0),
                    b"t" => {
                        self.tset = self.at;
                        *bpm = match sh.tmode { (1, r) => r / (256 - p(0, 0).clamp(0, 255)) as f64, (2, r) => r / p(0, 30).max(1) as f64, (_, r) => p(0, 120) as f64 * r }.clamp(1.0, 4000.0);
                    }
                    b"q" => self.q = p(0, sh.quant * 3 / 4) as f64 / sh.quant as f64,
                    b"@q" => (self.qc, self.kd) = (p(0, 0) as f64 * 10.0, p(1, 0) as f64 * 10.0),
                    b"v" | b"(" | b")" | b"x" => {
                        match name[0] {
                            b'v' => self.vel = p(0, 16) << self.vsh,
                            b'x' => self.x = p(0, 128).clamp(0, 128),
                            c => self.vel += (if c == b'(' { 1 } else { -1 }) * sh.rev.1 * (p(0, 1) << self.vsh),
                        }
                        self.vel = self.vel.clamp(0, 512);
                        ch.volume(self.x, self.vel);
                    }
                    b"%v" => { self.vsh = p(1, 4).clamp(0, 7); ch.vm.0 = p(0, 0); ch.volume(self.x, self.vel) }
                    b"%x" => { ch.vm.1 = p(0, 0); ch.volume(self.x, self.vel) }
                    b"@v" => for k in 0..8 { ch.vol[k] = if a[k] == MIN { (k == 0) as i32 as f32 * 0.5 } else { a[k] as f32 / 128.0 } },
                    b"p" => ch.pan = if a[0] == MIN { 64 } else { (a[0] * 16).clamp(0, 128) },
                    b"@p" => ch.pan = p(0, 0).clamp(-64, 64) + 64,
                    b"%f" => ch.f_type = if (0..3).contains(&a[0]) { a[0] as usize } else { 0 },
                    b"@f" => ch.filter(&[p(0, 128), p(1, 0), p(2, 0), p(3, 0), p(4, 0), p(5, 0), p(6, 128), p(7, 64), p(8, 32), p(9, 128)]),
                    b"@clock" => ch.fratio = p(0, 100).max(1) as f32 / 100.0,
                    b"@al" => { let cnt = p(0, 0).max(0) as usize; ch.set_alg(cnt, p(1, *[0, 1, 5, 7].get(cnt).unwrap_or(&0)).max(0) as usize) }
                    b"@fb" => (ch.fb, ch.fbc, ch.inp.0) = (p(0, 0), if (p(1, 0) as usize) < ch.ops.len() { p(1, 0) as usize } else { 0 }, 0),
                    b"@i" => ch.inp = (p(0, 5), p(1, 0) as usize & 3),
                    b"@o" => ch.outp = (p(0, 2), p(1, 0) as usize & 3),
                    b"@r" => ch.ring = (p(0, 4), p(1, 0) as usize & 3),
                    b"i" => ch.act = (p(0, 4).max(0) as usize).min(ch.ops.len() - 1),
                    b"@tl" => ch.ops[ch.act].tl = p(0, 0).clamp(0, 127),
                    b"@ml" => ch.ops[ch.act].fmul = (p(0, 0) << 7) + p(1, 0),
                    b"@dt" => ch.ops[ch.act].det = p(0, 0),
                    b"@ph" => {
                        // Key-on phase; on `%7`/`%10` the start frame, which may have a fraction (`@ph221.25`)
                        // and be negative (frames of silence first), then optionally how many frames to play
                        // from there before the sample stops (`@ph-16.5,100`; 0 or none: all of it).
                        let mut j = st + name.len();
                        let v = fnum(s, &mut j).unwrap_or(0.0);
                        let len = if s.get(j) == Some(&b',') { j += 1; fnum(s, &mut j).unwrap_or(0.0) } else { 0.0 };
                        *i = j.max(*i);
                        (ch.ops[ch.act].phase, ch.sfrac0, ch.slen) = (M::floor(v) as i32, v - M::floor(v), len.max(0.0));
                    }
                    b"@fx" => ch.ops[ch.act].fix = (p(0, 0) << 6) + p(1, 0),
                    b"@er" => for o in &mut ch.ops { o.erst = a[0] == 1 },
                    b"@se" => ch.ops[ch.act].ssg = match p(0, 0) { i @ 8..=17 => i, 18.. => 9, _ => 0 },
                    b"s" | b"@rr" => { if a[0] != MIN { ch.release_rate(a[0], name == b"s") } self.sweep[0] = p(1, 0); self.swe0 = if self.sweep[0] < 0 { 0.0 } else { 8191.0 } }
                    b"po" => self.sweep[1] = p(0, 0),
                    b"@fps" => self.itv = RATE / (if a[0] > 0 { a[0].min(1000) } else { 60 }) as f32,
                    b"@lfo" => {
                        let w = p(1, 2);
                        let tab = sh.tables.get(w.max(0) as usize).and_then(|t| t.as_ref()).filter(|_| (8..255).contains(&w));
                        ch.lfo_init(if (0..8).contains(&w) { w } else { 2 }, tab.map_or(vec![], |t| (0..256).map(|i| tval(t, i)).collect()));
                        ch.lfo_time(if a[0] == MIN { 333.0 } else { a[0] as f32 * 1000.0 / 60.0 });
                    }
                    b"mp" | b"ma" => {
                        let (k, d, e) = ((name == b"mp") as usize, p(0, 0), p(1, 0));
                        self.modt[k] = if (0 <= d && d < e) || (d < 0 && d > e) {
                            let term = p(3, 0).max(0);
                            Some((0..p(2, 0).max(0)).map(|_| d).chain((0..term).map(|j| d + (e - d) * j / term)).chain([e]).collect())
                        } else {
                            if k == 1 { ch.set_pm(d) } else { ch.set_am(d) }
                            None
                        };
                    }
                    _ if env => {
                        let (on, kind) = ((name[0] != b'_') as usize, match name[name.len() - 1] { b'@' => 1, b't' => 2, b'p' => 3, b'f' => 4, _ => 0 });
                        self.set[on][kind] = Some((a[0].max(0) as usize, p(1, 1))).filter(|(_, s)| (0..512).contains(&a[0]) && *s != 0);
                        if kind == 0 { self.xoff[on] = name[0] == b'!' }
                    }
                    b"%" => self.module(sh, p(0, 5), a[1]),
                    b"@" => {
                        let init = if a[0] != MIN { self.tone = a[0]; self.select(sh, a[0]) } else { None };
                        if self.ch.ks {
                            // Plucked string seed: `@1,n` excites with FM voice n, otherwise `@0,ar,dr,tl,fix,ws` sets a noise burst.
                            let voice = sh.voice(a[1].max(0) as usize).filter(|_| a[0] == 1);
                            if let Some(v) = voice { self.ch.set_voice(v) } else {
                                self.ch.params(&[a[1], a[2], 0, 63, 15, a[3], 0, 0, 1, 0, 0, 0, 0, a[4]]);
                                (self.ch.ops[0].pg, self.ch.ops[0].pt) = (p(5, 20), Op::pt_of(p(5, 20)));
                            }
                        } else {
                            self.ch.params(&a[1..15]);
                        }
                        let mut j = 0;
                        while let Some(seq) = init.filter(|q| j < q.len()) { self.exec(seq, &mut j, sh, bpm, false) }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// An effect slot: 0 is the master chain, 1..8 are sends (`@v` extra arguments).
struct Slot { fx: Vec<Fx>, vol: f32, pan: i32, mono: bool, buf: Vec<[f32; 2]> }

impl Default for Slot {
    fn default() -> Slot { Slot { fx: vec![], vol: 1.0, pan: 64, mono: false, buf: vec![] } }
}

pub struct Song {
    tracks: Vec<Track>, sh: Shared, slots: Vec<Slot>, bpm: f64, now: f64, tail: usize,
    /// Smooth over the jumps from key-ons and volume and pan changes (see `Declick`; on by default).
    declick: bool,
    /// Frames played (rendered or skipped) so far.
    pos: u64,
    /// Tick and tempo at the last tempo change, and frames since: `now` is computed from these rather than summed
    /// frame by frame, so rounding can't build up over a long song.
    base: (f64, f64, u64),
    /// The pipes (`@o`, `@i`, `@r`): each one's values over the last `PIPE_BLOCK` frames. SiON renders the tracks a
    /// block at a time, one after another, so a track reads what the tracks before it wrote in the same block, but
    /// from those after it the block before; and a pipe that no track overwrites keeps what it held, so `@o2`
    /// alone adds onto that. A ring of one block's frames does the same frame by frame.
    pipes: Vec<[f32; 4]>,
}

/// Frames in one of SiON's rendering blocks (its default buffer length): how far back a pipe written by a later
/// track is read.
const PIPE_BLOCK: usize = 2048;

/// Note number of letter `c` with accidental `nx`, octave, note shift and key signature.
fn key(c: u8, nx: Option<&u8>, oct: i32, ns: i32, sign: &[i32; 7]) -> i32 {
    let k = (c - b'a') as usize;
    [9, 11, 0, 2, 4, 5, 7][k] + oct * 12 + ns + match nx { Some(b'+' | b'#') => 1, Some(b'-') => -1, _ => 0 } + sign[k]
}

/// Resolve octave (`o < >`), note shift (`!@ns`) and default length (`l`) in text order, as SiON's parser does,
/// so repeats and `$` loops replay the same notes. Notes become `\x01<note>`, implicit lengths `\x02<ticks>`.
fn statics(s: &[u8], sh: &Shared) -> Vec<u8> {
    let (mut out, mut i, mut oct, mut ns, mut l) = (vec![], 0, 5, 0, 480);
    // Copy a length (number, dots, `^` ties), writing out implicit default lengths.
    let length = |i: &mut usize, out: &mut Vec<u8>, l: i32| loop {
        ws(s, i);
        let st = *i;
        if num(s, i).is_some() { out.extend(&s[st..*i]) } else { *i = st; cmd(out, b"\x02", &[l]) }
        let st = *i;
        ws(s, i);
        while s.get(*i) == Some(&b'.') { *i += 1 }
        ws(s, i);
        let tie = s.get(*i) == Some(&b'^');
        *i += tie as usize;
        out.extend(&s[st..*i]);
        if !tie { break }
    };
    while i < s.len() {
        let c = s[i];
        i += 1;
        match c {
            b'a'..=b'g' => {
                let n = key(c, s.get(i), oct, ns, &sh.sign).clamp(0, 127);
                if matches!(s.get(i), Some(b'+' | b'#' | b'-')) { i += 1 }
                cmd(&mut out, b"\x01", &[n]);
                out.push(b' ');
                length(&mut i, &mut out, l);
            }
            b'r' | b'^' => { out.push(c); length(&mut i, &mut out, l) }
            b'l' => l = len(s, &mut i, l),
            b'o' => oct = num(s, &mut i).unwrap_or(5),
            b'<' | b'>' => oct += num(s, &mut i).unwrap_or(1) * if c == b'<' { sh.rev.0 } else { -sh.rev.0 },
            b'{' => { let e = find(s, i, b"}"); out.push(c); out.extend(&s[i..e]); i = e }
            b'!' if s[i..].starts_with(b"@ns") => { i += 3; ns += num(s, &mut i).unwrap_or(0) }
            _ => {
                // Any other command: copy its name and numeric arguments unchanged.
                let st = i - 1;
                i = st + name_len(&s[st..]);
                loop {
                    let a = i;
                    if num(s, &mut i).is_none() { i = a }
                    let b = i;
                    ws(s, &mut i);
                    if s.get(i) == Some(&b',') { i += 1 } else { i = b; break }
                }
                out.extend(&s[st..i]);
            }
        }
    }
    out
}

/// Put `\x03<offset>` before each note letter in `s`, `at[k]` being where `s[k]` is in the source text, so
/// tracks can tell which note in the source is playing (`Song::notes`). Multi-character commands (`@f`, `ma`..)
/// and `{..}` are passed over as `statics` would.
fn spans(s: &[u8], at: &[usize]) -> Vec<u8> {
    let (mut out, mut i) = (Vec::with_capacity(s.len() * 2), 0);
    while i < s.len() {
        let n = match s[i] {
            b'{' => find(s, i, b"}") - i,
            b'a'..=b'g' => { cmd(&mut out, b"\x03", &[at[i] as i32]); 1 }
            _ => name_len(&s[i..]),
        };
        out.extend(&s[i..i + n]);
        i += n;
    }
    out
}

/// Replace macro references (`A`, `A(3)` = transposed) with their bodies.
fn expand(mac: &[Vec<u8>], s: &[u8], deep: u8) -> Vec<u8> {
    let (mut out, mut i) = (vec![], 0);
    while i < s.len() {
        let c = s[i];
        i += 1;
        if !c.is_ascii_uppercase() { out.push(c); continue }
        let (mut t, m) = (0, &mac[(c - b'A') as usize]);
        if s.get(i) == Some(&b'(') {
            let mut j = i + 1;
            if let (Some(n), Some(b')')) = (num(s, &mut j), s.get(j)) { t = n; i = j + 1 }
        }
        // Only a transposed macro is wrapped in note shifts: the closing one would otherwise swallow a length
        // written right after the macro (`Z16..` would read as `!@ns016` and lose its `16..`).
        if t != 0 { cmd(&mut out, b"!@ns", &[t]) }
        if deep > 0 && deep < 9 { out.extend(expand(mac, m, deep + 1)) } else { out.extend(m) }
        if t != 0 { cmd(&mut out, b"!@ns", &[-t]) }
    }
    out
}

/// Append MML command `name` with integer arguments, e.g. `@o1,0`.
fn cmd(out: &mut Vec<u8>, name: &[u8], args: &[i32]) {
    out.extend(name);
    for (k, &t) in args.iter().enumerate() {
        if k > 0 { out.push(b',') }
        if t < 0 { out.push(b'-') }
        let (mut d, st) = (t.unsigned_abs(), out.len());
        loop { out.insert(st, b'0' + (d % 10) as u8); d /= 10; if d == 0 { break } }
    }
}

/// `#FM{formula}`, e.g. `B3(A)`: letters name the sequences that follow (`a` = first), `(..)` lists the
/// modulators of the preceding letter, a digit sets its modulation level. Returns the reordered sequences
/// (modulators before carriers, wired through pipes), replacing the first `n` that follow.
fn fm(form: &[u8], seqs: &[Vec<u8>]) -> Option<(usize, Vec<Vec<u8>>)> {
    // Elements: (sequence, modulation, first child, next sibling, parent).
    let (mut el, mut prev, mut i) = (Vec::<(usize, i32, Option<usize>, Option<usize>, Option<usize>)>::new(), None::<usize>, 0);
    while i < form.len() {
        let open = form[i] == b'(';
        let j = i + open as usize;
        let Some(c) = form.get(j).filter(|c| c.is_ascii_alphabetic()) else { i += 1; continue };
        let mut k = j + 1;
        let m = form.get(k).filter(|d| (b'0'..=b'7').contains(d)).map_or(5, |d| { k += 1; (d - b'0') as i32 });
        let e = el.len();
        let parent = if open { let p = prev?; el[p].2 = Some(e); Some(p) } else { if let Some(p) = prev { el[p].3 = Some(e) } prev.and_then(|p| el[p].4) };
        el.push(((c.to_ascii_lowercase() - b'a') as usize, m, None, None, parent));
        let mut cur = e;
        while form.get(k) == Some(&b')') { cur = el[cur].4?; k += 1 }
        (prev, i) = (Some(cur), k);
    }
    if el[prev?].4.is_some() { return None }
    let n = el.iter().map(|e| e.0 + 1).max()?;
    fn walk(el: &[(usize, i32, Option<usize>, Option<usize>, Option<usize>)], e: usize, first: bool, outp: i32, seqs: &[Vec<u8>], out: &mut Vec<Vec<u8>>) {
        let (num, m, child, next, _) = el[e];
        let inp = outp + !first as i32;
        if let Some(c) = child { walk(el, c, true, inp, seqs, out) }
        let mut s = vec![];
        cmd(&mut s, b"@o", &if outp < 0 { [0, 0] } else { [if first { 1 } else { 2 }, outp] });
        cmd(&mut s, b"@i", &if child.is_some() { [m, inp] } else { [0, 0] });
        s.extend(&seqs[num]);
        out.push(s);
        if let Some(x) = next { walk(el, x, false, outp, seqs, out) }
    }
    let mut out = vec![];
    walk(&el, 0, false, -1, seqs.get(..n)?, &mut out);
    Some((n, out))
}

/// Replace inline envelope tables (`na{0,8,16}`) with references to new tables numbered down from 511.
fn inline(s: &[u8], tables: &mut [Option<Table>], next: &mut usize) -> Vec<u8> {
    let (mut out, mut i) = (vec![], 0);
    while i < s.len() {
        let name = ENV.iter().find(|c| s[i..].starts_with(c));
        let Some(name) = name else { out.push(s[i]); i += 1; continue };
        let mut j = i + name.len();
        out.extend(*name);
        ws(s, &mut j);
        if s.get(j) == Some(&b'{') && *next > 255 {
            let e = find(s, j, b"}");
            let mut k = e;
            while s.get(k).is_some_and(|c| c.is_ascii_digit() || matches!(c, b'*' | b'+' | b'-' | b'.')) { k += 1 }
            tables[*next] = Some(table(&s[j + 1..(e - 1).max(j + 1)], &s[e..k])).filter(|t| !t.0.is_empty());
            cmd(&mut out, b"", &[*next as i32]);
            *next -= 1;
            j = k;
        }
        i = j;
    }
    out
}

impl Song {
    /// Set PCM sample `n` (0..255), replacing any `#PCMB` of that number (and the parts of it that `#PCMWAVE`
    /// and `#SAMPLER` cut out).
    pub fn set_sample(&mut self, n: usize, s: Sample) {
        if n < 256 { self.sh.samples[n] = Some(s); self.sh.dpcm[n] = false; cuts(&mut self.sh, Some(n)) }
    }

    pub fn new(mml: &str) -> Song { Song::from_bytes(mml.as_bytes()) }

    /// Turn declicking on (the default) or off. Off, the output is exactly SiON's, jumps and all.
    pub fn set_declick(&mut self, on: bool) { self.declick = on; self.declick_reset() }

    /// Forget what declicking took note of: nothing from before now is smoothed over.
    fn declick_reset(&mut self) { for t in &mut self.tracks { (t.dc, t.ch.vj) = (Declick::new(t.ch.out_pan()), 1.0) } }

    /// `new` for MML as bytes (UTF-8, though only the ASCII in it matters), so no text conversion is needed.
    pub fn from_bytes(src: &[u8]) -> Song {
        // Strip comments.
        // (`at`: where each byte of `text` was in `src`, for `spans`.)
        let (mut text, mut i, mut at) = (vec![], 0, vec![]);
        while i < src.len() {
            if src[i..].starts_with(b"/*") { i = find(src, i + 2, b"*/") }
            else if src[i..].starts_with(b"//") { i = find(src, i, b"\n") }
            else { text.push(src[i]); if cfg!(feature = "spans") { at.push(i) } i += 1 }
        }
        text.push(b';');
        // Mark the notes in sequences and macro bodies with their place in `src` (with the `spans` feature).
        let mark = |s: &[u8]| -> Vec<u8> {
            if !cfg!(feature = "spans") { return s.to_vec() }
            let base = s.as_ptr() as usize - text.as_ptr() as usize;
            spans(s, &at[base..])
        };

        let mut sh = Shared {
            samples: vec![None; 256], cuts: vec![], pcmv: vec![None; 256], kit: vec![None; 4], dpcm: vec![false; 256], waves: vec![vec![]; 256], noise: noise(), tables: vec![None; 512], voices: vec![None; 256],
            fps: 60.0, quant: 8, tmode: (0, 1.0), vmode: (0, 0, 4), rev: (1, 1), fb2: false, sign: [0; 7],
            sine: lookup(|i| M::sin(i * TAU / 1024.0)), pan: lookup(|i| M::sin(i * PI / 256.0)), att: lookup(|i| M::exp2(-(i / 4.0 - 256.0) / 64.0)),
        };

        // Split into `;`-terminated statements: macro definitions, system commands, sequences.
        let (mut mac, mut dynamic, mut seqs, mut effects, mut conn) = (vec![vec![]; 26], false, vec![], vec![], vec![]);
        // Each DPCM `#PCMB`'s bytes, for `{=m}` references.
        let mut raw: Vec<(usize, Vec<u8>)> = vec![];
        let (mut st, mut brace) = (0, false);
        for e in 0..text.len() {
            match text[e] { b'{' => brace = true, b'}' => brace = false, _ => {} }
            if brace || text[e] != b';' { continue }
            let s = text[st..e].trim_ascii_start();
            st = e + 1;
            if s.first() != Some(&b'#') { seqs.push(expand(&mac, &mark(s), dynamic as u8)); continue }
            let n = 1 + s[1..].iter().take_while(|c| c.is_ascii_uppercase() || matches!(c, b'@' | b'-')).count();
            let (name, body) = (&s[1..n], &s[n..]);
            let add = body.starts_with(b"+=");
            if add || body.starts_with(b"=") {
                // `#A=`, `#ABC=`, `#A-C=`: (re)define or append to macros. Static macros expand at definition.
                let x = mark(&body[1 + add as usize..]);
                let x = if dynamic { x } else { expand(&mac, &x, 0) };
                let mut j = 0;
                while j < name.len() {
                    let letter = |j: &mut usize| name.get(*j).filter(|c| c.is_ascii_uppercase()).map(|c| { *j += 1; (c - b'A') as usize });
                    let a = letter(&mut j);
                    let b = if name.get(j) == Some(&b'-') { j += 1; letter(&mut j).unwrap_or(25) } else { a.unwrap_or(0) };
                    if a.is_none() && name.get(j) == Some(&b'@') { j += 1; continue }
                    for m in mac[a.unwrap_or(0)..=b.max(a.unwrap_or(0))].iter_mut() { if !add { m.clear() } m.extend(&x) }
                }
                continue;
            }
            let mut j = 0;
            let k = num(body, &mut j).unwrap_or(0).max(0) as usize;
            ws(body, &mut j);
            let (dat, pfx) = if body.get(j) == Some(&b'{') { let e = find(body, j, b"}"); (&body[j + 1..(e - 1).max(j + 1)], &body[e..]) } else { (&body[j..], &body[j..]) };
            let text = dat;
            match name {
                b"END" => break,
                b"@" | b"OPM@" | b"OPN@" | b"OPL@" | b"OPX@" | b"MA@" | b"AL@" => sh.voices[k & 255] = voice(name, &nums(dat), pfx),
                b"FPS" => sh.fps = if k > 0 { k as f32 } else { nums(dat).first().copied().unwrap_or(60.0).max(1.0) },
                b"SIGN" => sh.sign = sign(text),
                b"MACRO" => dynamic = has(text, b"dynamic"),
                b"QUANT" => if k > 0 { sh.quant = k as i32 },
                b"TMODE" => {
                    let v = nums(&dat[find(dat, 0, b"=")..]).first().copied().filter(|v| *v > 0.0);
                    sh.tmode = if has(text, b"timerb") { (1, v.unwrap_or(4000.0) as f64 * 1.220703125) } else if has(text, b"fps") { (2, v.unwrap_or(60.0) as f64 * 60.0) } else { (0, 1.0 / v.unwrap_or(1.0) as f64) };
                }
                b"VMODE" => {
                    if has(text, b"n88") || has(text, b"mdx") { (sh.vmode.0, sh.vmode.1) = (4, 3) }
                    if has(text, b"psg") { (sh.vmode.0, sh.vmode.1) = (3, 3) }
                    for (key, x) in [(b"%v", false), (b"%x", true)] {
                        if let Some(p) = text.windows(2).position(|w| w == key) {
                            let (mut j, d) = (p + 2, text);
                            let m = num(d, &mut j).unwrap_or(0).clamp(0, 4);
                            if x { sh.vmode.1 = m } else { sh.vmode.0 = m; ws(d, &mut j); if d.get(j) == Some(&b',') { j += 1; sh.vmode.2 = num(d, &mut j).unwrap_or(4).clamp(0, 7) } }
                        }
                    }
                }
                b"FBMODE" => sh.fb2 = has(text, b"chip"),
                b"REV" => sh.rev = (if has(text, b"vol") { 1 } else { -1 }, if has(text, b"oct") { 1 } else { -1 }),
                b"TABLE" => if k < 255 { sh.tables[k] = Some(table(dat, pfx)).filter(|t| !t.0.is_empty()) },
                b"WAV" => {
                    let t = table(dat, pfx).0;
                    let n = t.len().clamp(2, 1024).next_power_of_two();
                    sh.waves[k & 255] = (0..n).map(|i| t.get(i).map_or(0.0, |v| ((*v as f32 + 0.5) / 128.0).clamp(-1.0, 1.0))).collect();
                }
                b"WAVB" => {
                    let hex: Vec<u32> = dat.iter().filter_map(|c| (*c as char).to_digit(16)).collect();
                    sh.waves[k & 255] = hex.chunks_exact(2).map(|p| (p[0] * 16 + p[1]) as u8 as i8 as f32 / 128.0).collect();
                }
                b"WAVCOLOR" | b"WAVC" => {
                    let hex = dat.strip_prefix(b"0x").or_else(|| dat.strip_prefix(b"0X")).unwrap_or(dat);
                    let c = hex.iter().filter_map(|c| (*c as char).to_digit(16)).fold(0u32, |a, d| a << 4 | d);
                    sh.waves[k & 255] = wave_color(&sh, c);
                }
                b"PCMB" => {
                    let bytes = match dat.strip_prefix(b"=") {
                        Some(m) => { let mut j = 0; let m = num(m, &mut j).unwrap_or(-1); raw.iter().rev().find(|r| r.0 as i32 == m).map_or(vec![], |r| r.1.clone()) }
                        None => unb64(dat),
                    };
                    let a = nums(pfx);
                    sh.samples[k & 255] = Some(pcm(&bytes, &a));
                    sh.dpcm[k & 255] = a.get(3).is_some_and(|f| *f as i32 & 4 != 0);
                    if sh.dpcm[k & 255] { raw.push((k & 255, bytes)) }
                }
                b"PCMWAVE" => pcm_table(&mut sh, b'W', k, dat, pfx),
                b"PCMVOICE" => pcm_table(&mut sh, b'V', k, dat, pfx),
                b"SAMPLER" => pcm_table(&mut sh, b'S', k, dat, pfx),
                b"EFFECT" => effects.push((k, dat.to_vec(), pfx.to_vec())),
                b"FM" => conn.push((seqs.len(), dat.to_vec())),
                _ => {}
            }
        }

        cuts(&mut sh, None);

        // Effect chains: `name arg,arg,...` units in series, then `p`/`@p`/`@v` for the slot's return.
        let mut slots: Vec<Slot> = (0..8).map(|_| Slot::default()).collect();
        for (k, dat, pfx) in effects {
            let Some(slot) = slots.get_mut(k) else { continue };
            *slot = Slot::default();
            for (post, src) in [(false, &dat), (true, &pfx)] {
                let mut i = 0;
                while i < src.len() {
                    let st = i;
                    while src.get(i).is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'_' | b'@')) { i += 1 }
                    if i == st { i += 1; continue }
                    let (name, mut a, mut n) = (&src[st..i], [f32::NAN; 16], 0);
                    loop {
                        a[n] = fnum(src, &mut i).unwrap_or(f32::NAN);
                        n += 1;
                        ws(src, &mut i);
                        if n < 16 && src.get(i) == Some(&b',') { i += 1 } else { break }
                    }
                    match (post, name) {
                        (false, name) => slot.fx.extend(Fx::new(name, &a, &sh.tables)),
                        // (Without a value the return stays as it is: NaN would silence the mix, or pan it hard left.)
                        _ if a[0].is_nan() => {}
                        (_, b"p") => slot.pan = (a[0] as i32 * 16).clamp(0, 128),
                        (_, b"@p") => slot.pan = (a[0] as i32).clamp(-64, 64) + 64,
                        (_, b"@v") => slot.vol = (a[0] / 128.0).clamp(0.0, 1.0),
                        _ => {}
                    }
                }
            }
            slot.mono = slot.fx.first().is_some_and(Fx::mono);
        }
        for (at, form) in conn.into_iter().rev() {
            if let Some((n, new)) = fm(&form, &seqs[at.min(seqs.len())..]) { seqs.splice(at..at + n, new); }
        }
        let mut next = 511;
        let tracks = seqs.into_iter().map(|s| Track::new(statics(&inline(&s, &mut sh.tables, &mut next), &sh), &sh)).collect();
        Song { tracks, sh, slots, bpm: 120.0, now: 0.0, tail: 0, declick: true, pos: 0, base: (0.0, 120.0, 0), pipes: vec![[0.0; 4]; PIPE_BLOCK] }
    }

    /// Per track, the byte offset in the source MML of the note it is playing now (-1: none, resting or ended),
    /// for following playback in the text. Only with the `spans` feature; otherwise always -1.
    pub fn notes(&self) -> impl Iterator<Item = i32> + '_ { self.tracks.iter().map(|t| t.src) }

    /// Frames played so far (rendered or skipped): the playback position, in `RATE`ths of a second.
    pub fn position(&self) -> u64 { self.pos }

    /// Length of one pass through the MML in frames, without rendering any audio: until every track has
    /// reached its end, or jumped back to its `$` loop point, at least once. Capped at one hour.
    pub fn length(mml: &str) -> u64 { Song::length_bytes(mml.as_bytes()) }

    /// `length` for MML as bytes (see `from_bytes`).
    pub fn length_bytes(mml: &[u8]) -> u64 { Song::pass(mml).0.pos }

    /// Where a song with a `$` loop carries on from once it has played `length` frames: the frame of the
    /// loop point of the track that finished its first pass last (the one `length` measures; if tracks put
    /// their `$` at different times, the others drift against it). None if the song ends instead.
    pub fn loop_start(mml: &str) -> Option<u64> { Song::loop_start_bytes(mml.as_bytes()) }

    /// `loop_start` for MML as bytes (see `from_bytes`).
    pub fn loop_start_bytes(mml: &[u8]) -> Option<u64> {
        let (s, marks) = Song::pass(mml);
        s.tracks.iter().zip(marks).filter(|(t, _)| !t.end).filter_map(|(_, m)| m).max().map(|(_, at)| at)
    }

    /// Sequence one pass through the MML (see `length`), without rendering. Also gives, per track that has
    /// a `$` loop point, (frame its first pass ended, frame it first reached `$`).
    fn pass(mml: &[u8]) -> (Song, Vec<Option<(u64, u64)>>) {
        let mut s = Song::from_bytes(mml);
        let (mut marks, mut loops) = (vec![None; s.tracks.len()], vec![None; s.tracks.len()]);
        while s.pos < 3600 * RATE as u64 && !s.tracks.iter().all(|t| t.once) {
            let at = s.pos;
            s.sequence(3600 * RATE as u64, false, false);
            for ((m, l), t) in marks.iter_mut().zip(&mut loops).zip(&s.tracks) {
                if m.is_none() && t.rep.is_some() { *m = Some(at) }
                if let (None, true, Some(m)) = (*l, t.once, *m) { *l = Some((at, m)) }
            }
        }
        (s, loops)
    }

    /// Move playback `frames` ahead without producing output, for seeking. The sequencer runs (notes, tempo,
    /// envelope tables), leaping straight from one event to the next, and each channel's slow-moving state
    /// (envelopes, PCM sample positions, LFO, filter sweep, oscillator phases) leaps along with it. Plucked
    /// strings (`%11`) are synthesized in full for the last two seconds, and the last moment (a quarter
    /// second with effect units, else a twentieth) is rendered in full, so feedback, filters and effects
    /// (reverb, delay..) settle. The result is close to, though not exactly, what continuous playback would
    /// give there: echoes of what played before that last moment are missing, and the phases of oscillators
    /// with vibrato drift. Returns false once the song has finished, as `render` does.
    /// To seek backwards, make a new `Song` and skip forward from the start.
    pub fn skip(&mut self, frames: u64) -> bool {
        let end = self.pos + frames;
        let at = |s: f32| end.saturating_sub((s * RATE) as u64);
        // Effects need longer to build up (delay lines fill, reverb rings in) than anything else does.
        let fx = self.slots.iter().any(|s| !s.fx.is_empty());
        let (strings, tail) = (at(2.0), at(if fx { 0.25 } else { 0.05 }));
        for t in &mut self.tracks { (t.lag, t.seen, t.next) = (0, self.pos, self.pos) }
        while self.pos < strings { self.sequence(strings, true, false) }
        for t in self.tracks.iter_mut().filter(|t| t.ch.ks) { t.next = t.next.min(self.pos) }
        while self.pos < tail { self.sequence(tail, true, true) }
        self.catch_up();
        // (Nothing to smooth over from before the skip.)
        self.declick_reset();
        let (mut buf, mut playing) = ([0f32; 512], self.tail < RATE as usize / 2);
        while self.pos < end { playing = self.render(&mut buf[..2 * (end - self.pos).min(256) as usize]) }
        playing
    }

    /// Sequence on towards frame `end` without rendering: leap to the next frame some track has something
    /// to do in (see `Track::next`) and run those tracks for it. With `synth`, leap the channels along
    /// too (see `skip`), sampling them at those frames, and every frame for plucked strings with `strings`.
    fn sequence(&mut self, end: u64, synth: bool, strings: bool) {
        let (pos, bpm) = (self.pos, self.bpm);
        let next = self.tracks.iter().map(|t| t.next).fold(end, u64::min);
        if next > pos {
            self.advance(next - pos);
        } else {
            let mut pipes = [0f32; 4];
            for t in self.tracks.iter_mut().filter(|t| t.next <= pos) {
                t.catch(&self.sh, pos, synth);
                // Leap a channel along just before what changes how it leaps: commands, key on/off, and
                // envelope frames that do (see `Track::ticks`).
                let due = self.now + 1e-6;
                let cmd = t.ticks() || (!t.end && due >= t.at) || due >= t.kon || due >= t.koff;
                if synth && cmd { t.ch.leap(&self.sh, core::mem::take(&mut t.lag)) }
                t.run(&self.sh, self.now, &mut self.bpm);
                // Strings are synthesized; other channels pick up what the commands changed and leap on.
                if synth && strings && t.ch.ks { t.ch.sample(&self.sh, &mut pipes); } else if synth {
                    let late = core::mem::take(&mut t.ch.slate);
                    if cmd && t.ch.stale { t.ch.refresh(&self.sh, late) }
                    t.lag += 1;
                }
                t.seen = pos + 1;
            }
            self.advance(1);
            let step = self.bpm * 8.0 / RATE as f64;
            for t in self.tracks.iter_mut().filter(|t| t.seen > pos || self.bpm != bpm) {
                t.next = if strings && t.ch.ks { self.pos } else { t.next(self.pos, self.now, step, synth && t.ticks()) };
            }
        }
        if !synth { return }
        let ended = self.tracks.iter().all(Track::done) && { self.catch_up(); self.tracks.iter().all(|t| t.ch.silent()) };
        self.tail = if ended { self.tail + (self.pos - pos) as usize } else { 0 };
    }

    /// Bring every track and channel up to the current frame (see `sequence`).
    fn catch_up(&mut self) {
        for t in &mut self.tracks {
            t.catch(&self.sh, self.pos, true);
            t.ch.leap(&self.sh, core::mem::take(&mut t.lag));
        }
    }

    /// Step the clock `n` frames, after the tracks have run (only the first can have changed the tempo).
    fn advance(&mut self, n: u64) {
        if self.bpm != self.base.1 {
            // The tempo changed partway into this frame: count the part after the change at the new tempo.
            let at = self.tracks.iter().map(|t| t.tset).fold(self.base.0, f64::max).min(self.now);
            self.base = (at + (self.now - at) * self.bpm / self.base.1, self.bpm, 0);
        }
        self.base.2 += n;
        self.pos += n;
        self.now = self.base.0 + self.base.2 as f64 * self.bpm * 8.0 / RATE as f64;
    }

    /// Fill `out` with interleaved stereo samples; returns false once the song has finished.
    /// Songs with a `$` loop point never finish.
    #[cfg_attr(feature = "small", optimize(speed))]
    pub fn render(&mut self, out: &mut [f32]) -> bool {
        let n = out.len() / 2;
        for s in &mut self.slots { s.buf.clear(); s.buf.resize(n, [0.0; 2]) }
        for i in 0..n {
            let at = (self.pos % PIPE_BLOCK as u64) as usize;
            let mut pipes = self.pipes[at];
            for t in &mut self.tracks {
                t.run(&self.sh, self.now, &mut self.bpm);
                let v = t.ch.sample(&self.sh, &mut pipes) * 0.5;
                // (Silent, and silent before: nothing to carry over; changes meanwhile wait for the next sound.)
                if v == 0.0 && t.dc.v == 0.0 && !t.dc.fading() { continue }
                let pan = t.ch.out_pan();
                let lr = |p: i32| [self.sh.pan[(128 - p) as usize], self.sh.pan[p as usize]];
                let o = if !self.declick || t.dc.calm(pan, t.ch.vj) {
                    t.dc.v = v;
                    if v == 0.0 { continue }
                    let [l, r] = lr(pan);
                    [v * l, v * r, v]
                } else {
                    t.dc.run(v, pan, lr(pan), lr(t.dc.pan), core::mem::replace(&mut t.ch.vj, 1.0))
                };
                let sends = if t.ch.vol[1..].iter().any(|v| *v > 0.0) { 8 } else { 1 };
                for (k, s) in self.slots.iter_mut().enumerate().take(sends) {
                    let g = t.ch.vol[k];
                    if g != 0.0 && (k == 0 || !s.fx.is_empty()) {
                        s.buf[i][0] += g * if s.mono { o[2] } else { o[0] };
                        s.buf[i][1] += g * if s.mono { o[2] } else { o[1] };
                    }
                }
            }
            self.pipes[at] = pipes;
            self.advance(1);
        }
        let (master, sends) = self.slots.split_first_mut().unwrap();
        for s in sends.iter_mut().filter(|s| !s.fx.is_empty()) {
            for f in &mut s.fx { f.run(&mut s.buf) }
            let direct = s.vol == 1.0 && s.pan == 64;
            let (l, r) = if direct { (1.0, 1.0) } else { (self.sh.pan[(128 - s.pan) as usize] * s.vol, self.sh.pan[s.pan as usize] * s.vol) };
            for (m, f) in master.buf.iter_mut().zip(&s.buf) { m[0] += f[0] * l; m[1] += f[1] * r }
        }
        for f in &mut master.fx { f.run(&mut master.buf) }
        for (o, f) in out.chunks_exact_mut(2).zip(&master.buf) { o[0] = f[0].clamp(-1.0, 1.0); o[1] = f[1].clamp(-1.0, 1.0) }
        // Finished once every track has ended and the output has been quiet for a while.
        let quiet = master.buf.iter().all(|f| f[0].abs() + f[1].abs() < 1e-4);
        self.tail = if self.tracks.iter().all(Track::done) && quiet { self.tail + n } else { 0 };
        self.tail < RATE as usize / 2
    }
}
