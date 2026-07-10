# emerald-rust

**Open-world Pokémon Emerald you can walk around in**, written in Rust —
browser-first (WASM) and native from one codebase, with
[pret/pokeemerald](https://github.com/pret/pokeemerald) as the source of
reference for all data and behavior. *Exploration first*, not a full
recreation — built to be the most **beautiful, durable, legible** version
of that idea.

**▶ Play it now: <https://toucans.github.io/emerald-rust/>**

**It runs.** The whole of Hoenn (518 maps, 470 realms) is walkable at the GBA's own
pace — running shoes, the Mach bike with its real acceleration, surfing, collision
and elevation from pret's mapgrid, doors that open/close with the GBA's full warp
choreography (fades, walk-in/walk-out, door and exit sounds), smooth continuous
zoom with pixel-perfect presets, a start menu in Emerald's own font (levitation
mode, save-anywhere), touch controls on phones, and the GBA soundtrack + sound
effects from a from-scratch m4a engine that switches songs as you ride and roam.

This file is the contract: the architecture below is what the code implements, and
changes to either happen together. The build philosophy throughout: smallest
long-lived dependency surface, boring proven tech, own what you depend on. AI
development rules are in [CLAUDE.md](CLAUDE.md).

## Play it / build it

- **Web (the primary target):** live at
  [toucans.github.io/emerald-rust](https://toucans.github.io/emerald-rust/),
  rebuilt from source by the pages workflow on every push. `web/build.sh`
  assembles the same tree into `web/dist/` — any static file server can
  serve it; there is no backend.
- **Native:** `cargo run --release -p backends --bin emerald` (needs a display;
  audio optional). `--bin emerald-headless` runs the same game with no graphics
  crate at all — the proof the backend swap is one file, and a CI smoke test.
- **Controls** (the GBA pad, mapped): **A = Z · B = X · Start = Enter · Select =
  Backspace.** Arrows walk, holding B (X) runs, A (Z) at the water's edge surfs,
  Select (Backspace) toggles the Mach bike, Start (Enter) opens the menu; mouse
  wheel or Q/E zoom continuously. On touch devices an on-screen pad appears —
  slide a finger across the d-pad's diamond zones to change direction without
  lifting.
- **The start menu** (Enter): LEVITATE — hover and walk through everything (warps
  become instant, no door animations), bobbing at the tileset-animation interval;
  ZOOM — cycle the pixel-perfect integer zooms 1x-4x; SAVE / LOAD — the in-memory
  save-anywhere slot (a 37-byte serialized sim state); EXIT.
- **Data packs** (gitignored, regenerable, derived from the pret clone at
  `~/pokeemerald`): `cargo run -p worldgen --release` → `assets/world.bin` (~2 MB),
  `cargo run -p musicgen --release` → `assets/music/` — one small `sfx.bin`
  (every se_* effect) plus one self-contained pack per mus_* song, so the web
  page streams a song only when a map asks for it.
- **Audio acceptance harness:** `cargo run -p audio --bin verify --release`
  (all 473 songs — music and the se_* sound effects; `--wav out.wav` renders one).
- **Visual check without a display:** `cargo run -p worldgen --bin framedump`
  software-renders a frame to PNG.

## The one decision everything else follows from

**No graphics/audio/windowing crate's vocabulary is allowed into the game.** The game
depends only on thin **ports** (traits) written in the game's own words. Every crate —
macroquad today, whatever wins in 2030 — lives *behind* a port as a single **adapter**
file.

Why: the durable asset is the game (world, movement, camera, the engine logic). Graphics
crates churn; the WebGPU/wgpu world especially. If the game imports `macroquad::` types
directly, swapping backends later means touching everything. If it depends only on *our*
tiny seam — plain frame **data** the game emits, plus one small `Backend` trait that
consumes it — swapping backends is **one adapter file** and the game is untouched. The
seam is the thing we own and understand; the crate is a replaceable cache. This is
ports-and-adapters (hexagonal), and it is the whole point of doing the rewrite well.

The seam doubles as the spec for a swap: "consume this data / implement this trait
against crate X" is a crisp, self-contained task. `backends/src/headless.rs` proves it.

**Keep the seam narrow and domain-shaped.** The game emits one kind of thing (a textured
quad) and needs one audio sink. Do *not* build a general "engine abstraction" — that is
over-engineering and, counterintuitively, *less* swappable. Narrowness is what keeps it
beautiful. The seam **does not change** as the game grows richer — only the code that
*produces* the data does.

## Rendering: the game emits quads; the backend draws them

The game produces an immutable description of each frame and the backend renders it.
The game never calls a rendering method, never clears, never presents. Everything
Emerald draws — a metatile, the player, a menu — is **a textured quad from an atlas,
placed at a rect, on a layer, optionally tinted and/or flipped** (that is literally the
GBA's own screen model: tiled BGs + OAM sprites with priority, palette, flip bits).
The types live in [`types/`](types/), the seam crate:

- `Quad { tex, src, dst, layer, tint, flip }` — `layer` is GBA priority (stable-sorted,
  vec order breaks ties), `tint` buys day/night/fades, `flip` is core GBA sprite
  semantics. Resist adding a seventh field until the pret data demands it.
- `Frame { offscreen, target_size, screen }` — one frame = an **offscreen pass**
  rendered into a target the game sizes, then a **screen pass** composited to the
  window, where `TextureId::TARGET` samples the target. Still just quads plus a
  target. This buys crisp+smooth zoom (below), realm-transition wipes/fades, and any
  full-screen effect — the things a draw-straight-to-screen backend can't do.
- `trait Backend { load_atlas, poll_input, draw_frame }` — the whole rendering seam.
- `trait AudioSink { queue }` — the whole audio seam.

`World::frame(alpha, w, h, &mut Frame)` is pure and fills reused buffers — no
allocation in the steady state, trivially testable (assert on the quad lists, no mock
GPU).

### Smooth overworld zoom lives in the camera

A first-class feature: **smooth, continuous zoom** of the overworld, no fixed steps.
It is a *camera* property and stays in the game: the camera is `{ pos, zoom }`, world
quads fold both into `dst` (fractional rects are fine), UI quads are emitted
screen-space so they stay put. Nothing zoom-related exists in `Quad` or `Backend`.

Crisp *and* smooth comes from the two-pass frame: the world is drawn into the
offscreen target at an **integer** pixel scale (nearest sampling keeps pixels hard),
and the screen pass maps the target to the continuous zoom (linear minification stays
smooth).

### Assets: bake indexed graphics to RGBA at atlas-upload time

pret graphics are indexed (4bpp tiles + 16-color palettes). The packs carry them
*indexed*; at startup [`game/src/bake.rs`](game/src/bake.rs) renders every (tileset
pair, metatile) layer to RGBA, dedupes by content, and packs 2048² atlases the backend
uploads. The backend never learns what a palette is — the load-bearing choice that
keeps it dumb and universally swappable. Palette-cycle animation (water) will be done
game-side by emitting a different `src` per tick; global day/night with `tint`.

## Who owns the loop

> **The backend drives.** The adapter owns the event loop; each tick it polls input,
> `step()`s the sim at fixed dt, asks `frame(alpha)` for the quads, and draws them.

```rust
let input = backend.poll_input();
let (steps, alpha) = clock.advance(elapsed);
for _ in 0..steps { world.step(input); }
world.frame(alpha, w, h, &mut frame);   // pure
backend.draw_frame(&frame);             // the one stateful call
```

This shape fits both backend camps — *they drive you* (macroquad, rAF) and *you drive
them* (SDL-style) — so every future backend drops in.

## Load-bearing design, built in from day one

- **World = a graph of realms** ([`game/src/map.rs`](game/src/map.rs)). A realm is one
  self-contained coordinate space: the overworld is ONE large realm (pokeemerald_SDL3's
  stitched 800×383 space, carried over exactly — validated against its coordinates);
  each interior is its own small realm. **Warps are edges**: a warp switches the active
  realm and repositions the player, and is never interpolated across. Interiors don't
  belong in world coords — they have no spatial relationship to the outside.
- **No streaming — the whole world stays resident in RAM** (~2 MB packed: all 440
  layouts + tilesets, measured). `frame()` still culls to the camera-visible rect;
  resident ≠ "draw everything". At Emerald's scale streaming would be needless
  machinery — don't build it.
- **Fixed-step sim, interpolated render** ([`game/src/lib.rs`](game/src/lib.rs)). The
  sim ticks at the GBA's ~59.7275 Hz (deterministic, audio-locked); each render lerps
  prev/curr state by an alpha, so motion is smooth at any refresh rate. Every moving
  entity stores prev+curr — this was designed in because retrofitting it is a rewrite.
- **Pure, serializable state.** The sim is a function of `(state, input)`, no hidden
  globals; `State` is one Copy struct with a 34-byte serialization
  (`World::save_state`/`load_state`) and a restored sim continues deterministically —
  save-anywhere and rewind are cheap by construction. Tests pin all of these
  guarantees.

## Audio: the same pattern

The m4a/MP2K engine ([`audio/`](audio/)) is **pure logic** — score + samples in,
44.1 kHz (any-rate) float stereo out, no I/O, no device — behind the `AudioSink` port.
Sequencing and instrument dispatch are ported from **pret's engine as ground truth**
(the 59.7275 Hz frame tick, MP2K tempo/volume/pan/pitch math, envelopes, and runtime
keysplit/drumset resolution against the raw voicegroup tables); the output stage is
**hi-fi on purpose** (float mixing, interpolated samples, synthesized PSG). Design,
decisions, and the fidelity ceiling: **[docs/audio-engine.md](docs/audio-engine.md)**
— read it before touching audio. GBA soundtrack only; SC-88 is out of scope.

Adapters: **cpal** (native, `backends/src/audio_cpal.rs`) and **WebAudio**
(`backends/src/audio_web.rs` + the pull loop in `web/index.html`).

The acceptance bar is enforced by the harness (`cargo run -p audio --bin verify`):
every song's every note either sounds or is a classified authored-data quirk. The
original target — `mus_cycling` complete, keysplit piano and drumset included —
passes with zero drops. **Decided:** the pokeemerald-music site will switch onto this
engine (wasm replacing `m4a.wasm`), dropping SC-88 at the switch.

## Data & assets: pret is the source of truth

This is a rewrite of the *program*. Game data and assets are **derived from pret**
(reference clone at `~/pokeemerald`) — never invented here, never from the ROM:

- **Maps/graphics/behavior:** extracted by [`worldgen/`](worldgen/) from pret's
  layouts, tilesets, and map JSON. Behavior questions are answered by reading pret,
  not guessing.
- **Music:** re-extracted from pret by [`musicgen/`](musicgen/) — raw voicegroups,
  keysplit tables, drumsets, sample bank. pokeemerald_SDL3's `music.pak` is the output
  of the old buggy flattening and is never an input.
- Each pack format is defined **once, in Rust, shared between extractor and engine**
  (`game/src/data.rs`, `audio/src/data.rs`) — no separate spec to drift.
- The C project's ROM-extraction loaders are legacy; nothing here reads `p.gba`.

## Shape of the repo

```
types/      the seam: Quad, Frame, Input, Backend, AudioSink. Plain data we own.
game/       pure game logic; emits Frame data. Depends only on `types`.
backends/   one file per adapter, each owning the loop: macroquad.rs, headless.rs,
            audio_cpal.rs, audio_web.rs.
audio/      the m4a engine (pure logic) + the pack format + the verify harness.
worldgen/   pret → assets/world.bin extractor (+ framedump debug renderer).
musicgen/   pret → assets/music/ extractor (sfx.bin + one pack per song).
web/        static web shell + build.sh → web/dist (what /emerald serves).
docs/       audio-engine.md — the audio design doc.
```

Each directory has a short README pointing back here; this file stays the single
source of truth for the architecture.

## Deployment: fully static, no backend

The web build is fully static: `.wasm` + `gl.js` + `index.html` + the world pack
and the per-song music packs. GitHub Pages builds and serves it from source on
every push (`.github/workflows/pages.yml`); ~1.1 MB on the wire for first load,
songs streamed on demand. `web/build.sh` produces the identical tree in
`web/dist/` for any other static file server. Nothing else to run.

## Status & what's next

Everything above is implemented and tested. Natural next steps, roughly in
order of payoff:

- **Object events**: NPCs, and the overworld props (truck, boats) that live in map
  data but aren't drawn yet.
- **Realm transitions**: fades/wipes through the offscreen post pass on warp.
- **Animated tiles** (water, flowers) via per-tick `src` swaps; day/night via `tint`.
- **More metatile behaviors**: ledges (jumping), waterfalls, currents, ice.
- **Persist the save slot** (native: a file; web: localStorage) — the menu's
  SAVE/LOAD currently lives for the session.
- **Switch the music site onto the Rust engine** (the decided plan in
  docs/audio-engine.md) once its render quality is confirmed by ear across the
  soundtrack.
