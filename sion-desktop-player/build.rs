//! Measures every song in `songs/` and picks a volume that brings it to a common loudness, so no song is
//! much louder or quieter than the rest. The volumes are built into the desktop player (`volumes.rs` in
//! OUT_DIR) and written, with the song list, to the web player's `songs/songs.json`.

use std::{fs, path::Path, thread};

/// Target loudness (RMS level of the whole song).
const LOUDNESS: f32 = 0.12;

/// Volume for one song: its loudness over one full pass, limited so its loudest sample can't clip.
fn volume(mml: &str) -> f32 {
    let (mut song, mut left) = (sion::Song::new(mml), sion::Song::length(mml) as usize);
    let (mut buf, mut power, mut peak, mut n) = (vec![0f32; 2 * 4096], 0f64, 0f32, 0);
    while left > 0 {
        let out = &mut buf[..2 * left.min(4096)];
        let playing = song.render(out);
        power += out.iter().map(|x| (x * x) as f64).sum::<f64>();
        peak = out.iter().fold(peak, |m, x| m.max(x.abs()));
        (n, left) = (n + out.len(), left - out.len() / 2);
        if !playing { break }
    }
    if peak == 0.0 { return 1.0 }
    (LOUDNESS / (power / n as f64).sqrt() as f32).min(1.0 / peak)
}

fn main() {
    let (songs, web) = (Path::new("songs"), Path::new("../website"));
    println!("cargo::rerun-if-changed=songs");
    println!("cargo::rerun-if-changed={}", web.join("songs").display());
    let list = |dir: &Path| {
        let mut names: Vec<String> = fs::read_dir(dir).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().into_string().ok()).filter(|n| n.ends_with(".mml") || n.ends_with(".sionmml")).collect();
        names.sort_unstable();
        names
    };
    let names = list(songs);
    let volumes: Vec<f32> = thread::scope(|s| {
        let jobs: Vec<_> = names.iter().map(|n| s.spawn(move || volume(&String::from_utf8_lossy(&fs::read(songs.join(n)).unwrap_or_default())))).collect();
        jobs.into_iter().map(|j| j.join().unwrap_or(1.0)).collect()
    });
    let lookup = |name: &str| names.iter().position(|n| n == name).map_or(1.0, |i| volumes[i]);

    let rs: String = names.iter().zip(&volumes).map(|(n, v)| format!("    ({n:?}, {v:.3}),\n")).collect();
    fs::write(Path::new(&std::env::var("OUT_DIR").unwrap()).join("volumes.rs"), format!("&[\n{rs}]")).unwrap();

    if web.join("songs").is_dir() {
        let json = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let rows: Vec<String> = list(&web.join("songs")).iter().map(|n| format!("{{\"file\": \"{}\", \"volume\": {:.3}}}", json(n), lookup(n))).collect();
        // Only when it changes: songs.json sits in songs/, which reruns this script whenever anything in it is touched.
        let (path, text) = (web.join("songs/songs.json"), format!("[\n{}\n]\n", rows.join(",\n")));
        if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) { fs::write(path, text).unwrap(); }
        song_list(&web.join("songs.html"), Some(&list(&web.join("songs"))), lookup);
        song_list(&web.join("index.html"), None, lookup); // the home page's examples
    }
}

/// Writes the song list into songs.html, between its `<!-- songs -->` and `<!-- /songs -->` markers, so the list
/// is in the page as it loads: one row per song, linking to its file, with its volume. With no `names`, it keeps the
/// songs the page already lists there (by their links, in their order), so a hand-picked list just gets fresh rows.
fn song_list(page: &Path, names: Option<&[String]>, volume: impl Fn(&str) -> f32) {
    let Ok(html) = fs::read_to_string(page) else { return };
    let (open, close) = ("<!-- songs -->", "<!-- /songs -->");
    let (Some(start), Some(end)) = (html.find(open), html.find(close)) else { return };
    let listed: Vec<String>;
    let names = match names {
        Some(names) => names,
        None => {
            // The reverse of `url` below.
            let unescape = |s: &str| s.replace("&quot;", "\"").replace("&gt;", ">").replace("&lt;", "<").replace("&amp;", "&")
                .replace("%3F", "?").replace("%23", "#").replace("%20", " ").replace("%25", "%");
            listed = html[start..end].split("href=\"songs/").skip(1).filter_map(|r| r.split_once('"')).map(|(h, _)| unescape(h)).collect();
            &listed
        }
    };
    let text = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    // Only what would break the URL is escaped (the file names stay readable in the page).
    let url = |s: &str| text(&s.replace('%', "%25").replace(' ', "%20").replace('#', "%23").replace('?', "%3F"));
    let rows: String = names.iter().enumerate().map(|(i, n)| {
        let name = n.trim_end_matches(".sionmml").trim_end_matches(".mml");
        let (artist, title) = name.split_once(" - ").unwrap_or(("", name));
        format!("  <li><a href=\"songs/{}\" data-volume=\"{:.3}\"><span class=\"n\">{}</span><div class=\"t\"><span>{}</span><span class=\"a\">{}</span></div></a></li>\n",
                url(n), volume(n), i + 1, text(title), text(artist))
    }).collect();
    let new = format!("{}{open}\n{rows}  {}", &html[..start], &html[end..]);
    // Only when it changes (as with songs.json).
    if new != html { fs::write(page, new).unwrap(); }
}
