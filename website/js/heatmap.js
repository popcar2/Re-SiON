// A heatmap of the song's spectrum, rising from the bottom of the window behind the player bar's controls: low notes on the left,
// high on the right, and the louder a band, the brighter and taller its glow. player.js starts it when the page
// asks for it (createPlayer({ heatmap: true })).
//
//   const heat = heatmap(ctx, node, footer, playing);  // node: the audio to show; playing(): whether to draw it
//   heat.wake();                                       // call when playback starts, so drawing resumes
//   heat.enable(on);                                   // show or hide it (hidden, it doesn't draw at all)

const LOW = 40, HIGH = 10000; // Hz, the range shown, spaced logarithmically
const SUB = 4;                // pixels computed per column: the levels are curved between columns, then scaled up
const FADE = 0.9;             // per frame, how fast the heat dies away once paused or stopped

// Only the intense parts show: they fade in from nothing at INTENSE in the page's ice-blue accent, and turn white
// toward the most intense.
const INTENSE = 0.4; // how intense a point must be to show at all
const EDGE = 0.1;    // over how much more intensity it fades in (a soft edge rather than a hard cutoff)

export function heatmap(ctx, node, footer, playing, on = true) {
  const analyser = ctx.createAnalyser();
  analyser.fftSize = 2048;
  analyser.smoothingTimeConstant = 0.7;
  analyser.minDecibels = -90;
  analyser.maxDecibels = -25;
  node.connect(analyser);
  const bins = new Uint8Array(analyser.frequencyBinCount);

  const canvas = document.createElement("canvas");
  canvas.className = "heatmap";
  canvas.setAttribute("aria-hidden", "true");
  canvas.hidden = !on;
  footer.prepend(canvas); // (inside the bar, under its controls: see style.css)
  const g = canvas.getContext("2d");
  // Drawn at the canvas's full height but only SUB pixels per column, then stretched across: a glow that is
  // smooth up the page, without working out every pixel of the width.
  const cells = document.createElement("canvas").getContext("2d");

  let cols = 0, ranges = [], heat = new Float32Array(0), level = new Float32Array(0), image, color, frame = 0;

  // Columns: about one per 18 px of width, each covering a slice of the frequency range. (Fewer, wider bands
  // are steadier than many narrow ones.)
  function layout(rows) {
    const n = Math.max(20, Math.min(90, Math.round(innerWidth / 18)));
    if (n !== cols) {
      cols = n;
      heat = new Float32Array(n);
      level = new Float32Array(n);
      const hz = ctx.sampleRate / analyser.fftSize;
      ranges = Array.from({ length: n }, (_, i) => {
        const a = LOW * (HIGH / LOW) ** (i / n), b = LOW * (HIGH / LOW) ** ((i + 1) / n);
        const lo = Math.floor(a / hz);
        return [lo, Math.max(lo + 1, Math.ceil(b / hz))];
      });
    }
    if (!image || image.width !== n * SUB || image.height !== rows) {
      cells.canvas.width = n * SUB;
      cells.canvas.height = rows;
      image = cells.createImageData(n * SUB, rows);
    }
  }

  // 256 colours from quiet to loud: clear below INTENSE, then fading in and turning from accent to white.
  function palette() {
    const accent = getComputedStyle(document.documentElement).getPropertyValue("--accent").trim();
    const m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(accent);
    const accent3 = m ? [1, 2, 3].map((k) => parseInt(m[k], 16)) : [42, 143, 196];
    color = new Uint8ClampedArray(256 * 4);
    for (let i = 0; i < 256; i++) {
      const t = i / 255, u = Math.max(0, (t - INTENSE) / (1 - INTENSE)), e = Math.min(1, Math.max(0, (t - INTENSE) / EDGE));
      for (let c = 0; c < 3; c++) color[i * 4 + c] = accent3[c] + (255 - accent3[c]) * u;
      color[i * 4 + 3] = 255 * e * e * (3 - 2 * e) * 0.75;
    }
  }
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", palette);
  palette();

  function draw() {
    const w = canvas.clientWidth, h = canvas.clientHeight, dpr = devicePixelRatio || 1;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    const rows = Math.max(1, Math.min(160, canvas.height));
    layout(rows);

    const live = playing();
    if (live) {
      analyser.getByteFrequencyData(bins);
      for (let i = 0; i < cols; i++) {
        // Half the band's loudest bin (so a lone chiptune tone still counts), half its average (so one bin
        // flickering doesn't swing the whole band).
        let top = 0, sum = 0;
        const [lo, hi] = ranges[i];
        for (let b = lo; b < hi; b++) { top = Math.max(top, bins[b]); sum += bins[b] }
        let v = (top + sum / (hi - lo)) / 2;
        // Music has less energy up high; tilt the highs up a little so the right side isn't always dark.
        v = Math.min(1, v / 255 * (1 + 0.3 * i / cols));
        // Ease toward it: up almost at once, so hits land on the beat; down quickly enough to follow the music,
        // but not so fast it flickers.
        heat[i] += (v - heat[i]) * (v > heat[i] ? 0.7 : 0.18);
      }
    } else for (let i = 0; i < cols; i++) heat[i] *= FADE;

    // Neighbours blend (over two columns each side), so single bands don't spike.
    let any = false;
    for (let i = 0; i < cols; i++) {
      const at = (j) => heat[Math.min(cols - 1, Math.max(0, j))];
      level[i] = (at(i - 2) + 4 * at(i - 1) + 6 * heat[i] + 4 * at(i + 1) + at(i + 2)) / 16;
      if (level[i] > 0.01) any = true;
    }

    // Each point is hottest at the bar and cools to nothing at its level. Levels between column centres follow a
    // curve through them (Catmull-Rom), so the shapes roll rather than step.
    const px = image.data, iw = cols * SUB;
    for (let x = 0; x < iw; x++) {
      const p = (x + 0.5) / SUB - 0.5, i = Math.floor(p), t = p - i;
      const a = level[Math.max(0, i - 1)], b = level[Math.max(0, i)], c = level[Math.min(cols - 1, i + 1)], d = level[Math.min(cols - 1, i + 2)];
      const v = Math.min(1, Math.max(0, b + 0.5 * t * (c - a + t * (2 * a - 5 * b + 4 * c - d + t * (3 * (b - c) + d - a)))));
      for (let y = 0; y < rows; y++) {
        const up = (y + 0.5) / rows, f = v > up ? 1 - up / v : 0;
        const k = Math.round(255 * v * f ** 1.2) * 4, o = ((rows - 1 - y) * iw + x) * 4;
        px[o] = color[k]; px[o + 1] = color[k + 1]; px[o + 2] = color[k + 2]; px[o + 3] = color[k + 3];
      }
    }
    cells.putImageData(image, 0, 0);

    g.imageSmoothingEnabled = true;
    g.imageSmoothingQuality = "high";
    g.clearRect(0, 0, canvas.width, canvas.height);
    g.drawImage(cells.canvas, 0, 0, canvas.width, canvas.height);

    // Keep going while playing or still fading out; otherwise sleep until wake().
    frame = live || any ? requestAnimationFrame(draw) : 0;
  }

  const wake = () => { if (on && !frame) frame = requestAnimationFrame(draw) };
  return {
    wake,
    enable(v) {
      on = v;
      canvas.hidden = !on;
      if (on) { if (playing()) wake(); return }
      // Off: stop drawing, and start from nothing when turned back on.
      cancelAnimationFrame(frame);
      frame = 0;
      heat.fill(0);
    },
  };
}
