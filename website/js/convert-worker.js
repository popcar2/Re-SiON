// Worker for convert.html: runs convert.wasm off the page's thread (a conversion can take seconds).
// From the page: {id, bytes, options}. To the page: {id, result} (see convert.js) or {id, error}.
import { Converter } from "./convert.js";

const conv = Converter.load(fetch("../wasm/convert.wasm"));

onmessage = async ({ data: { id, bytes, options } }) => {
  try {
    postMessage({ id, result: (await conv).convert(bytes, options) });
  } catch (e) {
    postMessage({ id, error: e.message ?? String(e) });
  }
};
