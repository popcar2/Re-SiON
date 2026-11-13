// Worker: owns the sion.wasm instance and renders the current song ahead of playback, so a CPU hiccup eats into
// the buffer instead of the audio deadline. Chunks go straight to the worklet over a MessagePort.
//
// From the page: {port} once (the channel to the worklet), {mml, volume} to play (mml null stops), {seek: frame}.
// To the worklet: {gen, volume} = drop everything queued and start over; {gen, buf, pos} = a chunk of interleaved
//   stereo starting at frame pos; {gen, end} = no more chunks for this song.
// From the worklet: {gen, buf} = that chunk has been played (the buffer comes back to be reused).
import { Sion } from "./sion.js";

const CHUNK = 1024;   // frames per chunk (8 render quanta)
const AHEAD = 16384;  // frames kept rendered ahead of playback (~370 ms at 44.1 kHz)

const sion = Sion.load(fetch("sion.wasm"));
let port, song = null, mml = null, volume = 1, gen = 0, ahead = 0, done = true;
const spare = [];

function start(text, seekTo = 0) {
  song?.free();
  mml = text;
  song = null;
  gen++;
  ahead = 0;
  done = text == null;
  port?.postMessage({ gen, volume }); // (no port yet: stopped before audio was ever started)
  if (done) return;
  const g = gen;
  sion.then((s) => {
    if (g !== gen) return; // superseded while the wasm was still loading
    song = s.song(text);
    if (seekTo > 0 && !song.skip(seekTo)) finish();
    fill();
  });
}

function finish() {
  done = true;
  port.postMessage({ gen, end: true });
}

function fill() {
  while (song && !done && ahead < AHEAD) {
    const buf = spare.pop() ?? new Float32Array(CHUNK * 2);
    const pos = song.position;
    const playing = song.render(buf);
    port.postMessage({ gen, buf, pos }, [buf.buffer]);
    ahead += CHUNK;
    if (!playing) finish();
  }
}

onmessage = ({ data }) => {
  if (data.port) {
    port = data.port;
    port.onmessage = ({ data: { gen: g, buf } }) => {
      if (spare.length < 64) spare.push(buf);
      if (g !== gen) return;
      ahead -= buf.length >> 1;
      fill();
    };
  } else if ("mml" in data) {
    volume = data.volume ?? 1;
    start(data.mml);
  } else if ("seek" in data && mml != null) {
    // Songs only play forwards: seeking means starting over (cheap) and skipping ahead from there, or from the
    // current render position when that is still behind the target.
    const target = Math.max(0, Math.round(data.seek));
    if (song && !done && target >= song.position) {
      gen++;
      ahead = 0;
      port.postMessage({ gen, volume });
      if (!song.skip(target - song.position)) finish();
      fill();
    } else {
      start(mml, target);
    }
  }
};
