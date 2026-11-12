// SiON MML player: JS wrapper for sion.wasm (browser, Node, Deno; no dependencies).
//
//   const sion = await Sion.load(fetch("sion.wasm"));   // or bytes / a WebAssembly.Module
//   const song = sion.song(mmlText);
//   const buf = new Float32Array(2 * 1024);              // interleaved stereo, sion.rate Hz
//   while (song.render(buf)) { /* play buf */ }          // returns false once finished
//   song.free();
//
//   Seeking: song.skip(frames) jumps ahead (song.position tells where it is); sion.length(mml) gives a song's
//   length in frames. To go back, make a new song and skip from the start.

export class Sion {
  static async load(source) {
    source = await source;
    const { instance } = source instanceof WebAssembly.Module
      ? { instance: await WebAssembly.instantiate(source, {}) }
      : typeof Response !== "undefined" && source instanceof Response
        ? await WebAssembly.instantiateStreaming(source, {})
        : await WebAssembly.instantiate(source, {});
    return new Sion(instance.exports);
  }

  constructor(exports) {
    this.x = exports;
    this.rate = exports.sion_rate();
  }

  /** Parse MML text into a Song. */
  song(mml) {
    const bytes = new TextEncoder().encode(mml);
    const ptr = this.#copyIn(bytes);
    const handle = this.x.sion_song_new(ptr, bytes.length);
    this.x.sion_dealloc(ptr, bytes.length);
    return new Song(this, handle);
  }

  /** Length in frames of one pass through the MML (until every track ends or loops back once), without rendering. */
  length(mml) {
    const bytes = new TextEncoder().encode(mml);
    const ptr = this.#copyIn(bytes);
    const frames = this.x.sion_length(ptr, bytes.length);
    this.x.sion_dealloc(ptr, bytes.length);
    return frames;
  }

  /** For MML with a `$` loop: the frame playback carries on from after `length` frames (where the loop
   *  point is). -1 if the song ends instead. */
  loopStart(mml) {
    const bytes = new TextEncoder().encode(mml);
    const ptr = this.#copyIn(bytes);
    const frame = this.x.sion_loop_start(ptr, bytes.length);
    this.x.sion_dealloc(ptr, bytes.length);
    return frame;
  }

  #copyIn(bytes) {
    const ptr = this.x.sion_alloc(Math.max(bytes.length, 1));
    new Uint8Array(this.x.memory.buffer, ptr, bytes.length).set(bytes);
    return ptr;
  }
}

export class Song {
  #s; #h; #out = 0; #frames = 0;
  constructor(sion, handle) { this.#s = sion; this.#h = handle; }

  /** Fill `buf` (Float32Array, interleaved L/R) with audio. Returns false once the song has finished. */
  render(buf) {
    const x = this.#s.x, frames = buf.length >> 1;
    if (frames > this.#frames) {
      if (this.#out) x.sion_dealloc(this.#out, this.#frames * 8);
      this.#out = x.sion_alloc(frames * 8);
      this.#frames = frames;
    }
    const playing = x.sion_song_render(this.#h, this.#out, frames);
    buf.set(new Float32Array(x.memory.buffer, this.#out, frames * 2));
    return playing !== 0;
  }

  /** Move `frames` ahead without output, for seeking: fast, and close to (not exactly) what playing up to
   *  there would give. Returns false once the song has finished. */
  skip(frames) { return this.#s.x.sion_song_skip(this.#h, frames) !== 0; }

  /** Frames played (rendered or skipped) so far. */
  get position() { return this.#s.x.sion_song_position(this.#h); }

  /** Set PCM sample `n` (0..255) for %7 / %10: mono Float32Array in -1..1 at `rate` Hz, MIDI `key` (60 = as recorded).
   *  `loopStart`: frame to loop from (-1 = no loop); `bidi`: loop back and forth. */
  setSample(n, data, rate, key = 60, loopStart = -1, bidi = false) {
    const x = this.#s.x, len = data.length * 4;
    const ptr = x.sion_alloc(Math.max(len, 1));
    new Float32Array(x.memory.buffer, ptr, data.length).set(data);
    x.sion_song_set_sample(this.#h, n, ptr, data.length, rate, key, loopStart, bidi ? 1 : 0);
    x.sion_dealloc(ptr, Math.max(len, 1));
  }

  free() {
    const x = this.#s.x;
    if (this.#out) x.sion_dealloc(this.#out, this.#frames * 8);
    x.sion_song_free(this.#h);
    this.#h = this.#out = this.#frames = 0;
  }
}
