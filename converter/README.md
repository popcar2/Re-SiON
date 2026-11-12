# sion-converter

Converts other music formats to minified SiON MML: Standard MIDI Files, tracker modules (MOD, S3M, XM, IT),
VGM/VGZ chip music logs and NSF/NSFE NES music. The format is recognized from the file's contents.

```
cargo run --release -- song.mid              # writes song.mml
cargo run --release -- song.xm -o out.mml --verify
cargo run --release -- song.vgz --wav out.wav
cargo run --release -- game.nsfe --track 3
```

Options: `-o FILE`, `--grid N` (snap timing to 1/N notes: smaller output, but no longer exact),
`--wavetables` (tracker modules: approximate samples instead of embedding them; see below),
`--track N` (NSF/NSFE: which song of the file, from 1; default: the file's first song),
`--exact-pcm` (VGM and NSF: capture chip samples exactly, frame by frame; much larger, see below),
`--gain G` (volume; default is automatic, so the busiest chord doesn't clip), `--plain` (write the unminified MML),
`--verify` (render the plain and minified MML with the `sion` crate and check that every sample is identical;
for a tracker module, also compare the result with the module mixed straight from its samples),
`--wav FILE` (also render the result to a WAV file). `sion-convert song.mml --wav out.wav` renders any MML file.
Anything a file uses that can't be converted is reported as a `note:` on stderr.

Every reader produces the same format-neutral score (`src/score.rs`), which `src/arrange.rs` turns into tracks
and `src/mml.rs` minifies, so all formats get the same timing, volume and minification handling. When the
automatic volume would still clip (FM voices and some waveforms come out louder than estimated), the result is
rendered and the master volume (`x`) is lowered until it doesn't.

## MIDI

- Timing stays exact. When the MIDI resolution doesn't divide SiON's 480 ticks per quarter, the converter counts in MIDI ticks and scales the tempo instead.
- Each channel is split into monophonic tracks, one per simultaneous voice. Handles the sustain pedal, velocity × CC7 × CC11 (`v1`..`v16`, plus a master `x`), pan (`@p`), and pitch bend with RPN range (`k`, with mid-note bends written as `&` slurs). Tempo changes become `t`.
- Instruments: GM program families map to the NES APU module (`%1@2` 25% pulse, `%1@4` 50% pulse, `%1@8` triangle).
- Drums (channel 10): kick and toms are sine waves with a fast downward pitch sweep (`%5@0` + `np` table); snares layer
  white noise over a short swept tone; hats, cymbals and ride use high-passed noise (`%2@3`), each with its own decay.
  Each drum type gets its own track. To change sounds, edit `instrument()` and `drum()` in `src/midi.rs`
  (render with `--wav` to listen).

## Tracker modules (MOD, S3M, XM, IT)

`src/tracker.rs` loads ProTracker MODs (and the 6/8/xx-channel variants), Scream Tracker 3, FastTracker 2 and
Impulse Tracker files (including IT-compressed samples); `src/play.rs` performs the song tick by tick, the way a
tracker does, and records what each channel sounds like on every tick:

- Rows, speed, tempo (and tempo slides), pattern jumps, breaks, loops and delays; the song ends where it would loop.
  A quarter note is four rows at the song's starting speed, so a tracker tick is one score tick and timing is exact.
- Effects: arpeggio, portamento (coarse, fine, extra fine, tone portamento), vibrato, tremolo, tremor, volume,
  channel and global volume slides, panning and pan slides, sample offset, retrigger, note cut and note delay,
  key off, the volume column, Amiga and linear frequency slides, and each format's effect memory.
- Instruments: XM/IT note→sample maps, volume and panning envelopes (sustain and loops), fadeout, and IT new-note
  actions (notes that ring on after the next one starts get tracks of their own).
- Pitch (`k`) and volume (`v`) changes during a note are written as `&` slurs.

Samples are embedded in the MML as they are (`#PCMB`, see below) and played with `%7`, so the conversion sounds
like the module itself:

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
- Volumes are in decibels (`#VMODE{%v1,1}`: `v0`..`v128` in 0.75 dB steps, the YM2612's own total-level step),
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

## NSF / NSFE

An NSF is the game's own sound driver, not a log, so `src/nsf.rs` plays it the way an NSF player does: it runs
the driver on a 6502 (`src/cpu.rs`: every documented instruction and the stable undocumented ones, with cycle
counts, checked against Klaus Dormann's functional test) — INIT once with the song number, then PLAY at the
file's rate — and logs every write to the sound chips at the CPU cycle it happens on. That log goes through the
same chip models and frame sampling as a VGM's (above), so everything said there about the NES APU, DPCM and
`$4011` samples applies. NSF (with bankswitching), NSFE and NSF2 (metadata after the program) files are read;
NTSC unless the file is PAL-only.

An NSF holds several songs (`--track N`; the notes name the track and, in an NSFE, its title) and no song
lengths, so the converter works out where each one ends:

- The machine's state (RAM, work RAM, banks) comes round to a state it was in at the start of an earlier frame:
  from there the driver can only do the same again, so the song loops exactly there.
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

## Minification (audio-identical)

- Shortest length text for every duration (`n`, dots, `^` ties, empty = default length), with default length (`l`) changes chosen by Viterbi.
- Gaps between notes absorbed by `q` gate ratios where shorter (`c8r8` → `q4c4`).
- Octave/spelling choice by Viterbi (`<`, `>`, `o`, `b+`/`c-` across octave boundaries) and the best `#SIGN` key signature.
- Repeats become `[...]n` and `[...|...]n` loops (nested). Repeated material across and within tracks becomes `#A=...;` macros (up to 26, can nest); the result is then looped again.
- Drum hits last until the next hit, so drum tracks need no rests.

## Tests

`cargo test --release` builds small NES, SN76489, Game Boy, YM2151 and ProTracker files in code, NSF and NSFE
files (hand-assembled drivers: a bankswitched one that loops and plays DPCM, one that stops, one playing the
VRC6), and VGM files
that play YM2612 DAC samples (from the data bank, written directly, and by DAC stream), NES DPCM and `$4011`
levels, and YM2610 ADPCM-A and -B, converts them (by default and with `--exact-pcm`, each checked against the
chip's output), checks the notes and the Mega Drive's mix (PSG against FM, one-sided pan, feedback level, fast
noise), and checks that the minified MML renders identically to the plain conversion. `src/tests.rs`
builds Amiga-period and linear (IT) modules with 8- and 16-bit, one-shot, looped and ping-pong samples and most
effects (slides, arpeggio, vibrato, tremolo, offsets, retrigger, cuts, envelopes, background voices, tempo
changes) and checks SiON's rendering against the reference mix.
