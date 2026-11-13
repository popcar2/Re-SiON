//! Tracker playback: performs a `Module` tick by tick (rows, speed and tempo, the effects, instrument
//! envelopes and fadeout, IT new-note actions) and records each channel's sound per tick, which `Stream`
//! turns into notes. One tick is one score tick; a quarter note is four rows at the song's initial speed.

use crate::sample::{analyze, as_sion, pcm, Sound};
use crate::score::{Defs, Frame, Score, Stream};
use crate::tracker::*;

/// A sounding note: on its channel, or carried on in the background by an IT new-note action.
#[derive(Clone)]
struct Voice {
    smp: usize,
    ins: usize,
    pos: f64,
    on: bool,
    fade: f32,
    fading: bool,
    venv: u32,
    penv: u32,
    trig: bool,
    /// Background voices keep the pitch (Hz) and level they had.
    rate: f64,
    vol: f32,
    pan: i32,
}

#[derive(Default)]
struct Chan {
    voice: Option<Voice>,
    ins: usize,
    /// Pitch: an Amiga period, or (linear slides) 1/64 semitones above note 0.
    p: f64,
    target: f64,
    note: u8,
    vol: f32,
    chvol: f32,
    pan: i32,
    mem: [u8; 16],
    vib: (u8, u8, u8, u8),
    trem: (u8, u8, u8, u8),
    tremor: (u8, bool),
    retrig: u8,
    vib_now: f64,
    trem_now: f32,
    arp: Option<u8>,
    tremor_off: bool,
    nna: Option<u8>,
    loop_row: usize,
    loop_n: u8,
    offset_hi: u32,
    finetune: i32,
}

/// Effect memory slots.
const M_VOL: usize = 0;
const M_PORTA: usize = 1;
const M_PORTA_DOWN: usize = 2;
const M_TONE: usize = 3;
const M_VIB: usize = 4;
const M_FINE_UP: usize = 5;
const M_FINE_DOWN: usize = 6;
const M_XFINE_UP: usize = 7;
const M_XFINE_DOWN: usize = 8;
const M_FVOL_UP: usize = 9;
const M_FVOL_DOWN: usize = 10;
const M_OFFSET: usize = 11;
const M_RETRIG: usize = 12;
const M_GVOL: usize = 13;
const M_PAN: usize = 14;
const M_OTHER: usize = 15;

fn sine(pos: u8, wave: u8) -> f64 {
    let p = (pos & 63) as f64 / 64.0;
    match wave & 3 {
        1 => 1.0 - 2.0 * p,
        2 => if p < 0.5 { 1.0 } else { -1.0 },
        _ => (p * std::f64::consts::TAU).sin(),
    }
}

struct Player<'a> {
    m: &'a Module,
    sounds: Vec<Sound>,
    ch: Vec<Chan>,
    bg: Vec<Option<Voice>>,
    gvol: f32,
    speed: u32,
    tempo: u32,
    /// Amiga clock: rate = clock / period.
    clock: f64,
}

