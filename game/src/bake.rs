//! Startup baking: indexed pret graphics → RGBA atlases, at atlas-upload time
//! (README "Assets"). For every (tileset pair, metatile) the two 16x16 layer
//! images are rendered from 4bpp tiles + palettes, deduplicated by content
//! (metatiles repeat heavily across pairs), and packed into 2048x2048 RGBA
//! atlases. The backend uploads those and never learns what a palette is.
//!
//! Palette-cycle animation and day/night will be done by emitting different
//! `src` regions / `tint` later — the backend stays fixed.

use crate::data::{TilesetData, WorldPack, METATILES_IN_PRIMARY, TILES_IN_PRIMARY};
use std::collections::HashMap;
use types::Rect;

pub const ATLAS_SIZE: u32 = 2048; // WebGL2-safe
const CELL: u32 = 16;
const PER_ROW: u32 = ATLAS_SIZE / CELL;
const PER_ATLAS: u32 = PER_ROW * PER_ROW;

pub struct AtlasImage {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

/// Reference to one baked 16x16 cell; `NONE` = fully transparent (skip).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRef(u32);

impl CellRef {
    pub const NONE: CellRef = CellRef(u32::MAX);

    pub fn atlas(self) -> usize {
        (self.0 / PER_ATLAS) as usize
    }

