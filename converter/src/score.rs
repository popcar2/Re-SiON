//! Format-neutral song that every reader produces and `arrange` turns into SiON tracks: notes on numbered
//! channels. Tracker and VGM playback first record what each chip/tracker channel sounds like frame by frame
//! (`Stream`); `Stream::notes` turns those frames into notes with slurred pitch and volume changes.

pub struct Score {
    /// Ticks per quarter note.
    pub ppq: u32,
    /// Tempo changes: (tick, quarter notes per minute). Without one at tick 0 the song starts at 120.
    pub tempos: Vec<(u64, f64)>,
    /// System commands (without the trailing `;`), e.g. `#WAV0{...}`.
    pub header: Vec<String>,
    pub notes: Vec<Note>,
    /// (start, end, amplitude) spans that estimate how loud the busiest moment gets, when the notes alone
    /// don't tell (MIDI drum hits are written one tick long but ring until their note-off). Empty: the notes.
    pub mix: Vec<(u64, u64, f32)>,
    /// Where the song ends, if after its last note (trailing silence matters when the song loops).
    pub end: u64,
    /// Where the song loops back to once it reaches `end` (a VGM's loop point): every track then plays to `end` and
    /// jumps back to a `$` at this tick. None: the song plays once.
    pub lp: Option<u64>,
    /// Volume in 256 steps instead of 16 (PCM samples, whose level should come out as the source's).
    pub fine: bool,
    /// Volume in decibels instead (chips, whose levels step in dB): `v0`..`v128` in 0.75 dB steps over 96 dB
    /// (`#VMODE{%v1,1}`), the YM2612's own total-level step. (Songs with samples keep `fine`: 0.75 dB steps are
    /// too coarse for a sample to come out as the chip played it.) Takes precedence over `fine`.
    pub db: bool,
    /// Pitch to 1/1000 of SiON's 1/64-semitone step (fractional `k`, the notes' detunes in those units), with
    /// every change kept (tracker samples, which should play exactly as the module).
    pub kmilli: bool,
}

#[derive(Clone, Debug)]
pub struct Note {
    /// Notes of one channel are split into monophonic tracks; different channels never share a track.
    pub ch: u32,
    pub start: u64,
    /// Key-off tick.
    pub end: u64,
    pub key: u8,
    /// Linear amplitude, any scale: the loudest becomes `v16`.
    pub amp: f32,
    /// Module command (`%1@4`), then the tone/envelope commands that follow it (`@4s63`, may be empty).
    pub instr: String,
    pub env: String,
    /// -64..64.
    pub pan: i32,
    /// Pitch envelope table (`np`).
    pub ptab: Option<u8>,
    /// Detune (`k`, 1/64 semitone; in thousandths of that with `Score::kmilli`) at the start, then (tick, k)
    /// changes while it sounds.
    pub k0: i32,
    pub bends: Vec<(u64, i32)>,
    /// (tick, amplitude) and (tick, pan) changes while it sounds.
    pub vols: Vec<(u64, f32)>,
    pub pans: Vec<(u64, i32)>,
    /// PCM notes: the sample frame to start from (`@ph`).
    pub ofs: f64,
    /// PCM notes: how many sample frames to play from there (`@ph ofs,len`; 0: all).
    pub plen: f64,
    /// Hardware LFO depths (`mp` vibrato, `ma` tremolo; 0 = off) at the start, then (tick, depths) changes while
    /// it sounds. The LFO's rate is part of the tone (`@lfo` in `env`).
    pub lfo0: (i32, i32),
    pub lfos: Vec<(u64, (i32, i32))>,
    /// (tick, instr) waveform changes while it sounds, within `instr`'s module (`%4@1` → `%4@2`: written `@2`).
    pub waves: Vec<(u64, String)>,
    /// FM release: the pitch after the key-off, per 1/60 s frame, relative to the pitch at the key-off (1/64
    /// semitones; a key-off pitch table, `_np`), and the tick at which the release is silenced (`v0`), if before
    /// the next note.
    pub rel: Vec<i32>,
    pub cut: Option<u64>,
    /// Drum hit: lasts until the next hit on its track. `Some(true)`: keyed off at once (`q0`), fading by its
    /// release; `Some(false)`: held (`q8`), fading by its envelope's decay.
    pub drum: Option<bool>,
}

impl Score {
    /// Seconds from the start to tick `t`.
    pub fn seconds(&self, t: u64) -> f64 {
        let (mut s, mut at, mut bpm) = (0.0, 0u64, 120.0);
        for &(tt, b) in &self.tempos {
            if tt >= t { break }
            s += (tt - at) as f64 / self.ppq as f64 * 60.0 / bpm;
            (at, bpm) = (tt, b);
        }
        s + (t - at) as f64 / self.ppq as f64 * 60.0 / bpm
    }
}

