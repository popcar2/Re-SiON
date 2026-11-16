# sion-converter

Converts other music formats to SiON MML: Standard MIDI Files, tracker modules (MOD, S3M, XM, IT),
VGM/VGZ, GYM and S98 chip music logs, NSF/NSFE/NSF2 NES music, GBS Game Boy music and SPC SNES music. The format is recognized from the file's contents.

Based on [SiON by keim](https://github.com/keim/SiON) (MIT), while referencing [GDSiON by Yuri Sizov](https://github.com/YuriSizov/gdsion) (MIT) for correctness.

```
cargo run --release -- song.mid              # writes song.mml
cargo run --release -- song.xm -o out.mml --verify
cargo run --release -- song.vgz --wav out.wav
cargo run --release -- game.nsfe --track 3
cargo run --release -- game.gbs --track 2
cargo run --release -- song.spc
cargo run --release -- song.gym              # or .s98
```

Options: `-o FILE`, `--grid N` (snap timing to 1/N notes: smaller output, but no longer exact),
`--wavetables` (tracker modules: approximate samples instead of embedding them; see below),
`--exact` (tracker modules: tick-by-tick, minified, instead of written as by hand; see below),
`--track N` (NSF/NSFE/GBS: which song of the file, from 1; default: the file's first song),
`--exact-pcm` (VGM and NSF: capture chip samples exactly, frame by frame; much larger, see below),
`--gain G` (volume; default is automatic, so the busiest chord doesn't clip), `--plain` (write the plain MML, one
command per event, instead),
`--verify` (render the plain and the written MML with the `sion` crate and check that every sample is identical;
for a tracker module with `--exact`, also compare the result with the module mixed straight from its samples),
`--wav FILE` (also render the result to a WAV file). `sion-convert song.mml --wav out.wav` renders any MML file.
Anything a file uses that can't be converted is reported as a `note:` on stderr.

Every reader produces the same format-neutral score (`src/score.rs`), which `src/arrange.rs` turns into tracks
and `src/pretty.rs` lays out the way it would be written by hand (below), so all formats get the same timing,
volume and layout handling. Each line is one section of the source: a tracker pattern, or a bar for the others
(MIDI by its time signatures, 4/4 without one; VGM, NSF and SPC 16 rows, lined up with the loop point). When the
automatic volume would still clip (FM voices and some waveforms come out louder than estimated), the result is
rendered and the master volume (`x`) is lowered until it doesn't.

Per-frame changes become envelope tables where they play exactly the same (`Arranged::envelopes` in
`src/arrange.rs`): when the song keeps one tempo and its notes' volume and detune changes fall on a grid of whole
samples that SiON's table clock can step on (`#FPS`, 60 by default: a 1/60 s driver frame is 735 samples), each
note's run of levels becomes an `na` table and its run of detunes an `np` table, instead of a slur per frame
(`v16e-&v20e-24&v16e-32&v13e-32&...` → `na12 e-8`). SiON starts a table at the key-on and steps it every frame,
which is where the driver changed the chip. Notes share a table when it plays their values for as long as they
sound, a run that settles into a repeating figure (a vibrato) loops it (`|`), and a note keeps the table before it
when that one plays its values too. A pitch table is used only where it is shorter to write than its slurs, and
the tables used by the most notes get the shortest numbers. The frame is the one most notes' changes fall on. A note
keeps its slurs where a table would play otherwise: one sounding through the loop point (going back, it is keyed
on afresh there, and its table would start over), one whose other changes split it between frames (a slurred
note starts from its own pitch until the next frame), and a sample keyed on between two output frames.

- Volume tables set `x`, which multiplies with `v` (or adds, in decibels): a track with them has its master volume
  as its one `v` and the notes' levels in its tables, the same product (in decibels with `#VMODE{%x1}`, so `x`
  steps as `v` does). A release the driver silences is a key-off table (`_na`: the level held, then 0, however
  long the release), so `v`
  never changes under a releasing note. A track gets volume tables only if all its notes can (levels up to 128,
  none sounding through the loop point or silenced across it), and `x` is in decibels only for a song with one.
- `--verify` checks the tables against the plain MML, which plays the same runs as slurs (with the levels in `x`).
- VGM songs whose driver runs on the 60 Hz frame get them, and on the 50 Hz one unless they have release pitch
  sweeps (`_np` tables, which step at 1/60 s); tracker modules that keep one tempo get pitch tables on their ticks
  (`#FPS{50}` at 125 BPM). NSF (the NES frame is 733.8 samples), SPC, MIDI and songs timed by the YM2612's timers
  don't (their frames aren't whole samples), and keep the slurs.

## WebAssembly

`./build-wasm.sh` builds the converter as `convert.wasm` (needs the nightly toolchain with the
`wasm32-unknown-unknown` target; runs `wasm-opt` if Binaryen is installed). The exports are in `src/wasm.rs`, which
is only compiled for wasm32; `convert.js` wraps them for the browser, Node and Deno:

```js
const conv = await Converter.load(fetch("convert.wasm"));
const r = conv.convert(fileBytes, { grid, gain, wavetables, exact, exactPcm, track });
```

## MIDI

- Timing stays exact. When the MIDI resolution doesn't divide SiON's 480 ticks per quarter, the converter counts in MIDI ticks and scales the tempo instead.
- Each channel is split into monophonic tracks, one per simultaneous voice. Handles the sustain pedal, velocity × CC7 × CC11 (`v1`..`v16`, plus a master `x`), pan (`@p`), and pitch bend with RPN range (`k`, with mid-note bends written as `&` slurs). Tempo changes become `t`.
- Instruments: GM program families map to the NES APU module (`%1@2` 25% pulse, `%1@4` 50% pulse, `%1@8` triangle),
  each with an envelope shaped like the GM instrument's (`%1@2,63,0,12,38,0`: attack, fade while held, release), so
  short notes stay short instead of ringing on in SiON's default half-second release.
