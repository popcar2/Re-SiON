/* SiON MML player: C interface to libsion.so / sion.dll. */
#ifndef SION_H
#define SION_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct SionSong SionSong;

/* Output sample rate in Hz (44100). */
uint32_t sion_rate(void);

/* Parse `len` bytes of MML text (UTF-8, need not be NUL-terminated). NULL if `mml` is NULL. */
SionSong *sion_song_new(const char *mml, size_t len);

/* Free a song. NULL is ignored. */
void sion_song_free(SionSong *song);

/* Fill `out` with `frames` stereo frames (2 * frames interleaved floats, L then R).
   Returns 1 while playing, 0 once finished (songs with a `$` loop never finish). */
int32_t sion_song_render(SionSong *song, float *out, size_t frames);

/* Move playback `frames` ahead without output, for seeking: the sequencer leaps from event to event and
   the channels' envelopes, sample positions and LFOs leap along with it, then the last moment is rendered
   so filters and effects settle. Fast (cost grows with the song's events, not its length in frames), and
   close to, not exactly, what continuous playback would give there.
   Returns 1 while playing, 0 once finished. To seek backwards, make a new song and skip from the start. */
int32_t sion_song_skip(SionSong *song, uint64_t frames);

/* Frames played (rendered or skipped) so far. */
uint64_t sion_song_position(const SionSong *song);

/* Length in frames of one pass through the MML: until every track has ended or jumped back to its `$`
   loop point once. Computed without rendering; capped at one hour. 0 if `mml` is NULL. */
uint64_t sion_length(const char *mml, size_t len);

/* For MML with a `$` loop: the frame playback carries on from after sion_length frames, i.e. where the
   loop point is (of the track finishing its first pass last, if tracks differ).
   -1 if it ends, or if `mml` is NULL. */
int64_t sion_loop_start(const char *mml, size_t len);

/* Set PCM sample `n` (0..255) for %7 / %10, replacing any #PCMB of that number.
   `data`: `frames` mono floats in -1..1, played at `rate` Hz at MIDI note `key` (60 = as recorded).
   `loop_start`: frame looping restarts from (negative = no loop); `bidi` nonzero = back-and-forth loop. */
void sion_song_set_sample(SionSong *song, uint32_t n, const float *data, size_t frames,
                          float rate, int32_t key, int64_t loop_start, int32_t bidi);

#ifdef __cplusplus
}
#endif

#endif
