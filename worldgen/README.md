# worldgen — pret → world.bin

Build-time extractor: reads the pret/pokeemerald clone (never the ROM, never the C
project's legacy loaders) and writes `assets/world.bin` in the format defined once in
[`game/src/data.rs`](../game/src/data.rs).

    cargo run -p worldgen --release   # [pret-path] [output], defaults ~/pokeemerald

What it extracts: tileset structs (headers.h → real files), indexed 4bpp tiles +
JASC palettes, metatiles + attributes, all 440 layouts with pret's mapgrid words
intact (metatile + collision + the elevation nibble), and the realm graph — the
overworld stitched by BFS over map connections into the same 800×383 space as
pokeemerald_SDL3 (its own connection inconsistencies resolved
first-placement-wins, like the C), warps resolved to realm edges. Plus the player
sheets (walking + running stacked; surfing and mach_bike whose poses come in
PAIRS — sitting frames 0/2/4; the surf blob baked under the player's palette,
exactly what `paletteNum = 0` does on hardware), Emerald's latin_normal font
(charmap.txt + the fonts.c width table + a solid cell for UI fills), all 51
animated doors from field_door.c (3 open frames each, per-tile palette slots),
and two behavior tables parsed from pret's OWN sources rather than hardcoded:
the surfable mask (sTileBitAttributes) and the arrow-warp trigger directions
(MB_*_ARROW_WARP) — if pret changes, re-running the extractor tracks it.

`--bin framedump` software-renders a frame of the real world to PNG — the visual
check that needs no display.

Dependencies (`png`, `serde_json`) are tool-only and never linked into the game.
