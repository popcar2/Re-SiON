//! Pick a SiON MML song from a folder and listen to it. Usage: sion-player [songs-dir]
//! Double-clicking it opens a terminal window (on Linux it relaunches itself in one).
#![forbid(unsafe_code)]

use std::io::{stdin, stdout, Write};
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

    let playing = Arc::new(Mutex::new(None::<sion::Song>));
    let shared = playing.clone();
    let params = OutputDeviceParameters { channels_count: 2, sample_rate: sion::RATE as usize, channel_sample_count: 2048 };
    let Ok(_device) = run_output_device(params, move |out| {
        out.fill(0.0);
        if let Ok(mut song) = shared.lock() {
            if song.as_mut().is_some_and(|s| !s.render(out)) { *song = None }
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
            (_, Ok(n)) if (1..=songs.len()).contains(&n) => match std::fs::read(&songs[n - 1]) {
                Ok(mml) => { set(Some(sion::Song::new(&String::from_utf8_lossy(&mml)))); say(&["playing: ", &name(n - 1), "\n"]) }
                Err(_) => say(&["cannot read song\n"]),
            },
            ("", _) => {}
            _ => say(&["enter a number from 1 to ", &songs.len().to_string(), "\n"]),
        }
    }
}
