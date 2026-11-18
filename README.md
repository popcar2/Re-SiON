# Re-SiON

Re-SiON is a tiny software synthesizer library (~40kb WASM gzip'd) that plays plaintext Music Macro Language files, providing fantastic quality music and sound effects at negligible file sizes.

This is a Rust re-implementation of [the SiON library](https://github.com/keim/SiON), originally created by keim for Adobe Flash many moons ago.
Re-SiON brings it back for the web and desktop platforms via WebAssembly, a Rust crate, and a shared library (.dll/.so).

This project also comes with [a playground](https://popcar2.github.io/Re-SiON/playground) to write and test MML, and [a converter](https://popcar2.github.io/Re-SiON/convert) that converts from other tracker formats (e.g. MOD, XM, IT) and retro video game formats (e.g. NSF, VGZ, SPC) to readable MML.

## But why use Re-SiON?

- It's a tiny, no-dependency library. Perfect for use in small web apps and game engines.
- MML files are tiny, an entire song can fit in only a few kilobytes!
- MML files are plaintext, making them easy to edit without an external program.
- The converter translates many different file formats to MML, allowing you to play all your chiptune in one unified player without juggling file formats.

# Usage

For how to use the library, refer to the [documentation](https://popcar2.github.io/Re-SiON/docs). The website and sion-desktop-player serve as examples on how to implement Re-SiON.

For how to write MML, refer to the [SiON MML Reference Manual](https://keim.github.io/SiON/mmlref/sion_mml_reference_e.html), or [read the summary in the docs](http://localhost:8000/docs#mml-support). See also [the playground](http://localhost:8000/playground) for testing.

# Credits

Re-SiON is based on [the SiON library](https://github.com/keim/SiON) by keim, while referencing [GDSiON](https://github.com/YuriSizov/gdsion) by Yuri Sizov for correctness.

# License

This project uses the [MIT License](LICENSE). Both the original SiON and GDSiON also use MIT and are credited inside.

---

*AI Disclosure: This project is AI-assisted with heavy handholding and testing. It might need more testing before being used for production, but it works well as you can see on the website. Do with this info what you will.*