- Balance: velocity, CC7 and CC11 follow the GM loudness curve (squared), and every program has a gain (`PROG_DB` in
  `src/midi.rs`, measured against a GM sound font by loudness as heard: K-weighted, ITU-R BS.1770) applied through its tracks' `x`, so bass, lead and drums sit
  in the mix as a GM synthesizer plays them. Each program a channel uses gets its own tracks.
- Drums (channel 10): kick and toms are sine waves that drop 4–8 semitones within a frame or two (`%5@0` + `np`
  table) and hold their body before a fast fade (`%5@0,63,26,42,42,1`: a kick is gone in about 110 ms); the kick
  adds a few milliseconds of noise for the beater's click; snares layer white noise over a short swept tone; hats,
  cymbals and ride use high-passed noise (`%2@3`), each with its own decay. Each sound was fitted hit by hit
  against a GM sound font (level over time, spectrum and loudness). Each drum type gets its own track. To change
  sounds, edit `instrument()` and `drum()` in `src/midi.rs` (render with `--wav` to listen).

## Tracker modules (MOD, S3M, XM, IT)

`src/tracker.rs` loads ProTracker MODs (and the 6/8/xx-channel variants), Scream Tracker 3, FastTracker 2 and
Impulse Tracker files (including IT-compressed samples); `src/play.rs` performs the song tick by tick, the way a
tracker does, and records what each channel sounds like on every tick:

- Rows, speed, tempo (and tempo slides), pattern jumps, breaks, loops and delays. The song ends where it would loop, and the MML loops (`$`) back to there: a jump
  back to a row already played, or the end of the order list going back to the restart position (XM and MOD; otherwise
  the first order).
  A quarter note is four rows at the song's starting speed, so a tracker tick is one score tick and timing is exact.
- Effects: arpeggio, portamento (coarse, fine, extra fine, tone portamento), vibrato, tremolo, tremor, volume,
  channel and global volume slides, panning and pan slides, sample offset, retrigger, note cut and note delay,
  key off, the volume column, Amiga and linear frequency slides, and each format's effect memory.
- Instruments: XM/IT note→sample maps, volume and panning envelopes (sustain and loops), fadeout, and IT new-note
  actions (notes that ring on after the next one starts get tracks of their own).
- Pitch (`k`) and volume (`v`) changes during a note are written as `&` slurs.

By default a module is written the way someone would write it by hand (`src/pretty.rs`), as every format is:

```
// Track 2, channel 2: Melody11
#A=v7 o5g4 b-8<c8& v4c8v7d4>b-8&;
#B=v7 o5b-4.^16&[v6b-&v7b-&]4v6b-;
q8 x90 %4@0s63 l16
[A B]
v7 o5b-4.<c4. d4&
...
$
[A B C D E F C D]
[E F C D]4;
```

- One line per pattern, as the order list plays it. Each line starts with an absolute octave and restates the
  volume, pan and detune, so it means the same wherever it plays; the tone (`%`, `@n`) is written where it changes.
  (A line that starts partway into a note, or with a rest, restates the volume just before its first key-on: set
  while a note or its release is still sounding, it would set it there, where a volume table may have taken it.)
- Patterns that follow each other again are loops (`[A B]`, `[A B|C]3`); a pattern heard again elsewhere is a
  macro, defined just before its track. Within a line: one default length per track, relative octaves, a space
  per beat, short repeated figures as loops, black keys as flats in flat keys.
- Volume and pan are taken once per row (slides and envelopes become steps per row), volume in 16 steps, and
  detune in whole 1/64 semitones. Notes are whole rows: a one-shot sample that plays out lasts to the end of
  its row. Pitch changes (arpeggio, slides, vibrato) are still followed tick by tick.
- A sample that is one cycle of a waveform (all loop, at most 1024 frames) becomes that wavetable (`#WAV`, exact);
  other samples are embedded (below). Wavetables and samples are written at the end of the file.
- A note is cut at a line's end only where that sounds the same, as a slur (`c&` / `c`) into the next line: not
  under a pitch table (`np`; a slurred note starts from its own pitch, not where the table has taken it), except
  where the line's end falls on one of the table's frames (it steps there anyway), or a gate other than `q8` (drum
  hits: each piece would be keyed off at its own gate). Those notes run on into the next line.
