//! C interface to the SiON MML player. See `sion.h` for the API.

use core::{ffi::c_char, slice};
use sion_core::{Sample, Song, RATE};

/// Output sample rate in Hz (44100).
#[no_mangle]
pub extern "C" fn sion_rate() -> u32 { RATE as u32 }

/// Parse `len` bytes of MML text (UTF-8; need not be NUL-terminated). Free the result with `sion_song_free`.
/// Returns NULL if `mml` is NULL.
///
/// # Safety
/// `mml` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sion_song_new(mml: *const c_char, len: usize) -> *mut Song {
    if mml.is_null() { return core::ptr::null_mut() }
    let text = String::from_utf8_lossy(slice::from_raw_parts(mml.cast::<u8>(), len));
    Box::into_raw(Box::new(Song::new(&text)))
}

/// Free a song from `sion_song_new`. NULL is ignored.
///
/// # Safety
/// `song` must come from `sion_song_new` and not be used after this call.
#[no_mangle]
pub unsafe extern "C" fn sion_song_free(song: *mut Song) {
    if !song.is_null() { drop(Box::from_raw(song)) }
}

/// Fill `out` with `frames` stereo frames (`2 * frames` interleaved floats, left then right).
/// Returns 1 while the song plays, 0 once it has finished (songs with a `$` loop never finish).
///
/// # Safety
/// `song` must be a live song and `out` must point to `2 * frames` writable floats.
#[no_mangle]
pub unsafe extern "C" fn sion_song_render(song: *mut Song, out: *mut f32, frames: usize) -> i32 {
    let (Some(song), false) = (song.as_mut(), out.is_null()) else { return 0 };
    song.render(slice::from_raw_parts_mut(out, frames * 2)) as i32
}

/// Move playback `frames` ahead without output, for seeking (see `Song::skip`). Returns 1 while the song
/// plays, 0 once it has finished. To seek backwards, make a new song and skip from the start.
///
/// # Safety
/// `song` must be a live song.
#[no_mangle]
pub unsafe extern "C" fn sion_song_skip(song: *mut Song, frames: u64) -> i32 {
    song.as_mut().map_or(0, |s| s.skip(frames) as i32)
}

/// Frames played (rendered or skipped) so far.
///
/// # Safety
/// `song` must be a live song.
#[no_mangle]
pub unsafe extern "C" fn sion_song_position(song: *const Song) -> u64 {
    song.as_ref().map_or(0, Song::position)
}

/// Length in frames of one pass through `len` bytes of MML (until every track ends or loops once), computed
/// without rendering. 0 if `mml` is NULL.
///
/// # Safety
/// `mml` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sion_length(mml: *const c_char, len: usize) -> u64 {
    if mml.is_null() { return 0 }
    Song::length(&String::from_utf8_lossy(slice::from_raw_parts(mml.cast::<u8>(), len)))
}

/// For MML with a `$` loop (`len` bytes): the frame playback carries on from after `sion_length` frames,
/// i.e. where the loop point is (of the track that finishes its first pass last, if tracks differ).
/// -1 if it ends, or if `mml` is NULL.
///
/// # Safety
/// `mml` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sion_loop_start(mml: *const c_char, len: usize) -> i64 {
    if mml.is_null() { return -1 }
    Song::loop_start(&String::from_utf8_lossy(slice::from_raw_parts(mml.cast::<u8>(), len))).map_or(-1, |f| f as i64)
}

/// Set PCM sample `n` (0..255) for `%7` / `%10`, replacing any `#PCMB` of that number.
/// `data` is `frames` mono floats in -1..1, played at `rate` Hz at MIDI note `key` (60 = as recorded).
/// `loop_start` is the frame looping restarts from (negative: no loop); `bidi` nonzero loops back and forth.
///
/// # Safety
/// `song` must be a live song and `data` must point to `frames` readable floats.
#[no_mangle]
pub unsafe extern "C" fn sion_song_set_sample(song: *mut Song, n: u32, data: *const f32, frames: usize, rate: f32, key: i32, loop_start: i64, bidi: i32) {
    let (Some(song), false) = (song.as_mut(), data.is_null()) else { return };
    let data = slice::from_raw_parts(data, frames).to_vec();
    song.set_sample(n as usize, Sample { data, rate, key, lp: usize::try_from(loop_start).ok(), bidi: bidi != 0 });
}
