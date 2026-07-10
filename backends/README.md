# backends — one file per adapter

Each adapter is a single file that implements a port from [`types`](../types/) and
owns its side of the loop ("the backend drives" — root [README](../README.md)).
Swapping or adding a backend = adding/replacing one file here; nothing else in the
repo may name the adapter's crate.

- `macroquad.rs` — the graphics adapter + main loop (`--bin emerald`). The ONLY file
  allowed to name a macroquad type. Two-pass draw: offscreen quads into a render
  target, screen quads composited (crisp+smooth zoom). Note: render targets are
  created with `sample_count: 0` — macroquad's default of 1 takes an MSAA-resolve
  path whose blit is WebGL2-only, and the web glue is WebGL1.
- `headless.rs` — the stub second backend (`--bin emerald-headless`): no graphics
  crate at all; proves the seam and smoke-tests the real loop shape.
- `audio_cpal.rs` — native `AudioSink`: ring buffer drained by cpal's callback.
- `audio_web.rs` — wasm `AudioSink`: the engine lives inside the game's wasm; the
  page's ScriptProcessorNode (main-thread callbacks) pulls PCM through two exports.
  The JS side lives in [`web/index.html`](../web/index.html).

Dependency rule: one crate per adapter (macroquad, cpal), nothing else.
