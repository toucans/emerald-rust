# web — the static shell

`./build.sh` assembles the whole site into `web/dist/` (gitignored): the wasm
build, `index.html`, `gl.js`, the world pack and the per-song music packs.
Fully static, no server process — GitHub Pages builds the same tree from
source on every push, and any static file server can serve `web/dist/`.

- `gl.js` — miniquad's wasm↔JS glue, vendored **at the version matching the pinned
  miniquad crate** (currently v0.4.8's, for miniquad 0.4.10). If `Cargo.lock` moves
  miniquad, re-vendor from that tag — a mismatched loader is exactly the
  black-screen-with-music failure.
- `index.html` — canvas shell, audio, and touch controls:
  - Music + SFX stream to an **AudioWorklet** fed from the main thread
    (clock-based, ~200 ms ahead) through the `emerald_audio_*` wasm exports.
    The whole pipeline is set up **eagerly at page load** and user gestures only
    ever call `ctx.resume()` — async setup inside a gesture loses the user
    activation on Android and leaves the context suspended forever. (A 0-input
    ScriptProcessorNode never fires on iOS — the original mobile bug; a 1-input
    SPN remains as fallback.)
  - On coarse-pointer devices an on-screen GBA pad (d-pad, A/B, START, SELECT —
    zoom lives in the start menu) is shown; buttons dispatch synthetic keyboard
    events at the canvas, so touch rides the exact same path as a keyboard. The
    d-pad is ONE touch surface: direction = |dx| vs |dy| around the center,
    which partitions it into four 45°-rotated diamond zones with no dead gaps —
    a finger slides between directions without lifting.
  - The sim state autosaves: JS mirrors the latest save (a few dozen bytes,
    via the `emerald_save_*` exports) to localStorage every few seconds and on
    `pagehide`, and pushes it back through `emerald_restore_*` at boot.
- The wasm is built with the size-tuned `web` cargo profile; music streams one
  song at a time and GitHub Pages gzips in transit — ~1.1 MB on the wire for
  first load.
- `.cargo/config.toml` (repo root) carries the one wasm link flag the glue needs.

Headless browser debugging (no display needed): serve `web/dist` with
`python3 -m http.server`, run chromium under Xvfb with `--remote-debugging-port`,
and drive console/screenshots over CDP.