impl Note {
    pub fn new(ch: u32, start: u64, end: u64, key: u8, amp: f32, instr: String) -> Note {
        Note { ch, start, end, key, amp, instr, env: String::new(), pan: 0, ptab: None, k0: 0, bends: vec![], vols: vec![], pans: vec![], lfo0: (0, 0), lfos: vec![], waves: vec![], rel: vec![], cut: None, ofs: 0.0, plen: 0.0, drum: None }
    }
}

/// How a channel sounds during one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// MIDI key number with fraction (SiON `o5a` = 69 = 440 Hz; for noise tones the key that gives the rate).
    pub pitch: f32,
    /// Linear amplitude, 0 = silent.
    pub amp: f32,
    /// Index into `Stream::tones`.
    pub tone: usize,
    pub pan: i32,
    /// A new key-on happened this frame (restart the note even if nothing else changed).
    pub trig: bool,
    /// PCM samples: the playback position (frames) at the start of the frame.
    pub pos: f64,
    /// Hardware LFO depths (`mp`, `ma`).
    pub lfo: (i32, i32),
    /// Keyed off, releasing (FM): the pitch and level still change the sound of the note that was keyed off.
    pub rel: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tone {
    pub instr: String,
    pub env: String,
    /// Its own envelope shapes each note (FM, decaying samples): pitch changes are slurred bends instead of
    /// new notes, so the envelope isn't restarted.
    pub legato: bool,
}

/// Frame-by-frame channel states. Frame `i` covers ticks `i..i+1`.
#[derive(Default)]
pub struct Stream {
    pub tones: Vec<Tone>,
    pub chans: Vec<Vec<Option<Frame>>>,
    /// PCM playback: a note carries on through silent frames (the sample keeps playing), a note that starts
    /// mid-sample says where (`Note::ofs`), and pan changes are kept.
    pub exact: bool,
}

impl Stream {
    /// Index of a tone, adding it if new.
    pub fn tone(&mut self, instr: &str, env: &str, legato: bool) -> usize {
        let t = Tone { instr: instr.into(), env: env.into(), legato };
        self.tones.iter().position(|x| *x == t).unwrap_or_else(|| { self.tones.push(t); self.tones.len() - 1 })
    }

    /// Tone `a` can change to `b` during a note, without a new key-on: the same envelope, and another waveform
    /// of the same wavetable module (`%1@n`, `%4@n`), which SiON's `@n` swaps without resetting the phase. (A
    /// chip changing the waveform, the VRC6 saw's rate or a pulse's duty, doesn't restart it either.)
    fn switch(&self, a: usize, b: usize) -> bool {
        let (a, b) = (&self.tones[a], &self.tones[b]);
        let module = |s: &str| s.strip_prefix('%').and_then(|r| r.split_once('@')).filter(|(m, n)| matches!(*m, "1" | "4") && n.parse::<u32>().is_ok()).map(|(m, _)| m.to_string());
        a.env == b.env && a.legato == b.legato && module(&a.instr).is_some() && module(&a.instr) == module(&b.instr)
    }