impl Player<'_> {
    fn sample_of(&self, ins: usize, note: u8) -> Option<(usize, u8)> {
        let i = self.m.instruments.get(ins.checked_sub(1)?)?;
        let (n, s) = *i.map.get(note as usize)?;
        let s = (s as usize).checked_sub(1)?;
        (s < self.m.samples.len()).then_some((s, (n as i32 + self.m.note_ofs).clamp(0, 119) as u8))
    }

    /// Pitch value for `note` on sample `s`.
    fn pitch_of(&self, s: usize, note: u8, finetune: i32) -> f64 {
        let c4 = self.m.samples[s].c4 * 2f64.powf(finetune as f64 / 96.0);
        if self.m.linear {
            (note as f64 + 12.0 * (c4 / 8363.0).log2()) * 64.0
        } else {
            self.clock / c4 * 2f64.powf(-(note as f64 - 48.0) / 12.0)
        }
    }

    /// Playback rate (Hz) for pitch value `p`, `semis` semitones up (arpeggio) and `bend` Amiga units up (vibrato).
    fn rate(&self, p: f64, semis: f64, bend: f64) -> f64 {
        if self.m.linear {
            8363.0 * 2f64.powf(((p + 4.0 * bend) / 64.0 + semis - 48.0) / 12.0)
        } else {
            self.clock / (p - bend).max(1.0) * 2f64.powf(semis / 12.0)
        }
    }

    /// Slide the pitch up by `u` Amiga period units (negative: down).
    fn slide(&self, c: &mut Chan, u: f64) {
        if self.m.linear { c.p += 4.0 * u } else {
            c.p -= u;
            if self.m.kind == Kind::Mod { c.p = c.p.clamp(113.0, 856.0 * 2.0) } else { c.p = c.p.max(1.0) }
        }
    }

    /// Effect parameter with memory: 0 recalls the last nonzero one in `slot` (S3M shares one memory).
    fn mem(&self, c: &mut Chan, slot: usize, p: u8) -> u8 {
        let slot = if self.m.kind == Kind::S3m && ![M_TONE, M_VIB, M_OFFSET, M_GVOL, M_PAN].contains(&slot) { M_OTHER } else { slot };
        let slot = if self.m.kind == Kind::It && slot == M_TONE && !self.m.compat_g { M_PORTA } else { slot };
        if p != 0 { c.mem[slot] = p }
        c.mem[slot]
    }

    fn start(&mut self, ci: usize, note: u8, porta: bool) {
        let ins = self.ch[ci].ins;
        let Some((s, n)) = self.sample_of(ins, note) else { return };
        if self.sounds[s].silent() { self.ch[ci].voice = None; return }
        let p = self.pitch_of(s, n, self.ch[ci].finetune);
        let old_rate = self.rate(self.ch[ci].p, 0.0, 0.0);
        let c = &mut self.ch[ci];
        c.note = n;
        if porta && c.voice.is_some() { c.target = p; return }
        // New-note action: the old note carries on in the background.
        let nna = c.nna.unwrap_or_else(|| self.m.instruments.get(c.voice.as_ref().map_or(0, |v| v.ins).wrapping_sub(1)).map_or(0, |i| i.nna));
        if let Some(mut old) = c.voice.take().filter(|_| self.m.kind == Kind::It && nna != 0) {
            let i = &self.m.instruments[old.ins - 1];
            let sm = &self.m.samples[old.smp];
            let rings = i.venv.is_some() || i.fadeout > 0 || sm.lp.is_none();
            if rings {
                // On its background channel it starts a note of its own (from where it has got to).
                (old.rate, old.trig) = (old_rate, true);
                (old.vol, old.pan) = (c.vol * c.chvol / 64.0, c.pan);
                match nna { 2 => old.on = false, 3 => old.fading = true, _ => {} }
                if !old.on && i.venv.is_none() { old.fading = true }
                match self.bg.iter_mut().position(|b| b.is_none()) {
                    Some(k) => self.bg[k] = Some(old),
                    None if self.bg.len() < 32 => self.bg.push(Some(old)),
                    None => {}
                }
            }
        }
        let c = &mut self.ch[ci];
        c.p = p;
        c.target = p;
        if c.vib.3 & 4 == 0 { c.vib.2 = 0 }
        c.voice = Some(Voice { smp: s, ins, pos: 0.0, on: true, fade: 1.0, fading: false, venv: 0, penv: 0, trig: true, rate: 0.0, vol: 0.0, pan: 0 });
    }

    /// Apply an instrument number: default volume and panning.
    fn instrument(&mut self, ci: usize, ins: usize, note: u8) {
        self.ch[ci].ins = ins;
        let n = if (1..=120).contains(&note) { note - 1 } else { self.ch[ci].note };
        let Some((s, _)) = self.sample_of(ins, n) else { return };
        let (sm, i) = (&self.m.samples[s], &self.m.instruments[ins - 1]);
        let c = &mut self.ch[ci];
        c.vol = sm.vol as f32;
        if let Some(p) = i.pan.or(sm.pan) { c.pan = p }
        if self.m.kind == Kind::Mod || self.m.kind == Kind::Xm { c.finetune = 0 }
        if let Some(v) = &mut c.voice { if note == 0 { v.fade = 1.0; v.fading = false; v.on = true; v.venv = 0; v.penv = 0 } }
    }

    fn key_off(&mut self, ci: usize) {
        let c = &mut self.ch[ci];
        let Some(v) = &mut c.voice else { return };
        v.on = false;
        let i = &self.m.instruments[v.ins - 1];
        if i.venv.is_none() { if self.m.kind == Kind::Xm { c.vol = 0.0 } else { v.fading = true } }
        if self.m.kind == Kind::Xm && i.venv.is_some() { v.fading = true }
    }

    /// Tick 0 of a row for one channel.
    fn row(&mut self, ci: usize, cell: &Cell, jump: &mut Option<(Option<usize>, Option<usize>)>, delay: &mut u32, fine: &mut u32) {
        let porta = matches!(cell.fx, Fx::TonePorta(_) | Fx::TonePortaVol(_)) || matches!(cell.vol, Vol::TonePorta(_));
        if cell.ins > 0 { self.instrument(ci, cell.ins as usize, cell.note) }
        match cell.note {
            1..=120 => {
                if let Fx::Offset(_) = cell.fx {} else { self.ch[ci].offset_hi = 0 }
                self.start(ci, cell.note - 1, porta);
            }
            OFF => self.key_off(ci),
            CUT => { self.ch[ci].voice = None }
            FADE => if let Some(v) = &mut self.ch[ci].voice { v.fading = true },
            _ => {}
        }
        let c = &mut self.ch[ci];
        c.arp = None;
        c.vib_now = 0.0;
        c.trem_now = 0.0;
        c.tremor_off = false;
        match cell.vol {
            Vol::Set(v) => c.vol = v as f32,
            Vol::Pan(p) => c.pan = p as i32 * 2 - 64,
            Vol::FineUp(x) => c.vol = (c.vol + x as f32).min(64.0),
            Vol::FineDown(x) => c.vol = (c.vol - x as f32).max(0.0),
            _ => {}
        }
        let mut c = std::mem::take(&mut self.ch[ci]);
        match cell.fx {
            Fx::Speed(s) => self.speed = s as u32,
            Fx::Tempo(t) => self.tempo = t as u32,
            Fx::Jump(o) => *jump = Some((Some(o as usize), jump.and_then(|j| j.1).or(Some(0)))),
            Fx::Break(r) => *jump = Some((jump.and_then(|j| j.0), Some(r as usize))),
            Fx::VolSlide(p) | Fx::TonePortaVol(p) | Fx::VibratoVol(p) => {
                let p = self.mem(&mut c, M_VOL, p);
                let (a, b) = (p >> 4, p & 15);
                if b == 15 && a != 0 { c.vol = (c.vol + a as f32).min(64.0) } else if a == 15 && b != 0 { c.vol = (c.vol - b as f32).max(0.0) }
            }
            Fx::PortaDown(p) | Fx::PortaUp(p) => {
                let up = matches!(cell.fx, Fx::PortaUp(_));
                let slot = if self.m.kind == Kind::Xm && !up { M_PORTA_DOWN } else { M_PORTA };
                let p = self.mem(&mut c, slot, p);
                let s = if up { 1.0 } else { -1.0 };
                if self.m.kind != Kind::Xm && self.m.kind != Kind::Mod {
                    if p >> 4 == 15 { self.slide(&mut c, s * (p & 15) as f64) } else if p >> 4 == 14 { self.slide(&mut c, s * (p & 15) as f64 / 4.0) }
                }
            }
            Fx::FinePortaUp(p) => { let p = self.mem(&mut c, M_FINE_UP, p); self.slide(&mut c, p as f64) }
            Fx::FinePortaDown(p) => { let p = self.mem(&mut c, M_FINE_DOWN, p); self.slide(&mut c, -(p as f64)) }
            Fx::XFinePortaUp(p) => { let p = self.mem(&mut c, M_XFINE_UP, p); self.slide(&mut c, p as f64 / 4.0) }
            Fx::XFinePortaDown(p) => { let p = self.mem(&mut c, M_XFINE_DOWN, p); self.slide(&mut c, -(p as f64) / 4.0) }
            Fx::FineVolUp(p) => { let p = self.mem(&mut c, M_FVOL_UP, p); c.vol = (c.vol + p as f32).min(64.0) }
            Fx::FineVolDown(p) => { let p = self.mem(&mut c, M_FVOL_DOWN, p); c.vol = (c.vol - p as f32).max(0.0) }
            Fx::SetVol(v) => c.vol = v as f32,
            Fx::ChanVol(v) => c.chvol = v as f32,
            Fx::ChanVolSlide(p) => {
                let p = self.mem(&mut c, M_OTHER, p);
                let (a, b) = (p >> 4, p & 15);
                if b == 15 && a != 0 { c.chvol = (c.chvol + a as f32).min(64.0) } else if a == 15 && b != 0 { c.chvol = (c.chvol - b as f32).max(0.0) }
            }
            Fx::Offset(p) => {
                let p = self.mem(&mut c, M_OFFSET, p);
                if let Some(v) = c.voice.as_mut().filter(|v| v.trig) { v.pos = (p as u32 * 256 + c.offset_hi * 65536) as f64 }
            }
            Fx::HighOffset(h) => c.offset_hi = h as u32,
            Fx::Pan(p) => c.pan = p as i32 / 2 - 64,
            Fx::GlobalVol(v) => self.gvol = v as f32,
            Fx::SetFinetune(f) => { c.finetune = if f > 7 { f as i32 - 16 } else { f as i32 } }
            Fx::VibWave(w) => c.vib.3 = w,
            Fx::TremWave(w) => c.trem.3 = w,
            Fx::PatLoop(n) if n > 0 => {
                if c.loop_n == 0 { c.loop_n = n; *jump = Some((None, Some(usize::MAX))) } else {
                    c.loop_n -= 1;
                    if c.loop_n > 0 { *jump = Some((None, Some(usize::MAX))) }
                }
            }
            Fx::PatDelay(n) => { if *delay == 0 { *delay = n as u32 } }
            Fx::FinePatDelay(n) => *fine += n as u32,
            Fx::Past(k) => match k {
                0 => self.bg.fill(None),
                1 => for v in self.bg.iter_mut().flatten() { v.on = false },
                2 => for v in self.bg.iter_mut().flatten() { v.fading = true },
                3..=6 => c.nna = Some(k - 3),
                _ => {}
            },
            Fx::Retrig(p) => { self.mem(&mut c, M_RETRIG, p); c.retrig = 0 }
            Fx::EnvPos(p) => if let Some(v) = &mut c.voice { v.venv = p as u32; v.penv = p as u32 },
            Fx::KeyOff(0) => { self.ch[ci] = c; self.key_off(ci); c = std::mem::take(&mut self.ch[ci]) }
            Fx::Tremor(p) => { let p = self.mem(&mut c, M_OTHER, p); c.tremor.0 = p }
            _ => {}
        }
        self.ch[ci] = c;
    }

    /// Effects on every tick but the first.
    fn tick(&mut self, ci: usize, cell: &Cell, t: u32) {
        let mut c = std::mem::take(&mut self.ch[ci]);
        // Volume column.
        match cell.vol {
            Vol::SlideUp(x) if t > 0 => c.vol = (c.vol + x as f32).min(64.0),
            Vol::SlideDown(x) if t > 0 => c.vol = (c.vol - x as f32).max(0.0),
            Vol::PortaUp(x) if t > 0 => self.slide(&mut c, 4.0 * x as f64),
            Vol::PortaDown(x) if t > 0 => self.slide(&mut c, -4.0 * x as f64),
            Vol::TonePorta(x) if t > 0 => { let p = self.mem(&mut c, M_TONE, x); self.porta(&mut c, p) }
            Vol::VibSpeed(x) => { c.vib.0 = x; self.vibrato(&mut c, t, 1.0) }
            Vol::Vibrato(x) => { if x > 0 { c.vib.1 = x } self.vibrato(&mut c, t, 1.0) }
            Vol::PanLeft(x) if t > 0 => c.pan = (c.pan - 2 * x as i32).max(-64),
            Vol::PanRight(x) if t > 0 => c.pan = (c.pan + 2 * x as i32).min(64),
            _ => {}
        }
        let (dx, dy) = (|p: u8| p >> 4, |p: u8| p & 15);
        match cell.fx {
            Fx::Arp(p) => {
                let p = if self.m.kind == Kind::It || self.m.kind == Kind::S3m { self.mem(&mut c, M_OTHER, p) } else { p };
                c.arp = Some([0, dx(p), dy(p)][(t % 3) as usize]);
            }
            Fx::VolSlide(_) | Fx::TonePortaVol(_) | Fx::VibratoVol(_) if t > 0 => {
                let p = c.mem[if self.m.kind == Kind::S3m { M_OTHER } else { M_VOL }];
                let (a, b) = (p >> 4, p & 15);
                if b == 0 { c.vol = (c.vol + a as f32).min(64.0) } else if a == 0 { c.vol = (c.vol - b as f32).max(0.0) }
                else if self.m.kind == Kind::Xm || self.m.kind == Kind::Mod { c.vol = (c.vol + a as f32).min(64.0) }
            }
            _ => {}
        }
        match cell.fx {
            Fx::PortaUp(p) | Fx::PortaDown(p) if t > 0 => {
                let up = matches!(cell.fx, Fx::PortaUp(_));
                let slot = if self.m.kind == Kind::Xm && !up { M_PORTA_DOWN } else { M_PORTA };
                let p = if self.m.kind == Kind::Mod { p } else { c.mem[if self.m.kind == Kind::S3m { M_OTHER } else { slot }] };
                if self.m.kind == Kind::Mod || self.m.kind == Kind::Xm || p >> 4 < 14 {
                    self.slide(&mut c, if up { 1.0 } else { -1.0 } * p as f64);
                }
            }
            Fx::TonePorta(p) if t > 0 => { let p = self.mem(&mut c, M_TONE, p); self.porta(&mut c, p) }
            Fx::TonePortaVol(_) if t > 0 => { let p = c.mem[if self.m.kind == Kind::It && !self.m.compat_g { M_PORTA } else { M_TONE }]; self.porta(&mut c, p) }
            Fx::Vibrato(p) | Fx::FineVibrato(p) => {
                // Speed and depth each keep their last nonzero value.
                let old = c.mem[M_VIB];
                let (sp, dp) = (if p >> 4 != 0 { p >> 4 } else { old >> 4 }, if p & 15 != 0 { p & 15 } else { old & 15 });
                c.mem[M_VIB] = sp << 4 | dp;
                (c.vib.0, c.vib.1) = (sp, dp);
                self.vibrato(&mut c, t, if matches!(cell.fx, Fx::FineVibrato(_)) { 0.25 } else { 1.0 });
            }
            Fx::VibratoVol(_) => self.vibrato(&mut c, t, 1.0),
            Fx::Tremolo(p) => {
                if p >> 4 != 0 { c.trem.0 = p >> 4 }
                if p & 15 != 0 { c.trem.1 = p & 15 }
                c.trem_now = (sine(c.trem.2, c.trem.3) * c.trem.1 as f64 * 4.0) as f32;
                if t > 0 { c.trem.2 = c.trem.2.wrapping_add(c.trem.0) }
            }
            Fx::Tremor(_) => {
                let (on, off) = (dx(c.tremor.0) + 1, dy(c.tremor.0) + 1);
                c.tremor_off = (t % (on + off) as u32) >= on as u32;
            }
            Fx::NoteCut(x) if t == x as u32 => c.vol = 0.0,
            Fx::Retrig(_) if t > 0 => {
                let p = c.mem[if self.m.kind == Kind::S3m { M_OTHER } else { M_RETRIG }];
                let (vx, iv) = (p >> 4, (p & 15).max(1));
                c.retrig += 1;
                if c.retrig >= iv {
                    c.retrig = 0;
                    if let Some(v) = &mut c.voice { v.pos = 0.0; v.trig = true; v.venv = 0; v.penv = 0 }
                    c.vol = match vx { 1..=5 => c.vol - (1 << (vx - 1)) as f32, 6 => c.vol * 2.0 / 3.0, 7 => c.vol / 2.0, 9..=13 => c.vol + (1 << (vx - 9)) as f32, 14 => c.vol * 1.5, 15 => c.vol * 2.0, _ => c.vol }.clamp(0.0, 64.0);
                }
            }
            Fx::ChanVolSlide(_) if t > 0 => {
                let p = c.mem[M_OTHER];
                let (a, b) = (p >> 4, p & 15);
                if b == 0 { c.chvol = (c.chvol + a as f32).min(64.0) } else if a == 0 { c.chvol = (c.chvol - b as f32).max(0.0) }
            }
            Fx::PanSlide(p) if t > 0 => {
                let p = self.mem(&mut c, M_PAN, p);
                let (a, b) = (p >> 4, p & 15);
                if b == 0 { c.pan = (c.pan - 2 * a as i32).max(-64) } else if a == 0 { c.pan = (c.pan + 2 * b as i32).min(64) }
            }
            Fx::GlobalVolSlide(p) if t > 0 => {
                let p = self.mem(&mut c, M_GVOL, p);
                let (a, b) = (p >> 4, p & 15);
                if b == 0 { self.gvol = (self.gvol + 2.0 * a as f32).min(128.0) } else if a == 0 { self.gvol = (self.gvol - 2.0 * b as f32).max(0.0) }
            }
            Fx::KeyOff(x) if x > 0 && t == x as u32 => { self.ch[ci] = c; self.key_off(ci); c = std::mem::take(&mut self.ch[ci]) }
            Fx::TempoSlide(p) if t > 0 => {
                let (a, b) = (p >> 4, p & 15);
                self.tempo = if a == 1 { (self.tempo + b as u32).min(255) } else { self.tempo.saturating_sub(b as u32).max(32) };
            }
            _ => {}
        }
        self.ch[ci] = c;
    }

    /// Tone portamento: move the pitch toward the target by `p` (Amiga units, or 4/64 semitone) per tick.
    fn porta(&self, c: &mut Chan, p: u8) {
        let s = p as f64 * if self.m.linear { 4.0 } else { 1.0 };
        if (c.p - c.target).abs() <= s { c.p = c.target } else if c.p < c.target { c.p += s } else { c.p -= s }
    }

    fn vibrato(&self, c: &mut Chan, t: u32, scale: f64) {
        c.vib_now = sine(c.vib.2, c.vib.3) * c.vib.1 as f64 * 2.0 * scale;
        if t > 0 || self.m.kind == Kind::It { c.vib.2 = c.vib.2.wrapping_add(c.vib.0) }
    }

    /// Advance envelopes and fadeout by one tick; false once the voice has gone silent.
    fn age(m: &Module, v: &mut Voice) -> bool {
        let i = &m.instruments[v.ins - 1];
        if let Some(e) = &i.venv {
            v.venv = step(e, v.venv, v.on);
            if e.lp.is_none() && !(v.on && e.sustain.is_some()) && v.venv >= e.pts.last().map_or(0, |p| p.0 as u32) && e.at(v.venv) <= 0.0 { return false }
        }
        if let Some(e) = &i.penv { v.penv = step(e, v.penv, v.on) }
        if v.fading {
            v.fade -= i.fadeout as f32 / 65536.0;
            if v.fade <= 0.0 || i.fadeout == 0 && !v.on && i.venv.is_none() { return false }
        }
        true
    }

    /// Envelope level (0..1) and panning offset of a voice.
    fn levels(m: &Module, v: &Voice) -> (f32, i32) {
        let i = &m.instruments[v.ins - 1];
        let e = i.venv.as_ref().map_or(1.0, |e| e.at(v.venv) / 64.0);
        let p = i.penv.as_ref().map_or(0, |e| (e.at(v.penv) * 2.0) as i32);
        (e * v.fade.max(0.0) * i.gvol as f32 / 128.0 * m.samples[v.smp].gvol as f32 / 64.0, p)
    }
}

