//! WebAssembly interface to the SiON MML player (`sion.wasm`, built by `build-wasm.sh` with the `wasm` feature).
//! See `sion.js` for a JS wrapper. It brings the module's panic handler and allocator, so it's only compiled with
//! the feature: as a dependency (the converter's wasm build, which has `std`) the crate is left without them.
//!
//! Same functions as the C interface (`ffi.rs`), plus `sion_alloc` / `sion_dealloc` so the host
//! can place MML text and sample data in the module's memory.

use alloc::boxed::Box;
use core::{alloc::{GlobalAlloc, Layout}, cell::UnsafeCell, ptr::null_mut, slice};
use crate::{Sample, Song, RATE};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { core::arch::wasm32::unreachable() }

// ---- Allocator: power-of-two size classes, freed blocks kept on a per-class free list. ----

const PAGE: usize = 65536;
const CLASSES: usize = 32;

struct Heap(UnsafeCell<([*mut u8; CLASSES], usize, usize)>); // free lists, bump pointer, bump end
unsafe impl Sync for Heap {} // wasm32-unknown-unknown is single-threaded

#[global_allocator]
static HEAP: Heap = Heap(UnsafeCell::new(([null_mut(); CLASSES], 0, 0)));

/// Size class for a layout: blocks are `1 << class` bytes (at least 8), aligned to min(size, 16).
fn class(l: Layout) -> usize { l.size().max(l.align()).max(8).next_power_of_two().trailing_zeros() as usize }

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let (free, next, end) = &mut *self.0.get();
        let c = class(l);
        if c >= CLASSES { return null_mut() }
        let head = free[c];
        if !head.is_null() {
            free[c] = *head.cast::<*mut u8>();
            return head;
        }
        let size = 1usize << c;
        let a = size.min(16) - 1;
        if ((*next + a) & !a) + size > *end {
            let pages = (size + 16).div_ceil(PAGE);
            let old = core::arch::wasm32::memory_grow(0, pages);
            if old == usize::MAX { return null_mut() }
            if old * PAGE != *end { *next = old * PAGE } // first growth: start at the fresh pages
            *end = (old + pages) * PAGE;
        }
        let start = (*next + a) & !a;
        *next = start + size;
        start as *mut u8
    }

    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let free = &mut (*self.0.get()).0;
        let c = class(l);
        *p.cast::<*mut u8>() = free[c];
        free[c] = p;
    }

    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let nl = Layout::from_size_align_unchecked(new, l.align());
        if class(nl) == class(l) { return p }
        let q = self.alloc(nl);
        if !q.is_null() {
            core::ptr::copy_nonoverlapping(p, q, l.size().min(new));
            self.dealloc(p, l);
        }
        q
    }
}

// ---- Exports ----

/// Allocate `len` bytes (8-byte aligned) for passing MML text or sample data in. Free with `sion_dealloc`.
#[no_mangle]
pub extern "C" fn sion_alloc(len: usize) -> *mut u8 {
    unsafe { HEAP.alloc(Layout::from_size_align_unchecked(len, 8)) }
}

/// Free memory from `sion_alloc`; `len` must be the same as passed to it.
///
/// # Safety
/// `ptr` must come from `sion_alloc(len)`.
#[no_mangle]
pub unsafe extern "C" fn sion_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() { HEAP.dealloc(ptr, Layout::from_size_align_unchecked(len, 8)) }
}

/// Output sample rate in Hz (44100).
#[no_mangle]
pub extern "C" fn sion_rate() -> u32 { RATE as u32 }

/// Parse `len` bytes of UTF-8 MML text. Free the result with `sion_song_free`. Returns 0 if `mml` is 0.
///
/// # Safety
/// `mml` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sion_song_new(mml: *const u8, len: usize) -> *mut Song {
    if mml.is_null() { return null_mut() }
    Box::into_raw(Box::new(Song::from_bytes(slice::from_raw_parts(mml, len))))
}

/// Free a song from `sion_song_new`. 0 is ignored.
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
pub unsafe extern "C" fn sion_song_skip(song: *mut Song, frames: u32) -> i32 {
    song.as_mut().map_or(0, |s| s.skip(frames as u64) as i32)
}

/// Frames played (rendered or skipped) so far. (A float, so JS gets a plain number rather than a BigInt.)
///
/// # Safety
/// `song` must be a live song.
#[no_mangle]
pub unsafe extern "C" fn sion_song_position(song: *const Song) -> f64 {
    song.as_ref().map_or(0.0, |s| s.position() as f64)
}

/// Write, for up to `max` tracks, the byte offset in the MML text of the note each is playing now (-1: none)
/// to `out`, for following playback in the text. Returns the number of tracks (which may be more than `max`).
///
/// # Safety
/// `song` must be a live song and `out` must point to `max` writable ints.
#[no_mangle]
pub unsafe extern "C" fn sion_song_notes(song: *const Song, out: *mut i32, max: usize) -> usize {
    let (Some(song), false) = (song.as_ref(), out.is_null()) else { return 0 };
    let mut notes = song.notes();
    slice::from_raw_parts_mut(out, max).iter_mut().zip(&mut notes).map(|(o, src)| *o = src).count() + notes.count()
}

/// Length in frames of one pass through `len` bytes of MML (until every track ends or loops once), computed
/// without rendering. 0 if `mml` is 0.
///
/// # Safety
/// `mml` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sion_length(mml: *const u8, len: usize) -> f64 {
    if mml.is_null() { return 0.0 }
    Song::length_bytes(slice::from_raw_parts(mml, len)) as f64
}

/// For MML with a `$` loop (`len` bytes): the frame playback carries on from after `sion_length` frames,
/// i.e. where the loop point is (of the track that finishes its first pass last, if tracks differ).
/// -1 if it ends.
///
/// # Safety
/// `mml` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sion_loop_start(mml: *const u8, len: usize) -> f64 {
    if mml.is_null() { return -1.0 }
    Song::loop_start_bytes(slice::from_raw_parts(mml, len)).map_or(-1.0, |f| f as f64)
}

/// Set PCM sample `n` (0..255) for `%7` / `%10`, replacing any `#PCMB` of that number.
/// `data` is `frames` mono floats in -1..1, played at `rate` Hz at MIDI note `key` (60 = as recorded).
/// `loop_start` is the frame looping restarts from (negative: no loop); `bidi` nonzero loops back and forth.
///
/// # Safety
/// `song` must be a live song and `data` must point to `frames` readable floats.
#[no_mangle]
pub unsafe extern "C" fn sion_song_set_sample(song: *mut Song, n: u32, data: *const f32, frames: usize, rate: f32, key: i32, loop_start: i32, bidi: i32) {
    let (Some(song), false) = (song.as_mut(), data.is_null()) else { return };
    let data = slice::from_raw_parts(data, frames).to_vec();
    song.set_sample(n as usize, Sample { data, rate, key, lp: usize::try_from(loop_start).ok(), bidi: bidi != 0, ..Default::default() });
}
