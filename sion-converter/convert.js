// SiON MML converter: JS wrapper for convert.wasm (browser, Node, Deno; no dependencies).
//
//   const conv = await Converter.load(fetch("convert.wasm"));   // or bytes / a WebAssembly.Module
//   const r = conv.convert(fileBytes, { grid, gain, wavetables, exact, exactPcm, track });
//   if (r.ok) download(r.mml); else alert(r.error);
//
//   r: { ok, mml, format, tracks, notes, bytes, plainBytes, secs, loopSecs, warnings } or { ok: false, error }.
//   Conversion runs synchronously and can take seconds: call it from a Worker. A crash on a malformed file
//   (a wasm trap) is thrown as an Error; the converter starts over with a fresh instance for the next file.

const dec = new TextDecoder();

export class Converter {
  #module; #x;

  static async load(source) {
    source = await source;
    const module = typeof Response !== "undefined" && source instanceof Response
      ? await WebAssembly.compileStreaming(source)
      : source instanceof WebAssembly.Module ? source : await WebAssembly.compile(source);
    return new Converter(module);
  }

  constructor(module) { this.#module = module; this.#fresh(); }

  #fresh() { this.#x = new WebAssembly.Instance(this.#module, {}).exports; }

  /** Convert a file (Uint8Array / ArrayBuffer). Options as on the command line (all optional). */
  convert(bytes, { grid = 0, gain = 0, wavetables = false, exact = false, exactPcm = false, track = 0 } = {}) {
    const x = this.#x, data = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
    const ptr = x.convert_alloc(data.length);
    new Uint8Array(x.memory.buffer, ptr, data.length).set(data);
    const flags = (wavetables ? 1 : 0) | (exact ? 2 : 0) | (exactPcm ? 4 : 0);
    try {
      x.convert_run(ptr, data.length, grid, gain, flags, track);
    } catch (e) {
      // A trap leaves the instance in an unknown state: start over with a new one.
      this.#fresh();
      throw new Error("the converter crashed on this file (" + e.message + ")");
    }
    x.convert_dealloc(ptr, data.length);
    const out = (i) => dec.decode(new Uint8Array(x.memory.buffer, x.convert_output(i), x.convert_output_len(i)));
    const r = JSON.parse(out(1));
    if (r.ok) r.mml = out(0);
    x.convert_clear();
    return r;
  }
}