/// What a voice plays during one tick, for `render`: sample, position at the tick's start (folded into the first
/// loop period), rate (frames per second), amplitude and pan.
#[derive(Clone, Copy, Debug)]
pub struct Played { pub smp: usize, pub pos: f64, pub rate: f64, pub amp: f32, pub pan: i32 }

/// The performance tick by tick: each tick's length in seconds, and what each score channel plays in it.
#[derive(Default)]
pub struct Trace { pub secs: Vec<f64>, pub chans: Vec<Vec<Option<Played>>> }

/// A playback position past the loop end, folded back into the loop's first period.
fn fold(s: &Sample, pos: f64) -> f64 {
    let Some((l, e, ping)) = s.lp else { return pos };
    if pos < e as f64 { return pos }
    let n = e - l;
    let per = if ping && n > 1 { 2 * n - 2 } else { n } as f64;
    l as f64 + (pos - l as f64) % per
}

/// Mix the traced performance straight from the samples (linear interpolation, SiON's pan law) at SiON's rate,
/// each tick's sound starting at its exact time: the reference that the `%7` conversion should match.
pub fn render(m: &Module, tr: &Trace) -> Vec<f32> {
    let rate = sion::RATE as f64;
    let smp: Vec<sion::Sample> = m.samples.iter().map(as_sion).collect();
    // Tick times counted from the last tempo change (summing every tick would let rounding build up).
    let (mut at, mut times, mut base) = (vec![0usize], vec![0f64], (0f64, 0f64, 0u64));
    for &s in &tr.secs {
        if s != base.1 { base = (times[times.len() - 1], s, 0) }
        base.2 += 1;
        let t = base.0 + base.2 as f64 * s;
        at.push((t * rate - 1e-6).ceil() as usize);
        times.push(t);
    }
    let mut out = vec![0f32; 2 * at[at.len() - 1]];
    let q = std::f32::consts::PI / 256.0;
    for ch in &tr.chans {
        for (k, p) in ch.iter().enumerate() {
            let Some(p) = p.filter(|p| p.amp != 0.0) else { continue };
            let s = &smp[p.smp];
            let pan = (p.pan + 64) as f32;
            let (l, r) = (((128.0 - pan) * q).sin() * p.amp, (pan * q).sin() * p.amp);
            for i in at[k]..at[k + 1] {
                let x = (p.pos + p.rate * (i as f64 / rate - times[k])).max(0.0);
                let (u, f) = (x as usize, x.fract() as f32);
                let (a, b) = (s.at(u), s.at(u + 1));
                let v = a + (b - a) * f;
                out[2 * i] += v * l;
                out[2 * i + 1] += v * r;
            }
        }
    }
    out
}

