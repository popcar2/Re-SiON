// The player bar shared by the song list and the playground: audio (render.js → worklet.js), the seek bar and
// the transport buttons. It adds the bar's markup to the page itself.
//
//   const player = createPlayer({ prev, next, idle, ended, notes });
//   player.play(mmlOrPromise, { title, artist, volume });
//
// Hooks (all optional): prev() / next() — show those buttons, called on click; canPrev() / canNext() — whether
// they're enabled; idle() — play pressed with nothing loaded; idleText — shown under "Nothing playing";
// show(song) — after the bar updates (song: the meta passed to play, null when stopped); ended() — the song played out (default: stop);
// notes(offsets) — follow playback: called with the UTF-8 byte offsets in the MML of the notes playing, as they
// are heard ([] once nothing is); heatmap: true — show the song's spectrum rising from the bar (heatmap.js), with a
// checkbox in the bar to turn it off (remembered per browser). The bar's volume slider and mute button are remembered too.

import { heatmap } from "./heatmap.js";

const RATE = 44100;
// MML is refused (with the reason shown in the bar) when it's larger than this (real songs are well under 1 MB)...
const MAX_MML = 16 << 20;
// ...or holds control characters no text file has: it's some other kind of file, which would play as noise.
const BINARY = /[\0-\x08\x0e-\x19\x1c-\x1f]/;
const PLAY = "M4 2.5v11L13.5 8z", PAUSE = "M3.5 2.5h3v11h-3zM9.5 2.5h3v11h-3z";
// The volume button's speaker, and what's drawn beside it: sound waves (one when quiet, two when loud), or a cross when muted.
const SPEAKER = "M1.5 5.5h3L8 2.5v11l-3.5-3h-3z";
const WAVES = ["M10.5 6a3 3 0 010 4", "M10.5 6a3 3 0 010 4M12.5 4a6 6 0 010 8"], MUTED = "M10.5 5.5l4 5M14.5 5.5l-4 5";

const BAR = `<footer>
  <div class="bar">
  <label class="viz" id="viz" title="Show the visualizer" hidden><input type="checkbox" id="vizon" checked>Visualizer</label>
  <div class="seek">
    <time id="pos">0:00</time>
    <input type="range" id="seek" min="0" max="1" step="any" value="0" disabled aria-label="Seek">
    <time id="dur">0:00</time>
  </div>
  <div class="row">
    <button id="prev" title="Previous" disabled hidden><svg viewBox="0 0 16 16"><path d="M3 3h2v10H3zM14 3v10L6 8z"/></svg></button>
    <button id="play" class="main" title="Play / pause (space)"><svg viewBox="0 0 16 16"><path id="icon" d="${PLAY}"/></svg></button>
    <button id="next" title="Next" disabled hidden><svg viewBox="0 0 16 16"><path d="M11 3h2v10h-2zM2 3v10l8-5z"/></svg></button>
    <div class="now"><div class="t" id="title">Nothing playing</div><div class="a" id="artist"></div></div>
    <div class="vol">
      <button id="mute" title="Mute (M)"><svg viewBox="0 0 16 16"><path d="${SPEAKER}"/><path id="waves" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg></button>
      <input type="range" id="vol" min="0" max="1" step="0.01" value="1" aria-label="Volume">
    </div>
    <button id="save" title="Download MML" disabled><svg viewBox="0 0 16 16"><path d="M7 2h2v6.5l2.3-2.3 1.4 1.4L8 12.3 3.3 7.6l1.4-1.4L7 8.5zM3 13h10v1.5H3z"/></svg></button>
  </div>
  </div>
</footer>`;

