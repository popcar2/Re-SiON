//! WebAssembly interface (`convert.wasm`, built by `build-wasm.sh`; only compiled for wasm32). See `convert.js` for a JS wrapper.
//!
//! The host copies the input file into memory from `convert_alloc`, calls `convert_run`, then reads the
//! results (`convert_output`): the MML, and a small JSON object with the stats, warnings or the error.
//! A panic aborts (a wasm trap): the host should then drop the instance and make a new one.

use std::cell::RefCell;
use std::fmt::Write;

thread_local! {
    /// The last conversion's outputs: [MML, info JSON].
    static OUT: RefCell<[String; 2]> = RefCell::default();
}

/// Allocate `len` bytes for the input file. Free with `convert_dealloc`.
#[no_mangle]
pub extern "C" fn convert_alloc(len: usize) -> *mut u8 {
    let mut v = std::mem::ManuallyDrop::new(Vec::<u8>::with_capacity(len.max(1)));
    v.as_mut_ptr()
}

/// Free memory from `convert_alloc`; `len` must be the same as passed to it.
///
/// # Safety
/// `ptr` must come from `convert_alloc(len)`.
#[no_mangle]
pub unsafe extern "C" fn convert_dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() { drop(Vec::from_raw_parts(ptr, 0, len.max(1))) }
}

/// Convert the `len` bytes at `data` (any supported format, recognised from its contents). Options as on the
/// command line: `grid` (0: exact timing), `gain` (0 or less: automatic), `flags` (1 `--wavetables`, 2 `--exact`,
/// 4 `--exact-pcm`), `track` (0: the file's first song). Returns 1 on success, 0 on failure (the error is in
/// the info).
///
/// # Safety
/// `data` must point to `len` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn convert_run(data: *const u8, len: usize, grid: u32, gain: f32, flags: u32, track: u32) -> i32 {
    let data = if data.is_null() { &[][..] } else { std::slice::from_raw_parts(data, len) };
    let opt = crate::Options {
        grid: (grid > 0).then_some(grid),
        gain: (gain > 0.0).then_some(gain),
        wavetables: flags & 1 != 0,
        exact: flags & 2 != 0,
        exact_pcm: flags & 4 != 0,
        track: (track > 0).then_some(track as usize),
    };
    let (mml, info, ok) = match crate::convert(data, &opt) {
        Ok(c) => {
            let mut info = format!(
                "{{\"ok\":true,\"format\":{},\"tracks\":{},\"notes\":{},\"bytes\":{},\"plainBytes\":{},\"secs\":{},\"loopSecs\":{},\"warnings\":[",
                json_str(&format!("{:?}", crate::detect(data).unwrap())), c.tracks, c.notes, c.mml.len(), c.plain.len(),
                c.secs, c.loop_secs.map_or("null".into(), |l| l.to_string()));
            for (i, w) in c.warnings.iter().enumerate() { if i > 0 { info.push(',') } info += &json_str(w) }
            info += "]}";
            (c.mml, info, true)
        }
        Err(e) => (String::new(), format!("{{\"ok\":false,\"error\":{}}}", json_str(&e)), false),
    };
    OUT.with(|o| *o.borrow_mut() = [mml, info]);
    ok as i32
}

/// Address of output `i` of the last conversion (0: the MML, 1: the info JSON), UTF-8.
#[no_mangle]
pub extern "C" fn convert_output(i: usize) -> *const u8 { OUT.with(|o| o.borrow().get(i).map_or(std::ptr::null(), |s| s.as_ptr())) }

/// Length in bytes of output `i`.
#[no_mangle]
pub extern "C" fn convert_output_len(i: usize) -> usize { OUT.with(|o| o.borrow().get(i).map_or(0, |s| s.len())) }

/// Free the last conversion's outputs.
#[no_mangle]
pub extern "C" fn convert_clear() { OUT.with(|o| *o.borrow_mut() = Default::default()) }

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => { let _ = write!(out, "\\u{:04x}", c as u32); }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
