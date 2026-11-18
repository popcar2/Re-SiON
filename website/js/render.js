// Worker: owns the sion.wasm instance. Renders the current song ahead of playback, straight to the worklet over a
// MessagePort, and measures its length.
//
// From the page: {port} once (the channel to the worklet), {mml, id, volume, notes} to play (mml null stops; notes:
//   send which notes are playing), {seek: frame}.
// To the page: {id, frames} (song length), then {id, loop} (loop start frame, -1 = none).
// To the worklet: {gen, volume} = drop everything queued and start over; {gen, buf, pos, notes} = a chunk of interleaved
//   stereo starting at frame pos (notes: byte offsets in the MML of the notes playing at its end); {gen, end} = no more chunks.
// From the worklet: {gen, buf} = that chunk has been played (the buffer comes back to be reused).
import { Sion } from "./sion.js";

const CHUNK = 1024, AHEAD = 16384; // frames per chunk, frames kept rendered ahead (~370 ms)
const sion = Sion.load(fetch("../wasm/sion.wasm"));
const spare = [];
let port, song, mml, volume = 1, notes = false, gen = 0, ahead = 0, done = true;

function reset() {
  gen++;
  ahead = 0;
  port?.postMessage({ gen, volume });
}

function start(text, seekTo, id) {
  song?.free();
  song = null;
  mml = text;
  done = text == null;
  reset();
  if (done) return;
  const g = gen;
  sion.then((s) => {
    if (g !== gen) return;
    song = s.song(text);
    if (seekTo && !song.skip(seekTo)) finish();
    fill();
    // Measured after the first fill, so audio starts first. The loop pass waits a turn to let the buffer top up.
    if (id == null) return;
    postMessage({ id, frames: s.length(text) });
    setTimeout(() => postMessage({ id, loop: s.loopStart(text) }));
  });
}

function finish() {
  done = true;
  port.postMessage({ gen, end: true });
}

function fill() {
  while (song && !done && ahead < AHEAD) {
    const buf = spare.pop() ?? new Float32Array(CHUNK * 2), pos = song.position, more = song.render(buf);
    port.postMessage({ gen, buf, pos, notes: notes ? song.notes().filter((o) => o >= 0) : undefined }, [buf.buffer]);
    ahead += CHUNK;
    if (!more) finish();
  }
}

onmessage = ({ data }) => {
  if (data.port) {
    port = data.port;
    port.onmessage = ({ data: { gen: g, buf } }) => {
      if (spare.length < 64) spare.push(buf);
      if (g === gen) { ahead -= CHUNK; fill(); }
    };
  } else if ("mml" in data) {
    volume = data.volume ?? 1;
    notes = !!data.notes;
    start(data.mml, 0, data.id);
  } else if (mml != null) {
    // Songs only play forwards: skip on from the render position if it's still behind the target, else start over.
    const target = Math.max(0, Math.round(data.seek));
    if (!song || done || target < song.position) return start(mml, target);
    reset();
    song.forgetRendered();
    if (!song.skip(target - song.position)) finish();
    fill();
  }
};
