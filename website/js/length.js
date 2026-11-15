// Worker: measures song lengths (Sion.length, Sion.loopStart) off the page and audio threads. In: {id, mml}. Out: {id, frames}, then {id, loop} (loop start frame, -1 = none).
import { Sion } from "./sion.js";

const sion = Sion.load(fetch("../wasm/sion.wasm"));
onmessage = async ({ data: { id, mml } }) => {
  const s = await sion;
  postMessage({ id, frames: s.length(mml) });
  postMessage({ id, loop: s.loopStart(mml) }); // a second pass: sent after, so the length shows without waiting for it
};
