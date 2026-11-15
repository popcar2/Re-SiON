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
//
//   Following along: song.notes() gives, per track, the byte offset in the MML of the note it is playing.

const enc = new TextEncoder();

export class Sion {
  static async load(source) {
    source = await source;
    const r = typeof Response !== "undefined" && source instanceof Response
      ? await WebAssembly.instantiateStreaming(source, {})
      : await WebAssembly.instantiate(source, {}); // a Module gives an Instance, bytes give { module, instance }
    return new Sion((r.instance ?? r).exports);
  }

  constructor(exports) {
    this.x = exports;
    this.rate = exports.sion_rate();
  }

  /** Parse MML text into a Song. */
  song(mml) { return new Song(this, this.#withMml(mml, this.x.sion_song_new)); }

  /** Length in frames of one pass through the MML (until every track ends or loops back once), without rendering. */
  length(mml) { return this.#withMml(mml, this.x.sion_length); }

  /** For MML with a `$` loop: the frame playback carries on from after `length` frames (where the loop
   *  point is). -1 if the song ends instead. */
  loopStart(mml) { return this.#withMml(mml, this.x.sion_loop_start); }

  #withMml(mml, f) { return this._with(enc.encode(mml), f); }

  /** (Internal.) Copy typed array `data` into wasm memory, call `f(ptr, data.length)`, free the copy, return f's result. */
  _with(data, f) {
    const x = this.x, len = Math.max(data.byteLength, 1);
    const ptr = x.sion_alloc(len);
    new data.constructor(x.memory.buffer, ptr, data.length).set(data);
    try { return f(ptr, data.length); } finally { x.sion_dealloc(ptr, len); }
  }
}

export class Song {
  #s; #h; #out = 0; #frames = 0; #notes = 0; #cap = 0;
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

  /** Per track, the byte offset in the MML text (as UTF-8) of the note it is playing now, -1 for none. */
  notes() {
    const x = this.#s.x;
    let n = this.#notes ? x.sion_song_notes(this.#h, this.#notes, this.#cap) : 16;
    while (n > this.#cap) { // first call, or more tracks than fit: (re)allocate and ask again
      if (this.#notes) x.sion_dealloc(this.#notes, this.#cap * 4);
      this.#notes = x.sion_alloc((this.#cap = n) * 4);
      n = x.sion_song_notes(this.#h, this.#notes, this.#cap);
    }
    return Array.from(new Int32Array(x.memory.buffer, this.#notes, n));
  }

  /** Frames played (rendered or skipped) so far. */
  get position() { return this.#s.x.sion_song_position(this.#h); }

  /** Set PCM sample `n` (0..255) for %7 / %10: mono Float32Array in -1..1 at `rate` Hz, MIDI `key` (60 = as recorded).
   *  `loopStart`: frame to loop from (-1 = no loop); `bidi`: loop back and forth. */
  setSample(n, data, rate, key = 60, loopStart = -1, bidi = false) {
    this.#s._with(data instanceof Float32Array ? data : Float32Array.from(data), (ptr, frames) =>
      this.#s.x.sion_song_set_sample(this.#h, n, ptr, frames, rate, key, loopStart, bidi ? 1 : 0));
  }

  free() {
    const x = this.#s.x;
    if (this.#out) x.sion_dealloc(this.#out, this.#frames * 8);
    if (this.#notes) x.sion_dealloc(this.#notes, this.#cap * 4);
    x.sion_song_free(this.#h);
    this.#h = this.#out = this.#frames = this.#notes = this.#cap = 0;
  }
}
