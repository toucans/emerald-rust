# game — pure game logic

Depends only on [`types`](../types/). No I/O, no hidden globals, no backend crate's
vocabulary; the sim is a pure function of `(state, input)` and rendering is
`frame(alpha) → Frame` data. Architecture: root [README](../README.md).

- `lib.rs` — `World`: fixed-step `step()` (~59.7275 Hz) + interpolated `frame(alpha)`,
  camera `{pos, zoom}`, save/load (`State` ser/de), the `FixedStep` accumulator, the
  start menu, and the warp choreography (`Transition`): door opens 16 frames → walk
  in → fade 12 → realm switch → fade 12; exterior-door arrivals walk the player down
  off the door and close it. Levitate skips all transitions.
- `map.rs` — the realm graph: overworld = one stitched realm, interiors = small
  realms, warps = edges; collision AND elevation from pret's mapgrid words, plus
  the behavior tables (surfable water, arrow-warp directions, animated doors).
  Whole world resident.
- `player.rs` — tile-by-tile movement ported from pret's rules: 16/8/4 frames
  per tile (walk / run+surf / mach top tier), instant turns in motion with
  gapless tile chaining, turn-in-place only from a standstill. Elevation is the
  water/cliff blocker: walkers are 3, water is 1; **transition tiles (0) assign
  0 to the player** — from stairs you can step to any elevation (pret's
  ObjectEventUpdateElevation) — and bridges (15) never block or assign. Avatar
  states: Walking, Surfing (A at a surfable edge; auto-dismount ashore),
  Bike (Select; tier climbs per pedaled tile, coasts down a tier per tile on
  release). Warps come in kinds: step-on (stairs, holes), walk-into animated
  doors (the open/walk-in choreography), and **arrow warps** (interior exits:
  fire only on pressing their direction while standing on the tile).
- `bake.rs` — startup baking of indexed tiles + palettes → deduped RGBA atlases.
- `data.rs` — the world-pack format, defined once and shared with `worldgen`
  (extractor writes, engine reads; no spec to drift).

`cargo test -p game` covers the guarantees the README promises (purity, determinism,
interpolation, culling, zoom contract, warp snapping, save/load roundtrip).
