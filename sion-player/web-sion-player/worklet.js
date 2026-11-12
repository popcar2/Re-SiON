// Audio thread: owns the sion.wasm instance and renders the current song into the output.
// Messages in: {wasm} once, {mml, volume} to play (mml null stops), {seek: frame}. Out: {ready}, {pos: frame}, {ended}.
import { Sion } from "./sion.js";

// Some browsers (Chrome) have no TextEncoder in AudioWorkletGlobalScope; sion.js only needs encode().
globalThis.TextEncoder ??= class {
  encode(s) {
    const out = [];
    for (const ch of s) {
      const c = ch.codePointAt(0);
      if (c < 0x80) out.push(c);
      else if (c < 0x800) out.push(0xc0 | c >> 6, 0x80 | c & 63);
      else if (c < 0x10000) out.push(0xe0 | c >> 12, 0x80 | c >> 6 & 63, 0x80 | c & 63);
      else out.push(0xf0 | c >> 18, 0x80 | c >> 12 & 63, 0x80 | c >> 6 & 63, 0x80 | c & 63);
    }
    return new Uint8Array(out);
  }
};

class SionProcessor extends AudioWorkletProcessor {
  sion = null; song = null; mml = null; volume = 1; buf = new Float32Array(256); tick = 0;

  constructor() {
    super();
    this.port.onmessage = async ({ data }) => {
      if (data.wasm) {
        this.sion = await Sion.load(data.wasm);
        this.port.postMessage({ ready: true });
      } else if ("mml" in data) {
        this.load(data.mml);
        this.volume = data.volume ?? 1;
      } else if ("seek" in data && this.song) {
        // Songs only play forwards: going back means starting over and skipping ahead from the beginning.
        if (data.seek < this.song.position) this.load(this.mml);
        if (!this.song.skip(Math.max(0, Math.round(data.seek - this.song.position)))) this.end();
        this.report();
      }
    };
  }

  load(mml) {
    this.song?.free();
    this.mml = mml;
    this.song = mml == null ? null : this.sion.song(mml);
  }

  end() {
    this.load(null);
    this.port.postMessage({ ended: true });
  }

  report() { if (this.song) this.port.postMessage({ pos: this.song.position }); }

  process(_inputs, [[l, r]]) {
    if (!this.song) return true;
    const n = l.length;
    if (this.buf.length < n * 2) this.buf = new Float32Array(n * 2);
    const b = this.buf.subarray(0, n * 2);
    const playing = this.song.render(b);
    const v = this.volume;
    for (let i = 0; i < n; i++) { l[i] = b[2 * i] * v; r[i] = b[2 * i + 1] * v; }
    if (++this.tick % 16 === 0) this.report();
    if (!playing) this.end();
    return true;
  }
}

registerProcessor("sion", SionProcessor);