    /// Notes on channels `ch0..`: one note per key-on (or per pitch step for tones without an envelope), with
    /// pitch and volume changes while it sounds. Detune changes smaller than `kstep` (1/64 semitones) are dropped,
    /// and with `kstep` > 1 so are detunes of 1/64 semitone. With `exact` the detunes are in 1/64000 semitones.
    pub fn notes(&self, ch0: u32, kstep: i32) -> Vec<Note> {
        let mut out = vec![];
        let ku = if self.exact { 64000.0 } else { 64.0 };
        let tiny = |k: i32| if kstep > 1 && k.abs() <= 1 { 0 } else { k };
        for (c, frames) in self.chans.iter().enumerate() {
            let mut cur: Option<(Note, usize, f32, i32)> = None; // note, tone, last amplitude, last pan
            // A released note: its index in `out`, its amplitude and pitch (1/64 semitones) at the key-off.
            let mut rel: Option<(usize, f32, i32)> = None;
            let close = |cur: &mut Option<(Note, usize, f32, i32)>, t: u64, out: &mut Vec<Note>| {
                if let Some((mut n, ..)) = cur.take() { n.end = t; out.push(n) }
            };
            for (t, f) in frames.iter().enumerate() {
                let t = t as u64;
                if let Some(f) = f.filter(|f| f.rel && f.pitch.is_finite()) {
                    if let Some((n, _, last, _)) = cur.take() {
                        let k = n.key as i32 * 64 + n.bends.last().map_or(n.k0, |b| b.1);
                        rel = Some((out.len(), last, k));
                        let mut n = n;
                        n.end = t;
                        out.push(n);
                    }
                    if let Some((i, amp, k)) = rel {
                        let n = &mut out[i];
                        n.rel.push((f.pitch as f64 * 64.0).round() as i32 - k);
                        // (Carriers turned all the way down: on the chips, a driver silencing the channel.)
                        if f.amp <= 1e-4 * amp { n.cut = Some(t); rel = None }
                    }
                    continue;
                }
                // (Drivers set the next note's pitch just before keying it on: not part of the release.)
                if let (Some((i, _, k)), Some(f)) = (rel.take(), f.filter(|f| f.trig && f.pitch.is_finite())) {
                    let next = (f.pitch as f64 * 64.0).round() as i32 - k;
                    let r = &mut out[i].rel;
                    while r.len() > 1 && r.last() == Some(&next) && r[0] != next { r.pop(); }
                }
                let Some(f) = f.filter(|f| (f.amp > 0.0 || self.exact) && f.pitch.is_finite()) else {
                    // A cut PCM note goes silent at once: even SiON's fastest release takes a few milliseconds,
                    // so turn the volume off for a tick before the key-off.
                    if let Some((n, _, last, _)) = cur.as_mut().filter(|c| self.exact && c.2 > 0.0) { n.vols.push((t, 0.0)); *last = 0.0; continue }
                    close(&mut cur, t, &mut out);
                    continue;
                };
                // A new pitch without a key-on carries on the same note (written as a slur): the chips' tone
                // generators don't restart their waveform when the frequency changes, and a new note in SiON
                // would, every frame of an arpeggio or slide (a buzz at the frame rate).
                let tone = &self.tones[f.tone];
                let keep = cur.as_ref().is_some_and(|(_, tn, ..)| !f.trig && (*tn == f.tone || self.switch(*tn, f.tone)));
                if !keep {
                    close(&mut cur, t, &mut out);
                    // (A PCM key-on starts its note even when silent: the sample is playing from its start.)
                    if f.amp <= 0.0 && !(self.exact && f.trig) { continue }
                    let key = f.pitch.round().clamp(0.0, 127.0);
                    let mut n = Note::new(ch0 + c as u32, t, t + 1, key as u8, f.amp, tone.instr.clone());
                    n.env = tone.env.clone();
                    n.pan = f.pan;
                    n.lfo0 = f.lfo;
                    // Detunes of a 64th of a semitone (frequency register rounding) are inaudible.
                    n.k0 = tiny(((f.pitch - key) as f64 * ku).round() as i32);
                    if self.exact { n.ofs = (f.pos * 1000.0).round() / 1000.0 }
                    cur = Some((n, f.tone, f.amp, f.pan));
                    continue;
                }
                let (n, tn, last, pan) = cur.as_mut().unwrap();
                if *tn != f.tone { n.waves.push((t, tone.instr.clone())); *tn = f.tone }
                if self.exact && f.pan != *pan { n.pans.push((t, f.pan)); *pan = f.pan }
                let k = tiny(((f.pitch - n.key as f32) as f64 * ku).round() as i32);
                let lk = n.bends.last().map_or(n.k0, |b| b.1);
                if (k - lk).abs() >= kstep { n.bends.push((t, k)) }
                if (f.amp - *last).abs() > 1e-4 * last.max(f.amp) { n.vols.push((t, f.amp)); *last = f.amp }
                if f.lfo != n.lfos.last().map_or(n.lfo0, |l| l.1) { n.lfos.push((t, f.lfo)) }
            }
            close(&mut cur, frames.len() as u64, &mut out);
        }
        // A PCM note that goes silent for good ends a tick later (the volume is already off by then).
        for n in out.iter_mut().filter(|_| self.exact) {
            if let Some(&(t, _)) = n.vols.last().filter(|v| v.1 == 0.0 && v.0 + 1 < n.end) {
                n.end = t + 1;
                n.bends.retain(|b| b.0 < n.end);
                n.pans.retain(|p| p.0 < n.end);
            }
        }
        // A release pitch that never moves needs no table, and one that settles needs none past that (the table holds
        // its last value).
        for n in &mut out {
            while n.rel.len() > 1 && n.rel[n.rel.len() - 2] == n.rel[n.rel.len() - 1] { n.rel.pop(); }
            if n.rel.iter().all(|&k| k.abs() <= 1) { n.rel.clear() }
        }
        // (and one keyed on silent that never sounds is left out.)
        out.retain(|n| !(self.exact && n.amp == 0.0 && n.vols.iter().all(|v| v.1 == 0.0)));
        out
    }
}