/// Next envelope tick: hold at the sustain point while the key is down, follow the loop.
fn step(e: &Env, t: u32, on: bool) -> u32 {
    let x = |k: usize| e.pts.get(k).map_or(0, |p| p.0 as u32);
    if on { if let Some((s, se)) = e.sustain { if t >= x(se) { return x(s) } } }
    if let Some((lb, le)) = e.lp { if t >= x(le) && !(on && e.sustain.is_some_and(|s| x(s.1) < x(le) && t < x(s.1))) { return x(lb) } }
    t + 1
}

/// Perform the module. Samples are embedded as PCM unless `wavetables`.
/// The module performed: its score, warnings, and the trace `render` mixes. `tidy` performs it the way it would
/// be written by hand rather than exactly (see `Options::exact`): volume and pan per row, whole notes per row,
/// single-cycle loops as wavetables, and the order list's patterns as the score's sections.
pub fn to_score(m: &Module, wavetables: bool, tidy: bool) -> (Score, Vec<String>, Trace) {
    let mut defs = Defs::default();
    let mut full = 0;
    // (A sample that is all loop, of at most 1024 frames, is one cycle of a waveform: the wavetable is exact.)
    let cycle = |s: &Sample| s.lp.is_some_and(|(a, b, _)| a <= 16 && b - a <= 1024 && b + 16 >= s.data.len());
    let sounds: Vec<Sound> = m.samples.iter().map(|s| {
        if wavetables || (tidy && cycle(s)) { return analyze(s, &mut defs) }
        pcm(s, &mut defs).unwrap_or_else(|| { full += 1; analyze(s, &mut defs) })
    }).collect();
    let mut tr = Trace { chans: vec![vec![]; m.chans], ..Trace::default() };
    let mut p = Player {
        m, sounds, bg: vec![], gvol: m.gvol as f32, speed: m.speed.max(1), tempo: m.tempo.max(32),
        clock: if m.kind == Kind::Mod { 3546895.0 } else { 8363.0 * 428.0 },
        ch: (0..m.chans).map(|c| Chan { vol: 64.0, chvol: m.chvol[c] as f32, pan: m.pan[c], ..Chan::default() }).collect(),
    };
    let mut st = Stream { chans: vec![vec![]; m.chans], exact: !wavetables && !tidy, ..Stream::default() };
    // Tidy: each channel's level and pan as set at the start of the row (or at its key-on), and its instrument names.
    let mut held: Vec<Option<(f32, i32)>> = vec![];
    let mut names: Vec<Vec<String>> = vec![];
    let name = |names: &mut Vec<Vec<String>>, c: usize, ins: usize| {
        if names.len() <= c { names.resize(c + 1, vec![]) }
        if let Some(i) = m.instruments.get(ins.wrapping_sub(1)).filter(|i| !i.name.is_empty() && !names[c].contains(&i.name)) { names[c].push(i.name.clone()) }
    };
    let mut sections: Vec<u64> = vec![];
    let mut last_ord = usize::MAX;
    let ppq = 4 * if (2..=12).contains(&p.speed) { p.speed } else { 6 };
    let mut tempos: Vec<(u64, f64)> = vec![];
    let (mut ord, mut row, mut tick) = (0usize, 0usize, 0u64);
    // The tick each (order, row) was first played at: going back to one is the song looping, to there.
    let mut seen = std::collections::HashMap::new();
    let mut lp = None;
    let limit = 30 * 60 * 255 * 2 / 5;
    let mut warnings = vec![];
    if full > 0 { warnings.push(format!("{full} samples past SiON's 256 PCM slots are approximated with wavetables or noise")) }
    'song: loop {
        // Find the next playable order.
        while ord < m.orders.len() && (m.orders[ord] == SKIP || m.orders[ord] as usize >= m.patterns.len()) {
            if m.orders[ord] == END { break }
            ord += 1;
        }
        if ord >= m.orders.len() || m.orders[ord] == END {
            // Past the last order the tracker goes back to the restart position.
            let mut o = if m.restart < ord { m.restart } else { 0 };
            while o < ord && (m.orders[o] == SKIP || m.orders[o] as usize >= m.patterns.len()) { o += 1 }
            lp = seen.get(&(o, 0)).copied();
            break
        }
        let pat = &m.patterns[m.orders[ord] as usize];
        if row >= pat.rows { row = 0; ord += 1; continue }
        if !p.ch.iter().any(|c| c.loop_n > 0) {
            if let Some(&t) = seen.get(&(ord, row)) { lp = Some(t); break }
            seen.insert((ord, row), tick);
        }
        if ord != last_ord || row == 0 { sections.push(tick); last_ord = ord }
        let cells: Vec<Cell> = (0..m.chans).map(|c| pat.cells.get(row * m.chans + c).copied().unwrap_or_default()).collect();
        let (mut jump, mut delay, mut fine) = (None, 0u32, 0u32);
        for (c, cell) in cells.iter().enumerate() {
            if let Fx::PatLoop(0) = cell.fx { p.ch[c].loop_row = row }
            if let Fx::NoteDelay(d) = cell.fx { if d > 0 { continue } }
            p.row(c, cell, &mut jump, &mut delay, &mut fine);
        }
        let ticks = p.speed * (1 + delay) + fine;
        for t in 0..ticks {
            let rt = t % p.speed.max(1);
            for (c, cell) in cells.iter().enumerate() {
                if let Fx::NoteDelay(d) = cell.fx { if d > 0 && t == d as u32 { let mut none = (None, 0, 0); p.row(c, cell, &mut none.0, &mut none.1, &mut none.2) } }
                p.tick(c, cell, rt);
            }
            // A tempo set on this row counts from its first tick.
            let bpm = p.tempo as f64 * 24.0 / ppq as f64;
            if tempos.last().is_none_or(|l: &(u64, f64)| l.1 != bpm) { tempos.push((tick, bpm)) }
            let secs = 2.5 / p.tempo as f64;
            tr.secs.push(secs);
            for c in 0..m.chans {
                let rate = p.rate(p.ch[c].p, p.ch[c].arp.unwrap_or(0) as f64, p.ch[c].vib_now);
                let ch = &mut p.ch[c];
                let Some(mut v) = ch.voice.take() else { st.chans[c].push(None); tr.chans[c].push(None); continue };
                let alive = Player::age(m, &mut v);
                let s = &m.samples[v.smp];
                let (env, pe) = Player::levels(m, &v);
                let snd = &p.sounds[v.smp];
                let amp = (ch.vol + ch.trem_now).clamp(0.0, 64.0) / 64.0 * ch.chvol / 64.0 * p.gvol / 128.0 * env * snd.gain * if ch.tremor_off { 0.0 } else { 1.0 };
                let tone = st.tone(&snd.instr, &snd.env, snd.legato);
                let (pos, pan) = (fold(s, v.pos), (ch.pan + pe).clamp(-64, 64));
                let trig = std::mem::take(&mut v.trig);
                let (amp, pan) = if tidy { hold(&mut held, c, rt == 0 || trig, amp, pan) } else { (amp, pan) };
                if tidy { name(&mut names, c, v.ins) }
                let f = Frame { pitch: snd.pitch(rate).clamp(0.0, 119.0), amp, tone, pan, trig, pos, lfo: (0, 0), rel: false };
                st.chans[c].push(Some(f));
                tr.chans[c].push(Some(Played { smp: v.smp, pos, rate, amp, pan }));
                v.pos += rate * secs;
                let ended = s.lp.is_none() && v.pos >= s.data.len() as f64;
                // (Tidy: a one-shot that has played out still lasts to the end of its row, so notes are whole rows.)
                if alive && (!ended || (tidy && rt + 1 < p.speed.max(1))) { ch.voice = Some(v) }
            }
            // Background voices, each on a channel of its own.
            for k in 0..p.bg.len() {
                let Some(mut v) = p.bg[k].take() else { continue };
                while st.chans.len() <= m.chans + k { st.chans.push(vec![None; tick as usize]); tr.chans.push(vec![None; tick as usize]) }
                let alive = Player::age(m, &mut v);
                let (env, pe) = Player::levels(m, &v);
                let snd = &p.sounds[v.smp];
                let tone = st.tone(&snd.instr, &snd.env, snd.legato);
                let amp = v.vol / 64.0 * p.gvol / 128.0 * env * snd.gain;
                let s = &m.samples[v.smp];
                let (pos, pan) = (fold(s, v.pos), (v.pan + pe).clamp(-64, 64));
                let trig = std::mem::take(&mut v.trig);
                let (amp, pan) = if tidy { hold(&mut held, m.chans + k, rt == 0 || trig, amp, pan) } else { (amp, pan) };
                if tidy { name(&mut names, m.chans + k, v.ins) }
                st.chans[m.chans + k].push(Some(Frame { pitch: snd.pitch(v.rate).clamp(0.0, 119.0), amp, tone, pan, trig, pos, lfo: (0, 0), rel: false }));
                tr.chans[m.chans + k].push(Some(Played { smp: v.smp, pos, rate: v.rate, amp, pan }));
                v.pos += v.rate * secs;
                if alive && (!(s.lp.is_none() && v.pos >= s.data.len() as f64) || (tidy && rt + 1 < p.speed.max(1))) { p.bg[k] = Some(v) }
            }
            tick += 1;
            for ch in st.chans.iter_mut() { if ch.len() < tick as usize { ch.push(None) } }
            for ch in tr.chans.iter_mut() { if ch.len() < tick as usize { ch.push(None) } }
            if tick > limit { warnings.push("stopped after 30 minutes".into()); break 'song }
        }
        // Next row.
        match jump {
            Some((_, Some(usize::MAX))) => {
                // Pattern loop back to the loop row.
                row = p.ch.iter().find(|c| c.loop_n > 0).map_or(0, |c| c.loop_row);
            }
            Some((o, r)) => {
                let next = o.unwrap_or(ord + 1);
                if next <= ord && o.is_some() { if let Some(&t) = seen.get(&(next, r.unwrap_or(0))) { lp = Some(t); break } }
                (ord, row) = (next, r.unwrap_or(0));
                for c in &mut p.ch { c.loop_row = 0 }
            }
            None => row += 1,
        }
    }
    let mut notes = st.notes(0, if wavetables || tidy { 2 } else { 1 });
    notes.retain(|n| n.end > n.start);
    warnings.extend(m.warnings.iter().cloned());
    let exact = !wavetables && !tidy;
    let used = notes.iter().map(|n| n.ch as usize + 1).max().unwrap_or(0).max(names.len());
    names.resize(used, vec![]);
    let names = names.into_iter().enumerate().map(|(c, n)| if n.is_empty() { format!("channel {}", c + 1) } else { format!("channel {}: {}", c + 1, n.join(", ")) }).collect();
    let mut score = Score { ppq, tempos, header: defs.header(), notes, mix: vec![], end: tick, lp, fine: exact, db: false, kmilli: exact, ..Score::default() };
    if tidy { (score.sections, score.names, score.title) = (sections, names, m.title.clone()) }
    (score, warnings, tr)
}

/// Tidy playback: the level and pan a channel had at the start of its row (`new`: this tick starts it), unless it
/// has gone silent since (a note cut).
fn hold(held: &mut Vec<Option<(f32, i32)>>, c: usize, new: bool, amp: f32, pan: i32) -> (f32, i32) {
    if held.len() <= c { held.resize(c + 1, None) }
    match held[c] {
        Some((a, p)) if !new => if amp <= 0.0 { (0.0, p) } else { (a, p) },
        _ => { held[c] = Some((amp, pan)); (amp, pan) }
    }
}