export function createPlayer(hooks = {}) {
  document.body.insertAdjacentHTML("beforeend", BAR);
  const footer = document.body.lastElementChild;
  const $ = (id) => document.getElementById(id);
  $("prev").hidden = !hooks.prev;
  $("next").hidden = !hooks.next;
  const idleText = $("artist").textContent = hooks.idleText ?? "";
  // The visualizer checkbox: whether it's on is remembered (when storage is available), and applies from the start.
  $("viz").hidden = !hooks.heatmap;
  // Clicked (not tabbed to): let go of focus, so space goes back to play / pause.
  const letGo = (el) => { if (!el.matches(":focus-visible")) el.blur() };
  try { if (localStorage.getItem("sion-visualizer") === "off") $("vizon").checked = false } catch {}
  $("vizon").onchange = () => {
    const on = $("vizon").checked;
    try { localStorage.setItem("sion-visualizer", on ? "on" : "off") } catch {}
    heat?.enable(on);
    letGo($("vizon"));
  };

  // Volume: the slider's position (0 to 1) and whether it's muted, remembered when storage is available.
  let volume = 1, muted = false;
  try {
    const v = parseFloat(localStorage.getItem("sion-volume"));
    if (v >= 0 && v <= 1) volume = v;
    muted = localStorage.getItem("sion-muted") === "on";
  } catch {}

  let ctx, node, gain, heat, ready, song = null, id = 0; // song: what's loaded ({title, artist}); id: bumped per song
  let text = null; // the MML of the song loaded, once it has arrived (for the download button)
  let refused = null; // {title, why}: the song last given to play() that can't be played, shown until the next
  let early = false; // the song played out before its length arrived: ended() waits for it (0 or 1 frame = plays no notes)
  // Songs are rendered ahead and measured in a worker (render.js) and handed to the audio worklet, which only copies samples.
  const renderer = new Worker(new URL("render.js", import.meta.url), { type: "module" });
  let pos = 0, dur = 0, loop = -1, dragging = false; // in frames; dur = 0 until measured, loop = -1 unless it loops
  renderer.onmessage = ({ data }) => {
    if (data.id !== id) return;
    // (A song with nothing in it still measures a frame: the sequencer always takes a step.)
    if (data.frames <= 1) return refuse("Plays no notes");
    if ("frames" in data) dur = data.frames;
    if (early && dur) { early = false; end() }
    if ("loop" in data) loop = data.loop;
    showTime();
  };

  // Which notes are playing reaches the page with the worklet's position, as it hands samples to the output;
  // they are heard one output latency later.
  const notes = (offsets, at = id) => {
    const delay = ctx ? (ctx.outputLatency || ctx.baseLatency || 0) * 1000 : 0;
    setTimeout(() => at === id && hooks.notes(offsets), delay);
  };

  function audio() {
    return ready ??= (async () => {
      // "playback": nothing here is live, so let the browser use its larger, more glitch-proof output buffer.
      ctx = new AudioContext({ sampleRate: RATE, latencyHint: "playback" });
      await ctx.audioWorklet.addModule(new URL("worklet.js", import.meta.url));
      node = new AudioWorkletNode(ctx, "sion", { numberOfInputs: 0, outputChannelCount: [2] });
      // The volume applies after the visualizer's tap on node, so the visualizer looks the same at any volume.
      gain = new GainNode(ctx, { gain: level() });
      node.connect(gain).connect(ctx.destination);
      if (hooks.heatmap) heat = heatmap(ctx, node, footer, playing, $("vizon").checked);
      const { port1, port2 } = new MessageChannel();
      node.port.postMessage({ port: port1 }, [port1]);
      renderer.postMessage({ port: port2 }, [port2]);
      node.port.onmessage = ({ data }) => {
        if ("pos" in data) { pos = data.pos; showTime(); if (hooks.notes && data.notes) notes(data.notes) }
        // (Nothing loaded: it was stopped already, perhaps refused, whose reason stays shown.)
        if (data.ended && song) { if (!dur) early = true; else end() }
      };
    })();
  }

  /** Play MML text (or a promise of it). meta: {title, artist, volume}. */
  async function play(mml, meta = {}) {
    const me = ++id;
    song = meta;
    text = null;
    pos = dur = 0;
    loop = -1;
    hooks.notes?.([]);
    show();
    refused = null;
    early = false;
    [mml] = await Promise.all([mml, audio()]);
    if (me !== id) return; // another song was picked while this one loaded
    const why = mml.length > MAX_MML ? `Too large to play (over ${MAX_MML >> 20} MB)` : BINARY.test(mml) ? "Not an MML file" : null;
    if (why) return refuse(why);
    text = mml;
    renderer.postMessage({ mml, id: me, volume: meta.volume ?? 1, notes: !!hooks.notes });
    await ctx.resume();
    show();
  }

  const end = () => hooks.ended ? hooks.ended() : stop();

  /** Stop, showing why the song given to play() can't be played. */
  function refuse(why) {
    const title = song?.title;
    stop();
    refused = { title, why };
    show();
  }

  function stop() {
    renderer.postMessage({ mml: null });
    refused = null;
    early = false;
    id++;
    song = text = null;
    pos = dur = 0;
    loop = -1;
    hooks.notes?.([]);
    show();
  }

  function seek(frame) {
    if (!song || !node) return;
    pos = Math.max(0, dur ? Math.min(frame, dur) : frame);
    renderer.postMessage({ seek: pos });
    showTime();
  }

  async function toggle() {
    if (!song) return hooks.idle?.();
    if (!ctx) return; // still starting up
    ctx.state === "running" ? await ctx.suspend() : await ctx.resume();
    show();
  }

  const playing = () => !!song && ctx?.state === "running";

  function show() {
    document.body.classList.toggle("playing", playing());
    if (playing()) heat?.wake();
    $("title").textContent = song ? song.title : refused?.title || "Nothing playing";
    $("artist").textContent = song ? song.artist || " " : refused ? refused.why : idleText;
    $("icon").setAttribute("d", playing() ? PAUSE : PLAY);
    $("prev").disabled = !song || !hooks.canPrev?.();
    $("next").disabled = !song || !hooks.canNext?.();
    $("save").disabled = text == null;
    showTime();
    hooks.show?.(song);
  }

  const clock = (frames) => { const t = Math.floor(frames / RATE); return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}` };

  // Songs with a `$` loop play on forever: each time playback reaches the end, the bar and clock jump back to the
  // loop point. (A song that ends instead can ring a moment past its length; the bar just stays full until it stops.)
  const shown = () => {
    if (!dur || pos < dur) return pos;
    return loop >= 0 && loop < dur ? loop + (pos - dur) % (dur - loop) : dur;
  };

  function showTime() {
    const bar = $("seek");
    bar.disabled = !song || !dur;
    if (!dragging) {
      bar.max = dur || 1;
      bar.value = dur ? shown() : 0; // until the length is known, max is a placeholder 1 — keep the bar empty
      $("pos").textContent = clock(shown());
    }
    bar.style.setProperty("--p", `${dur ? Math.min(1, bar.value / dur) * 100 : 0}%`);
    $("dur").textContent = dur ? clock(dur) : !song ? "0:00" : "–:––";
  }

  // The slider is curved (squared), so its travel matches how loud it sounds rather than the signal's amplitude.
  const level = () => muted ? 0 : volume * volume;
  function showVolume() {
    const shown = muted ? 0 : volume;
    $("vol").value = shown;
    $("vol").style.setProperty("--f", shown);
    $("waves").setAttribute("d", shown === 0 ? MUTED : WAVES[shown < 0.5 ? 0 : 1]);
    $("mute").title = muted ? "Unmute (M)" : "Mute (M)";
    $("mute").setAttribute("aria-pressed", muted);
    // A quick ramp, so a change (dragging especially) doesn't click.
    gain?.gain.setTargetAtTime(level(), ctx.currentTime, 0.015);
  }
  // Remembered once a change is made (not on every step of a drag).
  function saveVolume() {
    try {
      localStorage.setItem("sion-volume", volume);
      localStorage.setItem("sion-muted", muted ? "on" : "off");
    } catch {}
  }
  function setMuted(on) {
    muted = on;
    if (!muted && volume === 0) volume = 0.5; // unmuting with the slider at 0 would still be silent
    showVolume();
    saveVolume();
  }
  $("vol").oninput = () => { volume = +$("vol").value; muted = false; showVolume() };
  $("vol").onchange = () => { saveVolume(); letGo($("vol")) };
  $("mute").onclick = () => { setMuted(!muted); letGo($("mute")) };
  showVolume();

  $("seek").oninput = () => { dragging = true; $("pos").textContent = clock(+$("seek").value); showTime() };
  $("seek").onchange = () => { dragging = false; seek(+$("seek").value) };

  $("play").onclick = toggle;
  $("prev").onclick = () => hooks.prev?.();
  $("next").onclick = () => hooks.next?.();
  // Download the MML playing, under its file name (meta.file) or else its title.
  $("save").onclick = () => {
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([text], { type: "text/plain;charset=utf-8" }));
    a.download = song.file ?? `${(song.title || "song").replace(/[\\/:*?"<>|]/g, "_")}.mml`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(a.href), 1000);
  };
  addEventListener("keydown", (e) => {
    if (e.target.matches?.("input[type=range]") && e.key.startsWith("Arrow")) return; // the sliders handle their own arrows
    if (e.target.closest?.("textarea, input:not([type=range]), select, [contenteditable]")) return; // typing
    if (e.key === " ") { e.preventDefault(); toggle(); }
    else if (e.key === "Escape") stop();
    else if (e.key.toLowerCase() === "m" && !e.ctrlKey && !e.metaKey && !e.altKey) setMuted(!muted);
    else if (e.key === "ArrowLeft") seek(shown() - 5 * RATE);
    else if (e.key === "ArrowRight") seek(shown() + 5 * RATE);
  });
  show();

  return { play, stop, toggle, seek, show, get song() { return song }, get playing() { return playing() } };
}
