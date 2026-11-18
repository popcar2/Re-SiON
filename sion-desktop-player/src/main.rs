//! Pick a SiON MML song from a folder and listen to it. Usage: sion-player [songs-dir]
//! Double-clicking it opens a terminal window (on Linux it relaunches itself in one).
#![forbid(unsafe_code)]

use std::io::{stdin, stdout, Read, Write};
use std::sync::{Arc, Mutex};
use tinyaudio::prelude::*;

fn say(parts: &[&str]) {
    let mut o = stdout().lock();
    for p in parts { let _ = o.write_all(p.as_bytes()); }
    let _ = o.flush();
}

/// Set on the copy started inside a terminal window, so it doesn't relaunch again.
const TERM_VAR: &str = "SION_PLAYER_TERM";

/// Started by double-clicking, with no terminal attached? Reopen inside a terminal window.
/// Only needed on Linux: Windows and macOS already open one for console programs.
#[cfg(all(unix, not(target_os = "macos")))]
fn open_terminal() {
    if std::fs::File::open("/dev/tty").is_ok() || std::env::var_os(TERM_VAR).is_some() { return }
    let Ok(exe) = std::env::current_exe() else { return };
    let user = std::env::var("TERMINAL").ok();
    let known: [(&str, &[&str]); 11] = [
        ("x-terminal-emulator", &["-e"]), ("konsole", &["-e"]), ("gnome-terminal", &["--"]), ("ptyxis", &["--"]), ("kgx", &["-e"]),
        ("xfce4-terminal", &["-x"]), ("kitty", &[]), ("alacritty", &["-e"]), ("foot", &[]), ("wezterm", &["start", "--"]), ("xterm", &["-e"]),
    ];
    for (term, pre) in user.as_deref().map(|t| (t, &["-e"][..])).into_iter().chain(known) {
        let run = std::process::Command::new(term).args(pre).arg(&exe).args(std::env::args_os().skip(1)).env(TERM_VAR, "1").spawn();
        if run.is_ok() { std::process::exit(0) }
    }
}

/// Report an error; in a window we opened ourselves, wait so it can be read before the window closes.
fn fail(parts: &[&str]) {
    say(parts);
    if std::env::var_os(TERM_VAR).is_some() {
        say(&["press Enter to close"]);
        let _ = stdin().read_line(&mut String::new());
    }
}

/// Volume that brings a song to a common loudness, so no song is much louder or quieter than the rest.
/// Measured for each song in `songs/` when the player is built (see build.rs); other songs play at full volume.
fn volume(file: &std::path::Path) -> f32 {
    const VOLUMES: &[(&str, f32)] = include!(concat!(env!("OUT_DIR"), "/volumes.rs"));
    let name = file.file_name().unwrap_or_default().to_string_lossy();
    VOLUMES.iter().find(|(n, _)| *n == name).map_or(1.0, |(_, v)| *v)
}

fn main() {
    #[cfg(all(unix, not(target_os = "macos")))]
    open_terminal();
    // Songs folder: the argument, else `songs` next to the program, else the one in the source tree.
    let dir = std::env::args_os().nth(1).map(std::path::PathBuf::from)
        .or_else(|| Some(std::env::current_exe().ok()?.parent()?.join("songs")).filter(|p| p.is_dir()))
        .unwrap_or_else(|| concat!(env!("CARGO_MANIFEST_DIR"), "/songs").into());
    let mut songs: Vec<_> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "mml" || x == "sionmml")).collect();
    songs.sort_unstable();
    if songs.is_empty() { return fail(&["no songs in ", &dir.to_string_lossy(), "\n"]) }
    let name = |i: usize| songs[i].file_stem().unwrap_or_default().to_string_lossy();

    let playing = Arc::new(Mutex::new(None::<(sion::Song, f32)>)); // song and its volume
    let shared = playing.clone();
    let params = OutputDeviceParameters { channels_count: 2, sample_rate: sion::RATE as usize, channel_sample_count: 2048 };
    let Ok(_device) = run_output_device(params, move |out| {
        out.fill(0.0);
        if let Ok(mut song) = shared.lock() {
            let Some((s, vol)) = song.as_mut() else { return };
            let playing = s.render(out);
            for x in out.iter_mut() { *x *= *vol }
            if !playing { *song = None }
        }
    }) else { return fail(&["cannot open audio output\n"]) };

    for i in 0..songs.len() { let n = (i + 1).to_string(); say(&[&"   "[n.len().min(3)..], &n, "  ", &name(i), "\n"]) }
    loop {
        say(&["\nsong number (s = stop, q = quit)> "]);
        let mut line = String::new();
        if stdin().read_line(&mut line).unwrap_or(0) == 0 { break }
        let set = |s| { if let Ok(mut p) = playing.lock() { *p = s } };
        match (line.trim(), line.trim().parse::<usize>()) {
            ("q", _) => break,
            ("s", _) => set(None),
            (_, Ok(n)) if (1..=songs.len()).contains(&n) => match std::fs::File::open(&songs[n - 1]).and_then(|f| { let mut v = vec![]; f.take((16 << 20) + 1).read_to_end(&mut v).map(|_| v) }) {
                // (Refused: anything much larger than a song, or with control characters no text file has.)
                Ok(mml) if mml.len() > 16 << 20 => say(&["too large to play (over 16 MB)\n"]),
                Ok(mml) if mml.iter().any(|&c| matches!(c, 0..=8 | 0x0e..=0x19 | 0x1c..=0x1f)) => say(&["not an MML file\n"]),
                Ok(mml) => {
                    set(Some((sion::Song::from_bytes(&mml), volume(&songs[n - 1]))));
                    say(&["playing: ", &name(n - 1), "\n"])
                }
                Err(_) => say(&["cannot read song\n"]),
            },
            ("", _) => {}
            _ => say(&["enter a number from 1 to ", &songs.len().to_string(), "\n"]),
        }
    }
}
