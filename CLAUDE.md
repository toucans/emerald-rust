# emerald-rust — rules for AI development

Read `README.md` first — it is the contract: the goal (beautiful, durable,
browser-first open-world Emerald in Rust) and the ports-and-adapters
architecture. Don't restate it here.

The rules that most often prevent wasted work:

- **No backend crate's vocabulary in the game.** The game emits plain data — a
  `Vec<Quad>` from `types` — and depends only on `types`. It consumes the `Backend` /
  `AudioSink` traits' *effects*, never their crate. `macroquad::` — or any future
  graphics/audio/windowing type — appears **only** inside its one adapter file under
  `backends/`. If you're tempted to import a crate type into `game/`, add a field to
  `Quad` or a trait method instead. This is the whole reason the repo exists.
- **Rendering is data, not calls.** The game fills a `Vec<Quad>`; the backend draws it.
  Everything Emerald draws is a textured quad (tex, src, dst, layer, tint, flip) — keep
  it that way. The quad type and `Backend` trait **do not grow** as pret data grows; only
  the code that produces quads does. Don't add a `Quad` field until the pret data forces
  it; no general "engine abstraction."
- **Bake indexed graphics to RGBA at atlas-upload time** so the backend never learns what
  a palette is (keeps it dumb and swappable). Do palette-cycle animation game-side by
  emitting a different `src`; cover global day/night with `tint`.
- **Camera is `{pos, zoom}`; smooth continuous overworld zoom is a day-one feature.** The
  game folds pan+zoom into each world quad's `dst` (fractional dst is fine); UI/text quads
  are emitted unzoomed. Zoom adds nothing to `Quad` or `Backend` — only a texture-sampling
  choice (the pixel-art crispness knob). Don't retrofit it; build the camera with `zoom`
  from the start. See README "Smooth overworld zoom".
- **World is a graph of realms, not one universal space.** Overworld = one large realm
  (carry over pokeemerald_SDL3's single overworld coordinate space); each interior = its own
  small realm; warps switch the active realm + reposition the player. **No streaming — the
  whole world (~2 MB packed, all 440 layouts + tilesets) stays resident in RAM;** just cull to
  camera-visible quads per frame. Don't force interiors into world coords. Camera/zoom operate
  within the active realm. See README "Load-bearing design".
- **Fixed-step sim, interpolated render.** Sim advances in fixed steps (~59.7275 Hz,
  deterministic, audio-locked); each render interpolates between prev/curr sim state by an
  alpha. Real shape of the loop is `step()` at fixed dt + `frame(alpha)`, not a single
  variable-dt `update`. Design in from day one — retrofitting interpolation is a rewrite.
- **Backend supports an offscreen render target + one post pass** — needed for crisp+smooth
  zoom, realm/area transitions (wipes/fades/door), and full-screen effects. Still just quads
  plus a target.
- **Keep the sim pure and serializable** — a function of `(state, input)`, no hidden globals,
  world state serializable (cheap saves/rewind/testing later).
- **The backend drives the loop.** Adapters own the event loop; each tick poll input, `step()`
  the sim at fixed dt, `frame(alpha)` to get the `Vec<Quad>`, `draw_frame`. Never make the
  game pump a specific backend.
- **Audio: follow [docs/audio-engine.md](docs/audio-engine.md).** GBA soundtrack only
  (SC-88 out of scope). Port pret's MP2K engine as the authoritative sequencer/dispatch
  (resolve keysplits/drumkits at runtime, not pre-flattened) and feed it a hi-fi 44.1 kHz
  float output stage. Music data source-of-truth stays in
  [pokeemerald_SDL3](https://github.com/toucans/pokeemerald_SDL3)/pret — never extract or
  re-derive from the ROM. The leaked `pm_eme_ose` MIDIs were SC-88 only; don't use them.
  Don't silently fork the engine from the site's shared `m4a.wasm` — see the plan.
- **pret/pokeemerald answers behavior questions.** Reference clone at `~/pokeemerald`.
  Read it; don't guess how Emerald does something. Don't port the C project's legacy
  ROM-extraction loaders wholesale.
- **Dependencies:** Rust std + the ports + one crate per adapter. Nothing else without a
  very good reason — that reason is the same monk-like test as everywhere else.
