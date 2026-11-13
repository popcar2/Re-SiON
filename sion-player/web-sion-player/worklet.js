// Audio thread: plays chunks that render.js has already rendered, so all it does per callback is copy samples.
// Messages in (node.port): {port} once, the channel to render.js (see there for what travels on it).
// Out (node.port): {pos: frame, notes} now and then (notes: of the chunk playing, if render.js sends them), {ended}
// once a song has played out.

class SionProcessor extends AudioWorkletProcessor {
  gen = -1; volume = 1; queue = []; at = 0; end = false; tick = 0; pos = -1; notes;

  constructor() {
    super();
    this.port.onmessage = ({ data }) => {
      this.feed = data.port;
      this.feed.onmessage = ({ data: m }) => {
        if (m.buf) { if (m.gen === this.gen) this.queue.push(m); else this.recycle(m); }
        else if (m.end) { if (m.gen === this.gen) this.end = true; }
        else {
          for (const c of this.queue) this.recycle(c);
          this.queue = []; this.at = 0; this.end = false;
          this.gen = m.gen; this.volume = m.volume; this.pos = -1; this.notes = undefined;
        }
      };
    };
  }

  recycle(c) { this.feed.postMessage({ gen: c.gen, buf: c.buf }, [c.buf.buffer]); }

  process(_inputs, [[l, r]]) {
    const n = l.length, v = this.volume;
    let i = 0;
    while (i < n && this.queue.length) {
      const c = this.queue[0], b = c.buf, frames = b.length >> 1;
      const take = Math.min(n - i, frames - this.at);
      for (let k = 0, j = this.at * 2; k < take; k++, j += 2) { l[i + k] = b[j] * v; r[i + k] = b[j + 1] * v; }
      i += take;
      this.at += take;
      this.pos = c.pos + this.at;
      this.notes = c.notes;
      if (this.at === frames) { this.recycle(this.queue.shift()); this.at = 0; }
    }
    // Underrun (or nothing playing): the output buffers start zeroed, so the rest is silence.
    if (this.pos >= 0 && ++this.tick % 16 === 0) this.port.postMessage({ pos: this.pos, notes: this.notes });
    if (this.end && !this.queue.length) {
      this.end = false;
      this.pos = -1;
      this.port.postMessage({ ended: true });
    }
    return true;
  }
}

registerProcessor("sion", SionProcessor);
