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
// are heard ([] once nothing is).

const RATE = 44100;
const PLAY = "M4 2.5v11L13.5 8z", PAUSE = "M3.5 2.5h3v11h-3zM9.5 2.5h3v11h-3z";

const BAR = `<footer>
  <div class="bar">
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
    <button id="save" title="Download MML" disabled><svg viewBox="0 0 16 16"><path d="M7 2h2v6.5l2.3-2.3 1.4 1.4L8 12.3 3.3 7.6l1.4-1.4L7 8.5zM3 13h10v1.5H3z"/></svg></button>
  </div>
  </div>
</footer>`;

export function createPlayer(hooks = {}) {
  document.body.insertAdjacentHTML("beforeend", BAR);
  const $ = (id) => document.getElementById(id);
  $("prev").hidden = !hooks.prev;
  $("next").hidden = !hooks.next;
  const idleText = $("artist").textContent = hooks.idleText ?? "";

  let ctx, node, ready, song = null, id = 0; // song: what's loaded ({title, artist}); id: bumped per song
  let text = null; // the MML of the song loaded, once it has arrived (for the download button)
  // Songs are rendered ahead in a worker (render.js) and handed to the audio worklet, which only copies samples.
  const renderer = new Worker(new URL("render.js", import.meta.url), { type: "module" });
  let pos = 0, dur = 0, loop = -1, dragging = false; // in frames; dur = 0 until measured, loop = -1 unless it loops

  // Song lengths are measured in a worker, so neither the page nor the audio thread stalls.
  const lengths = new Worker(new URL("length.js", import.meta.url), { type: "module" });
  lengths.onmessage = ({ data }) => {
    if (data.id !== id) return;
    if ("frames" in data) dur = data.frames;
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
      node.connect(ctx.destination);
      const { port1, port2 } = new MessageChannel();
      node.port.postMessage({ port: port1 }, [port1]);
      renderer.postMessage({ port: port2 }, [port2]);
      node.port.onmessage = ({ data }) => {
        if ("pos" in data) { pos = data.pos; showTime(); if (hooks.notes && data.notes) notes(data.notes) }
        if (data.ended) hooks.ended ? hooks.ended() : stop();
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
    [mml] = await Promise.all([mml, audio()]);
    if (me !== id) return; // another song was picked while this one loaded
    text = mml;
    renderer.postMessage({ mml, volume: meta.volume ?? 1, notes: !!hooks.notes });
    lengths.postMessage({ id: me, mml });
    await ctx.resume();
    show();
  }

  function stop() {
    renderer.postMessage({ mml: null });
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
    $("title").textContent = song ? song.title : "Nothing playing";
    $("artist").textContent = song ? song.artist || " " : idleText;
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
    if (e.target === $("seek") && e.key.startsWith("Arrow")) return; // the slider handles its own arrows
    if (e.target.closest?.("textarea, input:not([type=range]), select, [contenteditable]")) return; // typing
    if (e.key === " ") { e.preventDefault(); toggle(); }
    else if (e.key === "Escape") stop();
    else if (e.key === "ArrowLeft") seek(shown() - 5 * RATE);
    else if (e.key === "ArrowRight") seek(shown() + 5 * RATE);
  });
  show();

  return { play, stop, toggle, seek, show, get song() { return song }, get playing() { return playing() } };
}
