# sion

A small player for SiON MML music. Give it MML text and it renders stereo audio. It has no dependencies and is
`no_std`, so the same code runs as a Rust crate, as WebAssembly in the browser, and as a C library.

- Output: interleaved stereo `f32` (left, right, left, right…) at 44,100 Hz.
- One *frame* is one left/right pair, so 44,100 frames are one second.
- The MML language itself is described in the
  [SiON MML reference manual](https://keim.github.io/SiON/mmlref/sion_mml_reference_e.html).
  [MML support](#mml-support) below lists what this player implements and where it differs.

## Contents

- [Quick start](#quick-start)
- [How playback works](#how-playback-works)
- [Rust API](#rust-api)
- [JavaScript API](#javascript-api)
- [C API](#c-api)
- [Building](#building)
- [MML support](#mml-support)

## Quick start

Every interface works the same way: create a song from MML, then ask it for audio a block at a time until it
says it has finished.

### Rust

```toml
[dependencies]
sion = { path = "../sion" }
```

```rust
use sion::{Song, RATE};

let mut song = Song::new("t120 %5@0 l8 cdefgab<c;");
let mut buf = vec![0f32; 2 * 1024]; // 1024 frames
while song.render(&mut buf) {
    // buf holds 1024 stereo frames at RATE Hz: send them to your audio output.
}
```

### JavaScript (browser, Node, Deno)

Copy `sion.wasm` and `sion.js` next to your page.

```js
import { Sion } from "./sion.js";

const sion = await Sion.load(fetch("sion.wasm"));
const song = sion.song("t120 %5@0 l8 cdefgab<c;");
const buf = new Float32Array(2 * 1024);
while (song.render(buf)) {
  // play buf (interleaved stereo, sion.rate Hz)
}
song.free(); // songs live in wasm memory: free them when done
```

### C / C++

Link against `libsion.so` (Linux) or `sion.dll` (Windows) and include `sion.h`.

```c
#include <string.h>
#include "sion.h"

const char *mml = "t120 %5@0 l8 cdefgab<c;";
SionSong *song = sion_song_new(mml, strlen(mml));
float buf[2 * 1024];
while (sion_song_render(song, buf, 1024)) {
    /* play buf (interleaved stereo, sion_rate() Hz) */
}
sion_song_free(song);
```

## How playback works

**Rendering.** Each `render` call fills the buffer and moves playback forward by that many frames. Any buffer
size works. Samples are clamped to -1..1.

**Declicking.** A key-on, volume change or pan change can make a track's output jump in the middle of a
wave: a waveform restarting from its first frame, or a hard pan flip. As in trackers, such a jump is
smoothed over: the output picks up where it would have been without the change, and the difference fades
out over about a millisecond. On a key-on, or when the volume comes up from silence, only the old
level fades out, so attacks stay sharp. Everything else is exactly SiON's output, and so is any track
that no such change touches. `set_declick(false)` turns it off.

**Ending.** `render` returns `false` once every track has ended and the output has been silent for half a
second, so release tails are not cut off. A song with a `$` loop point never ends: stop it yourself.

**Length.** `length` measures one pass through the MML, in frames, without rendering any audio. One pass ends
when every track has either finished or jumped back to its `$` once. It is capped at one hour. Release tails
are not included.

**Looping.** For a song with `$`, `loop_start` gives the frame playback returns to after `length` frames. Use
both to show a loop region or to seek within a looping song.

**Seeking.** `skip(frames)` moves forward without producing output. It jumps from event to event instead of
synthesizing every sample, so it is fast even for long songs. The result is close to, but not exactly, what
continuous playback gives: echoes from before the last quarter second are missing, and vibrato phases drift.
To seek backwards, create a new song and skip from the start.

**Following along.** `notes()` gives, per track, the byte offset in the MML text of the note playing now
(-1 for a rest or an ended track). Use it to highlight the playing notes in an editor. It needs the `spans`
feature (always on in the WebAssembly build).

**PCM samples.** Modules `%7` and `%10` play recorded samples. Embed them in the MML with `#PCMB`, or set them
from code with `set_sample`, which replaces any `#PCMB` of the same number. `#PCMWAVE`, `#PCMVOICE` and
`#SAMPLER` build instruments and drum kits out of them (see [Sample instruments and drum
kits](#sample-instruments-and-drum-kits)).

## Rust API

Everything is on `sion::Song`. The crate is not on crates.io, so add it as a path or git dependency.

| Item | Description |
| --- | --- |
| `RATE: f32` | Output sample rate: `44100.0`. |
| `Song::new(mml: &str) -> Song` | Parse MML. Never fails: anything it does not understand is ignored. |
| `Song::from_bytes(mml: &[u8]) -> Song` | Same, from bytes. Only the ASCII in the MML matters. |
| `song.render(out: &mut [f32]) -> bool` | Fill `out` with interleaved stereo. `false` once the song has finished. |
| `song.skip(frames: u64) -> bool` | Move ahead without output (see [Seeking](#how-playback-works)). `false` once finished. |
| `song.position() -> u64` | Frames rendered or skipped so far. |
| `song.notes() -> impl Iterator<Item = i32>` | Per track, the byte offset of the playing note, or -1. Needs the `spans` feature. |
| `song.set_sample(n: usize, s: Sample)` | Set PCM sample `n` (0..255) for `%7` / `%10`. |
| `song.set_declick(on: bool)` | Turn [declicking](#how-playback-works) on (the default) or off. Off, the output is exactly SiON's. |
| `Song::length(mml: &str) -> u64` | Frames in one pass through the MML, without rendering. |
| `Song::loop_start(mml: &str) -> Option<u64>` | Frame a `$` loop returns to after `length`. `None` if the song ends. |
| `Song::length_bytes`, `Song::loop_start_bytes` | `length` and `loop_start` for MML as bytes. |

`Sample` describes one PCM sample:

| Field | Description |
| --- | --- |
| `data: Vec<f32>` | Mono samples in -1..1. |
| `rate: f32` | Sample rate it was recorded at, in Hz. |
| `key: i32` | MIDI note that plays it at its original pitch (60 = middle C). |
| `lp: Option<usize>` | Frame to loop back to after the end, or `None` to play once. |
| `bidi: bool` | Loop back and forth instead of jumping back. |

```rust
use sion::{Sample, Song};

let mut song = Song::new("%7@0 o5 l4 c e g;");
song.set_sample(0, Sample { data: my_wave, rate: 22050.0, key: 60, lp: None, bidi: false });
```

## JavaScript API

`sion.js` wraps `sion.wasm`. It has no dependencies and works in browsers, Node and Deno.

### `Sion`

| Member | Description |
| --- | --- |
| `await Sion.load(source)` | Load the module. `source`: a `fetch` response, bytes, or a `WebAssembly.Module`. |
| `sion.rate` | Output sample rate (44100). |
| `sion.song(mml)` | Parse MML text into a `Song`. |
| `sion.length(mml)` | Frames in one pass through the MML, without rendering. |
| `sion.loopStart(mml)` | Frame a `$` loop returns to after `length`. -1 if the song ends. |

### `Song`

| Member | Description |
| --- | --- |
| `song.render(buf)` | Fill a `Float32Array` with interleaved stereo. `false` once the song has finished. |
| `song.skip(frames)` | Move ahead without output. `false` once finished. |
| `song.position` | Frames rendered or skipped so far. |
| `song.notes()` | Array with, per track, the UTF-8 byte offset of the playing note, or -1. |
| `song.setSample(n, data, rate, key, loopStart, bidi)` | Set PCM sample `n` (0..255). `data`: mono floats in -1..1. Optional: `key` (60), `loopStart` (-1 = no loop), `bidi` (false). |
| `song.free()` | Release the song's memory. Do not use the song afterwards. |

`notes()` offsets count UTF-8 bytes, not JS string characters. They only differ if the MML contains non-ASCII
text, such as comments in Japanese.

To play in real time, render inside an `AudioWorklet`. The web player's `worklet.js` and `player.js` show one way
to do it.

## C API

Declared in `sion.h`. All functions are safe to call with a `NULL` song or MML pointer.

| Function | Description |
| --- | --- |
| `uint32_t sion_rate(void)` | Output sample rate (44100). |
| `SionSong *sion_song_new(const char *mml, size_t len)` | Parse `len` bytes of MML. The text need not end in NUL. |
| `void sion_song_free(SionSong *song)` | Free a song. |
| `int32_t sion_song_render(SionSong *song, float *out, size_t frames)` | Fill `out` with `2 * frames` floats. 1 while playing, 0 once finished. |
| `int32_t sion_song_skip(SionSong *song, uint64_t frames)` | Move ahead without output. 1 while playing, 0 once finished. |
| `uint64_t sion_song_position(const SionSong *song)` | Frames rendered or skipped so far. |
| `uint64_t sion_length(const char *mml, size_t len)` | Frames in one pass through the MML. |
| `int64_t sion_loop_start(const char *mml, size_t len)` | Frame a `$` loop returns to after `sion_length`. -1 if the song ends. |
| `void sion_song_set_sample(song, n, data, frames, rate, key, loop_start, bidi)` | Set PCM sample `n` (0..255) from `frames` mono floats. Negative `loop_start` = no loop. Nonzero `bidi` = back-and-forth loop. See `sion.h` for the argument types. |

A song is not thread-safe: use each `SionSong` from one thread at a time. Separate songs are independent.

## Building

The crate builds on stable Rust. The release scripts need nightly with `rust-src`, which they use to make the
outputs small.

| Command | Output |
| --- | --- |
| `cargo build --release` | The Rust library. |
| `cargo test` | Runs the tests in `tests/`. |
| `./build-wasm.sh` | `target/wasm32-unknown-unknown/wasm/sion.wasm`. Runs `wasm-opt` too if Binaryen is installed. |
| `./build-ffi.sh [TARGET]` | `target/TARGET/ffi/libsion.so`, or `sion.dll` + `libsion.dll.a` for a Windows target. |

Cargo features:

| Feature | Description |
| --- | --- |
| `spans` | Track which note in the MML each track is playing (`Song::notes`). Slightly slower parsing. |
| `small` | Nightly only. For size-optimized builds: keeps the per-sample code optimized for speed. |
| `wasm` | The WebAssembly exports. Used by `build-wasm.sh`; not for use as a dependency. |
| `ffi` | The C exports. Used by `build-ffi.sh`; not for use as a dependency. |

## MML support

This section is a summary. For each command's full meaning and parameters, see the
[SiON MML reference manual](https://keim.github.io/SiON/mmlref/sion_mml_reference_e.html).

### Structure

- Statements end with `;`. Each statement that does not start with `#` is one track. All tracks play at once.
- Comments: `// to end of line` and `/* block */`.
- Unknown commands are skipped, so MML written for full SiON still plays, minus the parts this player lacks.

### Notes and timing

| Command | Meaning |
| --- | --- |
| `c d e f g a b` | Notes. Add `+` or `#` for sharp and `-` for flat, then an optional length: `c+8`, `e-4.`. |
| `r` | Rest. |
| `4`, `8.`, `16..` (after a note) | Length: a note value, with optional dots. |
| `^` | Tie: adds a length to the previous note (`c4^16`). |
| `&` | Slur: the next note changes pitch without re-attacking. |
| `&&` | Hold the note into the next one, which re-attacks. |
| `*` | Glide: bend from this note to the next over this note's length. |
| `l` | Default note length (`l8`). |
| `o`, `<`, `>` | Octave (`o5` is the default), up one, down one. |
| `t` | Tempo in BPM (`t120`). |
| `q` | Gate: the share of each note that sounds, in eighths (`q6`, default 6). |
| `@q` | Gate in absolute time: shorten notes, delay key-on. |
| `[ ... \| ... ]n` | Repeat `n` times (default 2). The part after `\|` is skipped on the last time. |
| `$` | Loop point: when the track ends, it jumps back here forever. |
| `kt`, `!@ns` | Transpose in semitones. |
| `k` | Detune, in 1/64 semitones. Fractions are allowed (`k-12.375`). |
| `!@kr` | Detune relative to the current value. |
| `po` | Portamento: slide between slurred notes over `n` frames. |

### Volume and panning

| Command | Meaning |
| --- | --- |
| `v` | Volume, 0..16 (default 16). |
| `(`, `)` | Volume up / down by one step. |
| `x` | Expression (a second volume), 0..128. |
| `%v`, `%x` | Volume curve used by `v` and `x`. |
| `@v` | Volume 0..128, then up to 7 effect send levels (`@v64,32` sends half to `#EFFECT1`). |
| `p` | Pan, 0 (left) to 8 (right); 4 is the center. |
| `@p` | Fine pan, -64 (left) to 64 (right). |
| `@mask` | Ignore later commands of some kinds (bits: 1 volume, 2 pan, 4 gate, 8 tone, 16 envelope, 32 modulation, 64 slur). |

### Sound modules

`%n` picks the sound source for the track, and `@n` picks a tone within it.

| Module | Sound |
| --- | --- |
| `%0` | PSG: square wave and noise. |
| `%1` | NES APU: pulse waves, triangle, noise. |
| `%2` | Noise. |
| `%3` | Yamaha MA-3 waveforms. |
| `%4` | Custom wavetables from `#WAV`, `#WAVB` or `#WAVCOLOR`. |
| `%5` | All built-in waveforms (`%5@0` is a sine wave). |
| `%6` | FM voice defined with `#@`, `#OPM@`, `#OPN@`, `#OPL@`, `#OPX@`, `#MA@` or `#AL@`. |
| `%7` | PCM sample: `%7@n` plays `#PCMB` n, or instrument n if `#PCMWAVE` / `#PCMVOICE` define one. |
| `%8` | Pulse wave with adjustable duty. |
| `%9` | Ramp wave. |
| `%10` | Sampler: each key plays its own sample at its own rate (a drum kit). `%10@n` picks bank n (0..3) of `#SAMPLER` keys; without any, key n plays `#PCMB` n. |
| `%11` | Plucked string (Karplus–Strong). |
| `%12` | Game Boy. |
| `%13` | Konami VRC6. |
| `%14` | Commodore SID. |

### Tone shaping

| Command | Meaning |
| --- | --- |
| `@` | Tone number, then optional envelope: `@tone,ar,dr,sr,rr,sl,tl,…`. |
| `s` | Release rate, then an optional pitch sweep during release. |
| `@rr` | Release rate of the carriers. |
| `@al`, `@fb` | FM algorithm (operator count, connection) and feedback. |
| `i` | Select which FM operator the next operator commands change. |
| `@tl`, `@ml`, `@dt`, `@fx`, `@ph`, `@se`, `@er` | Per-operator total level, multiple, detune, fixed pitch, start phase, SSG envelope, envelope reset. |
| `@ph` on `%7` / `%10` | Start frame (fractions and negative values allowed), then optionally how many frames to play. |
| `@f`, `%f` | Resonant filter with its own envelope; `%f` picks low-pass (0), band-pass (1) or high-pass (2). |
| `@lfo` | LFO speed and waveform for `mp` / `ma`. |
| `mp`, `ma` | Vibrato (pitch) and tremolo (amplitude) depth, optionally fading in. |
| `@clock` | Scale the FM operators' clock, in percent. |
| `@fps` | Envelope table speed for this track, in steps per second. |
| `@i`, `@o`, `@r` | Pipes: route one track's output into another as FM input or ring modulation. |

### Envelope tables

Define a table with `#TABLEn{...}`, then apply it per step to a note:

| Command | Applies the table to |
| --- | --- |
| `na` | Amplitude. |
| `nt` | Note (in semitones). |
| `np` | Pitch (in 1/64 semitones). |
| `nf` | Filter cutoff. |
| `@@` | Tone number. |

Prefix with `_` (`_na`) for the release part of the note. `na` values set the expression (`x`, 0..128) itself;
`!na` adds them to the track's `x`. A table can also be written inline: `na{0,8,16}`. Table syntax supports
`(a,b)n` interpolation, `[...]n` repeats, a `|` loop point and a `*scale+offset` suffix.

As in SiON, a table's first value applies at the key-on (or key-off, for `_` tables) and each next one an
envelope frame later (`#FPS`, `@fps`: 735 samples at the default 60), and values are read as `(int)(v + 0.5)`, so
a negative value comes out one higher (`-10` plays as -9; write `-11` for -10).

### Definitions (`#` statements)

| Statement | Meaning |
| --- | --- |
| `#A=...;` | Macro: `A` in a track is replaced by the text. `A(3)` plays it transposed. Also `#A+=`, `#ABC=` and `#A-C=`. |
| `#MACRO{dynamic};` | Expand macros where they are used rather than where they are defined. |
| `#@n{...}` and chip formats | FM voice `n`: `#@`, `#OPM@`, `#OPN@`, `#OPL@`, `#OPX@`, `#MA@`, `#AL@`. Text after `}` runs when the voice is selected. |
| `#TABLEn{...}` | Envelope table `n` (0..254). |
| `#WAVn{...}` | Wavetable `n` for `%4`, as values -128..127. |
| `#WAVBn{...}` | Wavetable `n` as hex bytes. |
| `#WAVCOLORn{hex}` | Wavetable `n` from a wave color: hex digits, lowest first, set the levels (0-f) of harmonics 1, 2, 3, 4, 5, 6 and 8; an 8th digit picks the base wave (`%5@n`, sine by default). Also `#WAVC`. |
| `#PCMBn{data}rate,key,loop,flags,level` | PCM sample `n`: see below. |
| `#PCMWAVEn{sample,note,from,to,,start,end,loop}` | Instrument `n` for `%7`: keys `from`..`to` play `#PCMB` `sample`. See below. |
| `#PCMVOICEn{...}commands` | Envelope, key scaling and commands for instrument `n`. See below. |
| `#SAMPLERn{sample,hold,pan,,start,end,loop}` | `%10` drum kit key: see below. |
| `#EFFECTn{...}` | Effect chain: see below. |
| `#FM{...}` | Wire the following tracks into one FM sound (`#FM{B3(A)}`: A modulates B). |
| `#SIGN{...}` | Key signature: `D`, `F#m`, `Bb`… or a list like `c+ f+`. |
| `#TMODE{unit=n \| timerb=n \| fps=n}` | How `t` values are read. |
| `#VMODE{n88 \| mdx \| psg \| %v.. \| %x..}` | Volume curves, matching other MML dialects. |
| `#QUANT{n}` | Steps for `q` (default 8). |
| `#FPS{n}` | Envelope table speed for all tracks (default 60). |
| `#REV` | Swap the direction of `<` `>` and `(` `)`. `#REV{oct}` or `#REV{vol}` swaps only one. |
| `#FBMODE{chip}` | Chip-style FM feedback. |
| `#END` | Stop reading here. |

### Sample instruments and drum kits

`#PCMB` holds the audio. These three statements say how to play it, and refer to a `#PCMB` by its number, so
one sample can be used by several instruments without being stored again. They can come before or after the
`#PCMB`s they use. Arguments can be left out (`#PCMWAVE0{3,,0,59}`): each one has a default.

`#PCMWAVEn{sample,note,from,to,channels,start,end,loop}` gives `%7` instrument `n` (0..255) a sample for a
range of keys. Several of them with different ranges make one instrument play different samples on different
keys (a multisample), and switching between them needs no `@n`:

| Argument | Meaning (default) |
| --- | --- |
| `sample` | The `#PCMB` number. |
| `note` | The note at which the sample plays at its `#PCMB` rate (the `#PCMB`'s key). Fractions are allowed. |
| `from`, `to` | The keys that play it (0, 127). `to` -1 means just `from`. Keys outside every range are silent. |
| `channels` | Ignored: samples are mono. |
| `start`, `end` | The first frame, and the frame to stop before, counted in the `#PCMB` (all of it). |
| `loop` | The frame to loop from, counted in the `#PCMB` (the `#PCMB`'s own loop). -1: no loop. Below -1, such as -500: loop the last 500 frames. |

`#PCMVOICEn{volume note,volume range,volume amount,pan note,pan range,pan width,ar,dr,sr,rr,sl}commands`
sets how instrument `n` plays:

- Volume key scaling (64, 0, 0): over `range` keys centered on `note`, the level rises by `amount` / 128.
  Keys below the range are that much quieter, keys above are at full level. A negative amount turns this
  around.
- Pan key scaling (64, 0, 0): over the same kind of range, pan moves from `-width / 2` to `+width / 2`,
  added to the track's pan. A width of 128 spreads the keys from left to right.
- `ar`, `dr`, `sr`, `rr`, `sl` (63, 0, 0, 63, 0): the envelope, as in `@n,ar,dr,sr,rr,sl`.
- Commands after `}` run each time the instrument is chosen, as they do for FM voices.

A `#PCMVOICE` without a `#PCMWAVE` plays `#PCMB` n on every key.

`#SAMPLERn{sample,hold,pan,channels,start,end,loop}` makes key `n % 128` of `%10` bank `n / 128` play a
sample, at its own rate, whatever the pitch. `hold` 1 plays the sample to its end after key-off (0). `pan`
(-64..64) is added to the track's pan (0). `channels`, `start`, `end` and `loop` are as for `#PCMWAVE`. In a
bank that has any `#SAMPLER`, keys without one are silent.

```
#PCMB0{...}22050,48;  #PCMB1{...}22050,72;   // a low and a high recording of the same instrument
#PCMWAVE0{0,,0,59};   #PCMWAVE0{1,,60,127};  // instrument 0: sample 0 below o5c, sample 1 from there
#PCMVOICE0{,,,,,,63,0,0,40,0};               // with a slower release
#PCMB2{...}; #PCMB3{...};
#SAMPLER36{2}; #SAMPLER38{3,1};              // kit: kick on o3c, snare on o3d (playing on through key-off)
%7@0 l8 o4 cegb<ceg;
%10 l8 o3 cdcd;
```

### Effects

`#EFFECT0` is the master chain, which everything passes through. `#EFFECT1` to `#EFFECT7` are send effects,
fed by `@v` send levels. Units run in order. After `}`, `@v`, `p` and `@p` set the effect's return level and pan.

```
#EFFECT0{ eq 100,100,120 };
#EFFECT1{ reverb 70,40,80,100 } @v64;
%5@0 @v100,40 l8 cdefgab<c;
```

| Unit | Parameters (defaults) |
| --- | --- |
| `delay` | time ms (250), feedback % (25), cross 0/1 (0), wet % (100) |
| `reverb` | delay 1 % (70), delay 2 % (40), feedback % (80), wet % (100) |
| `chorus` | delay ms (20), feedback % (20), rate Hz (4), depth (20), wet % (50), invert (1) |
| `autopan` | rate Hz (1), width % (100) |
| `dist` | pre-gain dB (-60), post-gain dB (18), low-pass Hz (2400), slope (1) |
| `ws` | wave shaper: amount % (50), output % (100) |
| `comp` | threshold % (70), window ms (50), attack ms (20), release ms (20), max gain dB (6), mix % (50) |
| `ds` | down-sampler: rate shift (0), bits (16), channels (2) |
| `speaker` | hardness % (20) |
| `stereo` | invert (0), width % (140), rotation % (0) |
| `eq` | low %, mid %, high % (100 each), low/mid split Hz (880), mid/high split Hz (5000) |
| `lf` `hf` `bf` `nf` `pf` `af` `lb` `hb` | low-pass, high-pass, band-pass, notch, peak, all-pass, low shelf, high shelf: frequency Hz, bandwidth (1), gain dB (6) |
| `nlf` `nhf` | low/high-pass swept by tables: cutoff table, resonance table, steps per second (20) |
| `vowel` | output % (100), then six formant frequency/gain pairs |

### Differences from SiON

Additions:

- `#PCMBn{data}rate,key,loop,flags,level` embeds a PCM sample in the MML text. `data` is base64url of signed
  little-endian samples: 8-bit, or 16-bit with flags 1. Flags 2 loops back and forth. Flags 4 decodes NES DPCM
  starting from `level`. `{=m}` reuses the data of earlier DPCM sample `m`. Defaults: rate 44100, key 60, no loop.
- `@ph` on PCM modules takes a fractional or negative start frame and a play length.
- `k` accepts fractions.

`#SAMPLER`, `#PCMWAVE` and `#PCMVOICE` use SiON's names and arguments, with one difference: where SiON
expects the name of a sound, they take a `#PCMB` number. In SiON these statements hold no audio. The braces
name a sound that the program running SiON loaded and registered under that name. An MML file can't carry
the sound, so a song that uses them only plays inside that program. Here the audio is in the file, in
`#PCMB`, and these statements point at it, so the whole song is one MML file. (In SiON, registering sounds
under the names `0`, `1`, … would make the same text work.) The rest differs only in small ways:

- Without a `note`, `#PCMWAVE` uses the `#PCMB`'s own key, because `#PCMB` already says what pitch the
  sample is at. SiON's default is 69 (`o5a`). Fractional notes are allowed.
- A left-out `start` is the first frame and a left-out `end` is the last. SiON skips quiet frames at the start
  and end by itself.
- A left-out `loop` keeps the `#PCMB`'s loop. A tail loop (below -1) doesn't crossfade.
- `channels` is ignored, because samples are mono.
- `%7` has 256 instruments (SiON has 128).

Playback differs in one way: jumps from key-ons and volume and pan changes are smoothed over (see
[Declicking](#how-playback-works)).

Not supported: event triggers (`%t`, `%e`), which are skipped.