- Data is written compactly, decoding to exactly the same values: runs of one value in `#WAV` and `#TABLE` (on
  each side of a table's loop point) as
  `(v)n` (which SiON and GDSiON both read as `n` copies; GDSiON crashes on `[v]n`, and `(v,v)n` rounds negative
  values differently), and `#PCMB` arguments at their defaults left off the end.

`--exact` instead performs everything tick by tick and minifies the result, as described below. That is the
version that sounds exactly like the module; the default trades small details (a slide's steps within a row, a
volume's fine steps) for MML that can be read and edited.

With `--exact`, samples are embedded in the MML as they are (`#PCMB`, see below) and played with `%7`, so the
conversion sounds like the module itself:

- 8-bit samples stay 8-bit and 16-bit ones 16-bit, so the data is exact; frames after the loop end are left out.
  Forward and ping-pong loops play as in the tracker.
- Every tracker tick's pitch, volume and pan carry over: pitch to 1/1000 of SiON's 1/64-semitone step (a fractional
  detune, `k-12.375`, so Amiga periods and vibrato come out exact too), volume in 256 steps (`#VMODE{%v0,0}`,
  `v0`..`v256`).
- A note that starts partway into a sample says where (`@ph`, to 1/1000 frame): sample offset (`9xx`/`Oxx`), IT
  background voices, and notes that come back after a stretch of zero volume. A note keyed on at zero volume
  still starts then, as the sample does.
- A note that is cut goes silent on that tick (SiON's fastest release would otherwise ring for ~5 ms).

`--verify` checks this: it mixes the module straight from its samples (linear interpolation, SiON's pan law) and
compares SiON's rendering of the MML with it. On 23 MOD, S3M, XM and IT files the result was 50-77 dB of signal
over the difference, mostly 55-70 (one 58-channel IT file: 40 dB); 50 dB means the difference is about 0.3% of the
signal. Note that this checks the conversion against the converter's own performance of the module, which follows each tracker's rules
but is not a bit-exact copy of any of them (IT filters, for one, are not performed at all).

The cost is size: the sample data is base64 text, a third larger than the samples themselves (a 52 KB MOD became
66 KB of MML, against 25 KB with `--wavetables`; a 3.3 MB IT became 4.2 MB, against 70 KB). SiON has 256 sample
slots; samples past that, and every sample with `--wavetables`, are turned into the closest sound SiON can play
without one (`src/sample.rs`):

- A loop of up to 1024 frames (the usual chiptune sample: one cycle of a square, saw or custom wave) becomes that
  wavetable (`#WAV`, played with `%4@n`) at exactly the same pitch.
- A longer periodic sample becomes one detected cycle of it as a wavetable. A one-shot sample also gets a decay
  envelope fitted to how the sample fades (`@n,63,dr,0,63,15`).
- A sample without a clear period (drums, effects) becomes white noise (`%2@0`) at a rate that matches its
  brightness; a very dull one (like a kick drum) becomes a low tone.

Chiptune modules come out close to the original that way; modules built from recorded instruments keep their notes
and rhythm but each sample is reduced to one waveform or noise.
S3M AdLib (FM) instruments are not converted.

## VGM / VGZ

`src/vgm.rs` replays the register writes into models of the sound chips (`src/chips.rs`, `src/fm.rs`), samples
every channel once per sound-driver frame (the rate is detected from the file), and turns those frames into notes.
The tempo is chosen so the song's most common row length becomes a 1/16 note.

A VGM with a loop point loops forever: every track gets a `$` there and plays to the song's end, where it sets its
voice, volume, pan, detune, octave, default length and gate back to what they were at the `$` before going round. A
note sounding through the loop point is split there (`c&$c`), so the first pass doesn't re-attack it. `--wav` and
`--verify` render such a song with its loop played twice.

| Chip | Systems | Converted as |
| --- | --- | --- |
| SN76489 | Master System, Game Gear, Mega Drive | squares (`%1@4`), noise (`%2@1`), periodic noise (`%8@1`), Game Gear stereo |
| AY-3-8910 / YM2149 | MSX, ZX Spectrum, Atari ST, CPC | squares, noise; the hardware envelope as a volume change, or as a saw/triangle "buzzer" tone |
| NES APU | NES / Famicom | pulses with duty (50% as `%1@4`, the others as `#WAV` tables, below), triangle (`%1@8`), noise (`%1@9`/`@10`); envelopes, sweeps, length and linear counters; DPCM and `$4011` samples |
| Game Boy | Game Boy | pulses with duty, the wave channel as `#WAV` tables, 15/7-bit noise; envelopes, sweep, length, panning |
| YM2612 / YM3438, YM2203, YM2608, YM2610 | Mega Drive, PC-88/98, Neo Geo | FM voices (`#OPN@`, `%6@n`) taken from the patch registers, plus the SSG part as an AY-3-8910; DAC (YM2612), ADPCM-A (YM2610) and ADPCM-B (YM2608, YM2610) samples |
| YM2151 | X68000, arcade boards | FM voices (`#OPM@`) |

Tone channels (the PSGs, NES, Game Boy):

- A pitch change without a key-on (arpeggios, slides, vibrato) carries the note on, slurred (`c&e`, with `k` for
  the fraction), and so does a change of waveform within the module (a pulse's duty, the VRC6 saw's rate: `@n` in
  the slur): the chips' tone generators keep running through both, while a new note in SiON restarts its
  waveform, which every frame of an arpeggio is a buzz at the driver's frame rate. The NES triangle and noise
  never restart (writing `$400B`/`$400F` reloads counters and the envelope only); the NES pulses restart when
  `$4003` is written, the SN76489 and Game Boy noise when their registers are written.
- Pulses other than 50% are 16-step wavetables whose two levels balance around zero (12.5%: `126` against `-18`,
  for 1/8 and 7/8 of the period), at a full pulse's peak-to-peak swing. SiON's pulses swing from -full to +full, so a narrow one sits
  mostly at -full: a DC offset that would follow every volume step. The chips' pulses rise from 0 to their volume,
  and the systems' output high-pass filters take their DC out.

FM patches become SiON voices with the carriers' total level taken out as the note volume, so fades and the same
patch at different volumes share one voice. The song is played once, without its loop.

- The OPN LFO (YM2612, YM2608, YM2610) becomes SiON's: its rate as `@lfo` (whole 1/60 s frames; a rate that
  rounds badly becomes up to 8 cycles of SiON's triangle in one custom waveform, `@lfo71,254` with a `#TABLE`, to
  within 0.5%), each channel's vibrato depth (PMS) as `mp` and tremolo depth (AMS) as `ma` on the operators that have
  amplitude modulation on (`a1` in the voice). Depth changes during a note (a driver's delayed vibrato) are written
  mid-note, and turning the LFO off is `mp0`/`ma0`.
- Volumes are in decibels (`#VMODE{%v1,1}`, with `%x1` where volume tables need `x` in decibels too: `v0`..`v128`
  in 0.75 dB steps, the YM2612's own total-level step),
  so FM levels are exact and the PSG's 2 dB steps within 0.4 dB; songs with samples keep 256 linear steps,
  which a sample needs to come out as the chip played it.
- Mega Drive songs (YM2612, SN76489) get a high shelf on the master, `#EFFECT0{hb9000,1,-3}`: libvgm, which
  plays the chips at their own rates and resamples, comes out about 3 dB down above 9 kHz from SiON's 44.1 kHz
  rendering (measured on two songs).
- An FM note keeps sounding after its key-off, through its release, and drivers often change it meanwhile: a
  pitch sweep during the release (tom and kick drums keyed on for a few milliseconds and swept down) becomes a
  key-off pitch table (`#TABLE`, `_np`), and carriers turned all the way down end the release there (`v0`).
  A key-on and key-off within one frame still play a note. The release is followed for as long as it can be
  heard (45 dB down at the carriers' release rate, at most two seconds); the next note's pitch, set just before
  its key-on, is left out.

Levels and timbres are checked against libvgm (the vgmrips player) on test tones and on real songs:

- Chips are mixed as libvgm mixes them: the Mega Drive's PSG 12 dB under a full-scale FM channel; the SSG part
  6 dB under it on the YM2203 and level with it on the YM2608 and YM2610. A full-scale YM2612 DAC sample matches
  a full-scale FM channel, as do the YM2610's ADPCM-A and -B at their levels.
- The chips pan by switching each side on or off, so a channel on one side is as loud there as a centred one;
  SiON pans at constant power (0.71 per side in the centre), so one-sided notes are written 3 dB down.
- Noise that shifts faster than the output rate (SN76489, AY-3-8910 and Game Boy noise) keeps only its share of
  power below 22 kHz, as the systems' output filters leave it (the Mega Drive's fastest noise: 37%, -4.3 dB);
  SiON's noise would put all of its power in band.
- FM feedback: the chips feed S1 the sum of its last two outputs, SiON the last one alone, so the chips' level
  is written one higher (1..7 → 2..8) and the file asks for the chips' kind of loop with `#FBMODE{chip}` (below).
  Without it (in SiON itself), deep feedback on a loud operator turns to noise sooner than on the chip.
- Not matched: a channel whose carriers add up past full scale clips on the chip and not in SiON.

Samples become embedded samples (`#PCMB`, `%7`; see below). Each note starts at the exact frame the chip started
it (keyed on at the tick before, with a negative `@ph` for the moment of silence until then), and a note that stops
partway into its sample plays just its frames (`@ph start,length`). Each sound is stored once, at its own rate:

- YM2612 DAC bytes a driver streams from the VGM's sample bank (`0x8n`, DAC streams `0x90`-`0x95`) are stored as
  the bank: each stretch of it that is played is one sample, and every hit plays it from where it starts reading,
  at the rate it reads. Drivers write with a few frames of jitter (Sonic's every 2 to 7 frames); that is left out.
- Values written directly (YM2612 DAC register writes, NES `$4011`) are sampled at the run's usual write rate:
  each write lasts as many frames as its gap spans at that rate, so a pause in the writing still holds the value.
- NES: DPCM samples are taken from the program memory in the VGM (`0xC2` blocks) and stored as they are, 1 bit a
  step (`#PCMB` flags 4, below), with the level the DMC was at when each started: a sample decodes differently
  from another level, where it reaches the 7-bit level's floor or ceiling. They play at the DMC rate (a `$4010`
  change bends a playing one), all on one track: the channel keeps the DMC's level between samples, so a sample
  stopped partway (`$4015`) leaves it where it got to and the next goes on from there, as on the console, with
  no click where one ends.
- YM2610 ADPCM-A (six channels, at clock / 432) and ADPCM-B (pitched, repeating), YM2608 ADPCM-B: decoded from the
  ROM blocks (`0x81`-`0x83`) as the chips decode them, kept in 8 bits scaled to each sample's peak, with their
  levels, pan and pitch changes.
- SiON interpolates between a sample's frames, where a DAC holds each value until the next: a little smoother
  (the consoles' output filters smooth it too).
- More than 256 different samples won't fit SiON's slots: written runs are joined until they do.

Sizes: Sonic's Sparkling Zone (32 KB VGZ) becomes 25 KB of MML, Gunstar Heroes' Legend of the Gunstars 55 KB.
Sounds that never repeat stay large: the digitized voice in Battletoads' title (several seconds of PCM) is 420 KB,
and Metal Slug's ADPCM songs (a 2.5 MB VGZ: 8 MB) store their ROM samples decoded.

`--exact-pcm` instead captures the samples exactly as the chips output them, frame by frame at 44100 Hz (VGM times
are 44100 Hz frames, so that is exact), with ADPCM in 16 bits: exact down to the drivers' timing jitter, but a
sample per hit, so many times larger (Sparkling Zone: 1.5 MB).

Not converted (reported as notes): the YM2608 rhythm sounds (they are in a ROM inside the chip, which VGM files
don't carry), ADPCM fed by the CPU instead of from memory, compressed data blocks, the YM2151 LFO, the FM channel 3
special mode, SSG-EG, the second chip of dual-chip files, and other chips (OPL/OPLL family, PCM chips such as
SegaPCM, RF5C68 and the OKI ADPCM chips, SCC, HuC6280...).

## GYM and S98

Two more register-log formats, read by rewriting them as VGM (`src/logs.rs`), so everything above applies:

- **GYM** (Mega Drive): YM2612 and PSG writes in 1/60 s frames, with or without the `GYMX` header (its loop start
  is honoured, and packed files are unpacked). Files without the header are recognized by being a well-formed
  command stream throughout. The DAC bytes a frame writes are spread over it the way libgme plays them (a frame
  with fewer than its neighbour starts or ends its sample with them).
- **S98** (PC-88/PC-98, MSX, X68000 logs), versions 0-3: YM2203, YM2608, YM2612, YM2151, YM2149/AY-3-8910 and
  SN76489 devices (one of each; the default device is a YM2608 at 7987200 Hz), the file's tick length and loop
  point. PSG clocks are halved, as libvgm plays them. YM2608 ADPCM data, which S98 writes through the chip's
  memory port, becomes the chip's RAM as a VGM data block would. Not converted: the OPL/OPLL family and
  compressed files. S98 has no frame-rate field, so the driver's frame is detected from the writes (as for a VGM
  without its rate).

Neither format points DAC bytes at a sample bank the way VGM's `0x8n` commands do: the converter makes one,
storing each stretch of DAC values (32 or more) heard before only once, so a drum played again shares its
sample. Aquatic Ruin Zone as a GYM becomes 49 KB of MML (55 KB from its VGZ), Streets of Rage 2's Go Straight as
S98 38 KB.

## NSF / NSFE

An NSF is the game's own sound driver, not a log, so `src/nsf.rs` plays it the way an NSF player does: it runs
the driver on a 6502 (`src/cpu.rs`: every documented instruction and the stable undocumented ones, with cycle
counts, checked against Klaus Dormann's functional test) — INIT once with the song number, then PLAY at the
file's rate — and logs every write to the sound chips at the CPU cycle it happens on. That log goes through the
same chip models and frame sampling as a VGM's (above), so everything said there about the NES APU, DPCM and
`$4011` samples applies. NSF (with bankswitching), NSFE and NSF2 files are read; NTSC unless the file is PAL-only
or an NSFE/NSF2 `regn` chunk prefers PAL or Dendy (X = 2, the Dendy's 1.773 MHz clock).

NSF2 (version 2, header byte `$7C`) adds to that:

- Metadata: NSFE chunks after the program (`time`, `fade`, `tlbl`, `RATE`, `regn`...). A chunk whose name starts
  with a capital is needed to play the file: an unknown one is an error (in an NSFE always, in an NSF2 when bit 7
  of `$7C` says the metadata has one).
- A non-returning INIT (bit 5): INIT is called with Y = `$80` (it returns), then again with Y = `$81` to run for
  good; PLAY interrupts it as the player's NMI handler would (IRQs masked, its registers restored after). If the
  second INIT returns, the player idles.
- IRQs (bit 4): the IRQ vector at `$FFFE` is RAM, and the CPU takes IRQs (while its I flag is clear) from the
  player's cycle timer (`$401B`/`$401C` reload, `$401D` start, stop and acknowledge), the APU frame counter (4-step
  mode, `$4017` bit 6 clear; acknowledged by reading `$4015`) and the DMC (a sample ending with `$4010` bit 7 set).
  IRQs also run between frames, while the player idles.
- No PLAY (bit 6): only INIT runs (with a non-returning INIT or IRQs); the driver's frame rate is then found from
  its writes, as for a VGM, rather than taken from the header.

For an NSF2 the machine's state that has to come round includes the CPU's registers (INIT may be running) and the
IRQ sources' timing, to 1/64 of a frame (a timer drifting a few cycles a loop against the frames still loops).

An NSF holds several songs (`--track N`; the notes name the track and, in an NSFE, its title) and no song
lengths, so the converter works out where each one ends:

- The machine's state (RAM, work RAM, banks) comes round to a state it was in at the start of an earlier frame:
  from there the driver can only do the same again, so the song loops exactly there. If every frame of that loop
  writes the same, the song has stopped (it ends there instead); and when the frames' writes repeat with a
  period that divides the state's, from no later (a counter the music doesn't hear makes the state come round
  only after many times through the music), the shorter loop is just as exact and is taken.
- Otherwise (drivers that count frames), the frames' writes repeat to the end, at least twice, with a loop of at
  least a second: the song loops there.
- The same writes every frame to the end, for a second or more: the song has stopped (or holds its last chord)
  and ends there. A loop that is silent is the same: the song ends with its last sound.
- Otherwise the NSFE's track time (plus fade) when it gives one, or the first 10 minutes (with a note).

DPCM samples are read from the program's memory as it is banked when each one starts. The DMC level starts at
0, as on the console, and a driver setting it now and then (`$4011`, a few writes: a DC step) isn't a sample: the
next DPCM sample starts from that level.

Expansion sound:

| Chip | Converted as |
| --- | --- |
| Konami VRC6 | pulses with duty in sixteenths (16-step `#WAV` tables, below; 8/16 as `%1@4`), the sawtooth as a 7-step `#WAV` table (its accumulator rate sets the level; rates past 42 wrap and change the shape, as on the chip; a rate change during a note, as a volume envelope makes, switches the table with `@n` inside the slur instead of restarting the note); `$9003` frequency scaling and halt |
| Nintendo MMC5 | its two pulses (no sweep units) |
| Sunsoft 5B | a YM2149 at half the CPU clock (as an AY-3-8910 above) |

The VRC6 is mixed level with the APU: a full-volume VRC6 pulse as loud as a full-volume APU pulse (as the
NESdev wiki has it; on Shovel Knight, each VRC6 voice comes out within a dB of libgme's level against the APU).
The 5B's level against the APU is not measured (the cartridge amplifies it). Not converted (reported as notes):
VRC7, Famicom Disk System and Namco 163 sound, MMC5 PCM, and the VRC6 pulses' digitized mode. The driver still
runs with them (FDS RAM, N163 sound RAM, the MMC5 multiplier and ExRAM are there), so the APU part converts.

Sizes: Shovel Knight's NSFE tracks (APU and VRC6) become 25-90 KB of MML; each takes a few seconds to convert.
A FamiTracker NSF with DPCM drums (a 15 KB E1M1 cover) becomes 26 KB.

## GBS

A GBS is a Game Boy game's sound driver, played like an NSF (`src/gbs.rs`): on the Game Boy's CPU (`src/sm83.rs`,
a Sharp SM83 with every instruction and its cycle count; Blargg's `cpu_instrs` tests pass (`BLARGG=dir cargo test --release --lib -- --ignored blargg`), the interrupt one aside:
the player calls the driver itself, so interrupts aren't modelled), INIT once with the song number, then PLAY at
the vertical blank's 59.73 Hz, or (TAC bit 2) at the timer's rate, `4096/262144/65536/16384 Hz / (256 - TMA)` as
TAC and TMA are set at that moment (drivers change TMA for their tempo; TAC bit 7 is double speed). Its writes to
the APU (`$FF10-$FF3F`) go through the same Game Boy chip model as a VGM's. As a GBS player does: the program is
placed at its load address, `$4000-$7FFF` is the bank last selected with a write to `$2000-$3FFF` (bank 0 there
is ignored), RST calls the load address plus the vector, RAM is at `$A000-$DFFF` (and its echo), and the APU starts
as the boot ROM leaves it (powered, `NR50` = `$77`, `NR51` = `$F3`, its wave RAM pattern). The song ends and loops
as an NSF's does (above); `--track N` picks the song, and `#TITLE` is the header's title and author.

Checked against libgme on Tetris, Super Mario Land (timer-driven, 64 Hz), Link's Awakening (bankswitched) and
Kirby's Dream Land: octave-band levels within about 1 dB above 100 Hz, and the notes on time (apart from the
lead-in, up to a row, that lines the notes up with the rows; see VGM). Songs take about a second each to convert
and become 4-18 KB of MML.

### `#PCMB`: samples in MML

`#PCMBn{data}rate,key,loop,flags;` defines sample `n` (0..255) for the `sion` crate's `%7@n` (pitched) and `%10`
(note number picks the sample) modules. `data` is URL-safe base64 (`A-Z a-z 0-9 - _`, no `/`, so it can't contain a
`//` comment) of signed little-endian frames: 8-bit, or 16-bit with flags 1. The sample plays at `rate` frames per
second at note `key` (MIDI numbering, 60 = `o5c`); with a `loop` frame (omitted or negative: none) it continues
from there after the last frame, back and forth with flags 2. On `%7`/`%10`, `@ph` is the frame a note starts
from, and may have a fraction (`@ph221.25`), be negative (that many frames of silence first) and give how many
frames to play before the sample stops (`@ph-16.5,100`); `k` may have a fraction too, on every module. A PCM note
starts, and changes pitch, at its exact time between output frames, so the sample's position never drifts from the
score. These are extensions: SiON itself loads PCM data from ActionScript, not from MML.

NES DPCM (flags 4, `#PCMBn{data}rate,key,loop,4,level;`): `data` is the DMC's bytes as they are, each bit (least
significant first) moving its 7-bit level up (1) or down (0) by 2 within 0..127, from `level`; the frames are the
level / 128, one per bit plus the starting level. `{=m}` instead of data takes the bytes of DPCM sample `m` (an
earlier `#PCMB`), so a sample started from several levels is stored once. A channel plays them as the console's
DMC does: its output is the level, which holds where a sample ends, is stopped by `@ph`'s length, or is keyed off
(those don't stop the channel); a note waiting for its start (a negative `@ph`) lets the sample before it play on
until then; and a first-order 60 Hz high-pass takes out the DC, as the console's output stage does (60 Hz matches
an NSF player's low end). Where a song loops back, the first sample after the loop point joins the level the end
left (the difference fading out over 20 ms), since it was decoded from the level of the first time through.

`#FBMODE{chip};` (another extension of the `sion` crate) feeds FM feedback from the sum of an operator's last
two outputs, each at half the depth, as the Yamaha chips do: the same depth for a slowly changing tone, but the
loop can't break into oscillation at the sample rate, so deep feedback stays tonal. SiON ignores the line and
uses its own feedback (the last output at full depth).

## SPC

An SPC is a snapshot of the SNES's sound unit: the SPC700's registers, its 64 KB of RAM (the game's sound
driver, the song and the samples) and the S-DSP's registers. `src/spc.rs` runs the driver from there on an SPC700
(`src/spc700.rs`: every instruction, with cycle counts) with the timers, I/O ports and IPL ROM, and a model of the
DSP follows what it plays, sample by sample at 32 kHz: key-ons and key-offs, each voice's BRR blocks and loop,
its pitch, and its envelope (ADSR and the GAIN modes, with the hardware's rate counter). It makes no sound itself:
each key-on becomes a note on its voice's track. The voices whose bit is set in the snapshot's KON register are
keyed on as it loads, as SPC players do (most snapshots are taken as a song starts, with its first notes there).

- Samples: each one played is taken from RAM as it was at its key-on and embedded as its BRR blocks (`#PCMB` with
  flags 8, about 3.5 times smaller than 16-bit frames, with its loop), which SiON decodes as the DSP does and plays
  with the DSP's Gaussian interpolation, so a sample pitched far down keeps the DSP's dull top end instead of a
  straight line's images (Donkey Kong Country's drums were up to 12 dB too bright at 4-8 kHz). A sample found with four
  different contents is a wave the driver rewrites as it plays (Chrono Trigger does): all its notes play the last
  version, so the voice keeps one tone and one track.
- Pitch: each sample's key is found from its loop's period (the octave kept so its usual note is within o3..o7)
  and its tuning from the pitches the driver plays it at, so the notes come out as whole notes. A driver's
  vibrato (its pitch writes going round the same values, at least twice, a set time each round) is SiON's pitch
  LFO: `@lfo` with a table in its exact shape from where it starts (the LFO stands still while its depth is 0,
  and runs at the envelope clock), `mp` its depth there and `k` its centre; even a 2-cent one, which the DSP's
  echo turns into a shimmer (it took Aquatic Ambiance's 50 ms loudness envelope from 0.90 to 0.99 correlation
  with libgme). Other pitch changes during a note (slides) are a pitch table (`np`, a value a 1/60 s frame from the
  key-on, to within a 64th of a semitone, written as `(from,to)count` runs where they lie on a line, its lowest
  value the note's `k`), where that is shorter than writing them as `k` slurs; under a vibrato's LFO the table
  moves its centre. (Aquatic Ambiance: 2,989 slurs to 489, 79 to 66 KB.) Sweeps wider than two octaves stay slurs.
- Envelopes become SiON's own (`@n,ar,dr,sr,rr,sl`): attack matched at half level (the DSP's is linear), the
  sustain level to 3 dB. A rise of 150 ms or more (a slow ADSR attack, a GAIN increase) is written as the DSP
  makes it instead: SiON's attack is a curve that starts late and ends suddenly (a 1.5 s pad 24 dB quieter than
  the DSP's at 0.2 s). The whole envelope, rise, decay and sustain, as long as the tone's notes last, is a volume
  table (`na`, one value a 1/60 s frame), and SiON's own envelope holds still; on a track with such tones, the
  others set the table back to full. The DSP's decay and sustain are not quite exponential (each step is -1, then -1/256 of
  the level, so they fall faster the lower they go, and below 1/8 of full scale linearly), while SiON's are
  straight lines in decibels: they are fitted to the DSP's own curve over how long the tone's notes usually last.
  The decay reaches SiON's sustain level when the DSP's reaches its own, and the sustain is the least-squares line
  on from there; with no decay stage (sustain level 7) SiON's decay is used as a second line, with the knee where
  the two come closest (a long note falling 34 dB stays within 0.5 dB of the DSP down to -15 dB, 1.5 dB down to
  -24 dB). SiON's rates are 19% apart: where some rate comes more than 3% closer for it, the tone also sets the
  envelope clock (`@clock`, percent). SiON's rates rise with the key (`kc >> 5`), so each note's are taken down
  by that at its own key. A key-off releases at the DSP's rate; a driver switching to a falling GAIN is a
  release at that rate. A note whose envelope the driver changes otherwise as it plays is written with a flat
  envelope and volume steps.
- Volume and pan from VOL × MVOL (in decibels, `#VMODE{%v1,1}`); echo as a delay on send 1 (`#EFFECT1{delay}`,
  EDL × 16 ms, the feedback and level from EFB and EVOL against MVOL, through the FIR's gain at 1 kHz), with the
  voices that have it on (EON) sending to it (`@v64,n`). The setting that holds longest is used.
- Timing: rows are the key-ons' grid (the longest row from 40 to 400 ms they line up with about as well as with
  any, by phase coherence, climbing to the top of that peak, then fitted over the song), a 1/16 note each;
  key-ons are put on the nearest row, half row, triplet or 1/64 note within a timer tick or two. A key-off a
  little before the voice's next key-on (drivers key off a set time, or a fraction of the note, before the next)
  keeps its gap, counted back from where that note is written and snapped to the voice's usual gap, so it comes
  out the same every time; the readable MML writes those as gate times (`q`, and `@q` for a fixed time before the
  next note) instead of a rest after every note. Other key-offs go on a whole or half row close by, or else the
  nearest tick.
- A voice that switches samples all the time (a drum kit) gets one track per sample, so each keeps its tone.
- Each sample with its envelope, echo send and envelope clock is an instrument (`#PCMWAVE` naming the `#PCMB`,
  `#PCMVOICE` with the envelope and the other settings after it), listed with the definitions at the top. A note
  changing tone chooses one with `%7@n` instead of writing all of it again. The audio is identical, and the
  files are about 1.3% smaller (4% of the text besides the sample data, on 269 test songs).

An SPC holds no song length either: the song loops where the driver's state (RAM, CPU and DSP registers, timer
settings) comes round to one it was in at an earlier timer tick, or else where its DSP writes repeat, or else
where its key-ons (voice, sample, pitch) repeat; a state loop that is several times round the music (its tempo
counter coming back into phase only then) is shortened to the key-ons' loop, from where it starts. The loop
point moves on to the next bar line of the music (16 rows from the row the most of the notes' lengths start on),
so the readable MML's lines are its bars; the loop's first notes, and any held across it, are played again after
the end, where the loop now runs on. A loop of silence is the song having ended. Otherwise it plays the ID666 tag's time, or
the first 10 minutes. The driver's writes and key-ons are checked for a loop every 20 s of the song as it runs,
and it stops early once the same loop is found twice in a row and has been playing three times round and for at
least 150 s (a section can repeat twice and go on, and an ambient piece can repeat a phrase for two minutes
before it changes). A driver waiting for its timer is skipped ahead to the tick, whole rounds of its wait loop
at a time.

Pitch modulation (PMON: a voice's pitch bent by the previous voice's output, before its VOL, P × (1 + out / 32768);
Final Fantasy VI's wind and vibratos, several Chrono Trigger tracks) is frequency modulation. SiON's way for one
track to modulate another is a pipe: a track's output goes to it (`@o`), and it moves another's operator phase
(`@i`; SiON's PCM channel ignores it, so the carrier can't stay `%7`). So:

- The modulated voice plays its sample's loop (up to 1024 frames: a single-cycle wave, as these are) as a
  wavetable (`#WAV`, `%4`), one table a loop, rebuilt from the loop's harmonics as the DSP plays them (SiON reads
  a wavetable without interpolating: a short loop as a short table would be a staircase), with `@i`.
- A copy of the modulating voice's notes, its own track just before the carrier's (SiON renders the tracks in
  order a block at a time, and reads a later track's pipe a block late), plays the integral of its loop into the
  pipe (`@o1`): moving the phase by an integral is moving the frequency by the wave. The depth, in cycles, is the
  carrier's table rate × the integral's peak / the modulator's rate: `@i n` gives 2^(n-3) cycles, the copy's
  volume (absolute, at full `x`, not heard) the rest. Its envelope is the modulator's; VOL doesn't count.
- A noise modulator can't be integrated: it moves the phase by one noise step's worth, which sounds alike (a
  rough, breathy carrier) but is approximate. A modulator that is itself modulated plays unmodulated into the pipe.
- A modulator slower than 20 Hz is a vibrato: the carrier stays PCM, with SiON's pitch LFO (`@lfo`, a `#TABLE` in
  the loop's shape, several cycles of it when that gets the rate within 0.5%; `mp` following the modulator's
  envelope). The table is the log of 1 + the level, so a deep one rises less than it falls, as on the DSP.
- Not converted (reported): a carrier or modulator sample without a loop of up to 1024 frames (a fast modulator),
  or more than 4 modulated voices (SiON has 4 pipes).

On a test song (a 62.5 Hz modulator under a 500 Hz carrier, deep enough that the sidebands are 10 dB over the
carrier), the first two sideband pairs come out within 0.2 dB of the DSP's.

Noise voices play SiON's noise (`%2`) at the DSP's noise rate. The echo FIR's shape (only its gain is kept) and
per-side echo settings are not converted.

Sizes: Super Mario World's tracks become 5-55 KB of MML (median 28 KB), Final Fantasy VI's, Chrono Trigger's and
Donkey Kong Country's 25-210 KB (medians 110-135 KB; the samples are most of it); each takes a few seconds to
convert.

## Minification (audio-identical, `--exact` modules)

- Shortest length text for every duration (`n`, dots, `^` ties, empty = default length), with default length (`l`) changes chosen by Viterbi.
- Gaps between notes absorbed by `q` gate ratios where shorter (`c8r8` → `q4c4`).
- Octave/spelling choice by Viterbi (`<`, `>`, `o`, `b+`/`c-` across octave boundaries) and the best `#SIGN` key signature.
- Repeats become `[...]n` and `[...|...]n` loops (nested). Repeated material across and within tracks becomes `#A=...;` macros (up to 26, can nest); the result is then looped again.
- Drum hits last until the next hit, so drum tracks need no rests.

## Tests

`cargo test --release` builds small NES, SN76489, Game Boy, YM2151, GYM, S98 and ProTracker files in code, NSF and NSFE
files (hand-assembled drivers: a bankswitched one that loops and plays DPCM, one that stops, one playing the
VRC6), NSF2 files (a non-returning INIT that PLAY interrupts, music on IRQs from the cycle timer and from the frame
counter, mandatory metadata chunks), GBS files (a scale from a switched bank, at the vertical blank and on the
timer), and VGM files
that play YM2612 DAC samples (from the data bank, written directly, and by DAC stream), NES DPCM and `$4011`
levels, and YM2610 ADPCM-A and -B, converts them (by default and with `--exact-pcm`, each checked against the
chip's output), checks the notes and the Mega Drive's mix (PSG against FM, one-sided pan, feedback level, fast
noise), and checks that the written MML renders identically to the plain conversion; an SPC with a hand-assembled
SPC700 driver (a scale on a looping BRR square) checks the notes, the loop and the sample's pitch, and a
table-driven one checks that the snapshot's KON keys its voices on, that key-offs a set time before the next note
are written as `@q` gates, that a long sustain follows the DSP's decay, and that pitch modulation comes out as
the DSP's (through a pipe: the sidebands; as a vibrato: the pitch swing). `src/tests.rs`
builds Amiga-period and linear (IT) modules with 8- and 16-bit, one-shot, looped and ping-pong samples and most
effects (slides, arpeggio, vibrato, tremolo, offsets, retrigger, cuts, envelopes, background voices, tempo
changes) and checks SiON's rendering against the reference mix.