/// Frequency in Hz as a key number (69 = A 440 Hz).
pub fn hz_key(hz: f64) -> f32 { (69.0 + 12.0 * (hz / 440.0).log2()) as f32 }

/// Key for SiON's sampled noise (`%2@n`) running at `rate` table steps per second.
pub fn noise_key(rate: f64) -> f32 { (12.0 * (rate / (0.018581361 * 44100.0)).log2()) as f32 }

/// Amplitude scale for a chip's noise shifting at `rate` per second: the share of its power below 22.05 kHz.
/// Each value is held for 1/rate s (a sinc² spectrum); what lies above that the output filters drop (a noise
/// shifting at 112 kHz keeps about 37%, -4.3 dB), while SiON's noise puts all of its power in band.
pub fn noise_level(rate: f64) -> f32 {
    use std::f64::consts::PI;
    let b = 22050.0 / rate.max(1.0);
    // 2 ∫0^b sinc²(x) dx, sinc(x) = sin(πx) / πx; past a few lobes the tail is about 1 / (π² b).
    let share = if b > 8.0 { 1.0 - 1.0 / (PI * PI * b) } else {
        let n = 4096;
        let h = b / n as f64;
        2.0 * h * (0..n).map(|i| { let x = PI * (i as f64 + 0.5) * h; (x.sin() / x).powi(2) }).sum::<f64>()
    };
    share.clamp(0.0, 1.0).sqrt() as f32
}

/// Wavetables (`#WAV`), FM voices (`#OPN@`, `#OPM@`) and PCM samples (`#PCMB`) used by the converted song,
/// numbered as added.
#[derive(Default)]
pub struct Defs {
    pub waves: Vec<Vec<i32>>,
    pub voices: Vec<String>,
    pub pcm: Vec<String>,
    /// `#TABLE`s, numbered down from 254 (custom LFO waveforms need 8..254).
    pub tables: Vec<String>,
    ids: std::collections::HashMap<String, usize>,
}

impl Defs {
    /// Number of a wavetable (values -128..127; length a power of two up to 1024).
    pub fn wave(&mut self, w: &[i32]) -> usize {
        let key = format!("w{w:?}");
        if let Some(&i) = self.ids.get(&key) { return i }
        if self.waves.len() >= 256 { return 255 }
        self.waves.push(w.to_vec());
        self.ids.insert(key, self.waves.len() - 1);
        self.waves.len() - 1
    }

    /// Number of an FM voice, e.g. `voice("OPN@", &[alg, fb, ...])`. SiON has 256 voice slots; past that the
    /// last one is reused.
    pub fn voice(&mut self, kind: &str, nums: &[i32]) -> usize {
        let body = nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(",");
        let key = format!("{kind}{body}");
        if let Some(&i) = self.ids.get(&key) { return i }
        if self.voices.len() >= 256 { return 255 }
        self.voices.push(format!("#{kind}{}{{{body}}}", self.voices.len()));
        self.ids.insert(key, self.voices.len() - 1);
        self.voices.len() - 1
    }

    /// Number of a `#TABLE` with this body.
    pub fn table(&mut self, body: &str) -> usize {
        let key = format!("t{body}");
        if let Some(&i) = self.ids.get(&key) { return i }
        self.tables.push(body.into());
        let i = 255 - self.tables.len();
        self.ids.insert(key, i);
        i
    }

    /// Number of a PCM sample given the text after `#PCMBn`, or None when all 256 slots are taken.
    pub fn pcm(&mut self, def: String) -> Option<usize> {
        if let Some(i) = self.pcm.iter().position(|d| *d == def) { return Some(i) }
        if self.pcm.len() >= 256 { return None }
        self.pcm.push(def);
        Some(self.pcm.len() - 1)
    }

    pub fn header(&self) -> Vec<String> {
        let mut h: Vec<String> = self.waves.iter().enumerate()
            .map(|(i, w)| format!("#WAV{i}{{{}}}", w.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","))).collect();
        // Chip FM voices: feedback the way the Yamaha chips do it (the `sion` crate's extension; SiON ignores it).
        if self.voices.iter().any(|v| v.starts_with("#OPN@") || v.starts_with("#OPM@")) { h.push("#FBMODE{chip}".into()) }
        h.extend(self.voices.iter().cloned());
        h.extend(self.tables.iter().enumerate().map(|(i, t)| format!("#TABLE{}{{{t}}}", 254 - i)));
        h.extend(self.pcm.iter().enumerate().map(|(i, d)| format!("#PCMB{i}{d}")));
        h
    }
}