    pub fn src(self) -> Rect {
        let i = self.0 % PER_ATLAS;
        Rect::new(((i % PER_ROW) * CELL) as f32, ((i / PER_ROW) * CELL) as f32, 16.0, 16.0)
    }
}

/// How to draw one metatile: bottom layer, top layer, and whether the top
/// layer covers the player (pret layer types: normal/split tops draw above
/// sprites, "covered" tops below).
#[derive(Clone, Copy, Debug)]
pub struct MetatileDraw {
    pub bottom: CellRef,
    pub top: CellRef,
    pub top_above_player: bool,
}

const EMPTY_DRAW: MetatileDraw =
    MetatileDraw { bottom: CellRef::NONE, top: CellRef::NONE, top_above_player: false };

pub struct Baked {
    pub atlases: Vec<AtlasImage>,
    /// Indexed by [pack pair index][metatile id].
    tables: Vec<Vec<MetatileDraw>>,
}

impl Baked {
    pub fn draw(&self, pair: u16, metatile: u16) -> MetatileDraw {
        self.tables
            .get(pair as usize)
            .and_then(|t| t.get(metatile as usize))
            .copied()
            .unwrap_or(EMPTY_DRAW)
    }
}

pub fn bake(pack: &WorldPack) -> Baked {
    let mut cells: Vec<u8> = Vec::new(); // deduped 16x16 RGBA cells, flat
    let mut dedupe: HashMap<Vec<u8>, CellRef> = HashMap::new();

    let mut intern = |img: [u8; 16 * 16 * 4]| -> CellRef {
        if img.iter().skip(3).step_by(4).all(|&a| a == 0) {
            return CellRef::NONE;
        }
        let key = img.to_vec();
        *dedupe.entry(key).or_insert_with(|| {
            cells.extend_from_slice(&img);
            CellRef((cells.len() / (16 * 16 * 4)) as u32 - 1)
        })
    };

    let mut tables = Vec::with_capacity(pack.pairs.len());
    for &(pi, si) in &pack.pairs {
        let prim = &pack.tilesets[pi as usize];
        let sec = &pack.tilesets[si as usize];
        // merged GBA palette slots: primary owns 0-5 (and the unused 12-15),
        // secondary owns 6-11 — same layout the GBA loads into VRAM
        let mut pal = [[0u8; 4]; 256];
        pal[..6 * 16].copy_from_slice(&prim.palettes[..6 * 16]);
        pal[6 * 16..12 * 16].copy_from_slice(&sec.palettes[6 * 16..12 * 16]);
        pal[12 * 16..].copy_from_slice(&prim.palettes[12 * 16..]);

        let n_sec = (sec.metatiles.len() / 8) as u16;
        let count = (METATILES_IN_PRIMARY + n_sec) as usize;
        let mut table = vec![EMPTY_DRAW; count];
        for (id, slot) in table.iter_mut().enumerate() {
            let id = id as u16;
            let (ts, local) = if id < METATILES_IN_PRIMARY {
                (prim, id)
            } else {
                (sec, id - METATILES_IN_PRIMARY)
            };
            let Some(entries) = ts.metatiles.get(local as usize * 8..local as usize * 8 + 8)
            else {
                continue;
            };
            let bottom = intern(render_layer(&entries[..4], prim, sec, &pal));
            let top = intern(render_layer(&entries[4..], prim, sec, &pal));
            let attr = ts.attributes.get(local as usize).copied().unwrap_or(0);
            let layer_type = (attr >> 12) & 0xF;
            *slot = MetatileDraw { bottom, top, top_above_player: layer_type != 1 };
        }
        tables.push(table);
    }

    // pack the deduped cells into atlases
    let n_cells = (cells.len() / (16 * 16 * 4)) as u32;
    let mut atlases = Vec::new();
    for a in 0..n_cells.div_ceil(PER_ATLAS) {
        let mut rgba = vec![0u8; (ATLAS_SIZE * ATLAS_SIZE * 4) as usize];
        for i in 0..PER_ATLAS.min(n_cells - a * PER_ATLAS) {
            let cell = &cells[((a * PER_ATLAS + i) * 16 * 16 * 4) as usize..][..16 * 16 * 4];
            let (cx, cy) = ((i % PER_ROW) * CELL, (i / PER_ROW) * CELL);
            for y in 0..16 {
                let dst = (((cy + y) * ATLAS_SIZE + cx) * 4) as usize;
                let src = (y * 16 * 4) as usize;
                rgba[dst..dst + 64].copy_from_slice(&cell[src..src + 64]);
            }
        }
        atlases.push(AtlasImage { rgba, w: ATLAS_SIZE, h: ATLAS_SIZE });
    }

    Baked { atlases, tables }
}

/// Render one metatile layer (4 tile entries, quadrants TL TR BL BR) to RGBA.
fn render_layer(
    entries: &[u16],
    prim: &TilesetData,
    sec: &TilesetData,
    pal: &[[u8; 4]; 256],
) -> [u8; 16 * 16 * 4] {
    let mut out = [0u8; 16 * 16 * 4];
    for (q, &e) in entries.iter().enumerate() {
        let tile_id = e & 0x3FF;
        let (ts, local) = if tile_id < TILES_IN_PRIMARY {
            (prim, tile_id)
        } else {
            (sec, tile_id - TILES_IN_PRIMARY)
        };
        let Some(tile) = ts.tiles_4bpp.get(local as usize * 32..local as usize * 32 + 32)
        else {
            continue; // out-of-range tile → transparent
        };
        let hflip = e & 0x400 != 0;
        let vflip = e & 0x800 != 0;
        let pal_row = ((e >> 12) & 0xF) as usize * 16;
        let (qx, qy) = ((q % 2) * 8, (q / 2) * 8);
        for y in 0..8usize {
            for x in 0..8usize {
                let sx = if hflip { 7 - x } else { x };
                let sy = if vflip { 7 - y } else { y };
                let b = tile[sy * 4 + sx / 2];
                let c = if sx % 2 == 0 { b & 0xF } else { b >> 4 } as usize;
                if c == 0 {
                    continue; // transparent
                }
                let at = ((qy + y) * 16 + qx + x) * 4;
                out[at..at + 4].copy_from_slice(&pal[pal_row + c]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cellref_src_walks_the_atlas_grid() {
        assert_eq!(CellRef(0).src(), Rect::new(0.0, 0.0, 16.0, 16.0));
        assert_eq!(CellRef(0).atlas(), 0);
        assert_eq!(CellRef(PER_ROW).src(), Rect::new(0.0, 16.0, 16.0, 16.0));
        assert_eq!(CellRef(PER_ATLAS + 1).atlas(), 1);
        assert_eq!(CellRef(PER_ATLAS + 1).src(), Rect::new(16.0, 0.0, 16.0, 16.0));
    }
}
