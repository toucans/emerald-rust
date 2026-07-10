//! The world pack: the one data format between the extractor (`worldgen`) and
//! the engine. Defined once, here, in Rust — the extractor writes it, the game
//! reads it, there is no separate format spec to drift (the same
//! single-source-of-truth rule docs/audio-engine.md sets for music data).
//!
//! The pack carries *raw indexed* graphics (GBA 4bpp tiles + palettes), not
//! baked RGBA: baking happens game-side at atlas-upload time (see `bake.rs`),
//! so the pack stays small and the backend never learns what a palette is.
//!
//! Format: `EMWORLD4` magic, little-endian, length-prefixed vectors/strings.
//! Regenerate with: `cargo run -p worldgen -- <path-to-pret-clone>`.

pub const MAGIC: &[u8; 8] = b"EMWORLD4";

/// Metatile ids 0..512 index the primary tileset, 512.. the secondary.
pub const METATILES_IN_PRIMARY: u16 = 512;
/// Tile ids 0..512 index primary tiles, 512.. secondary tiles.
pub const TILES_IN_PRIMARY: u16 = 512;

#[derive(Debug, PartialEq)]
pub struct WorldPack {
    pub tilesets: Vec<TilesetData>,
    /// Unique (primary, secondary) tileset pairings; layouts reference these.
    pub pairs: Vec<(u16, u16)>,
    pub layouts: Vec<LayoutData>,
    pub realms: Vec<RealmData>,
    /// Player sprite sheet (RGBA8), 144x64: row 0 = pret's walking.png
    /// (stand down/up/side, walk down x2, up x2, side x2), row 1 = running.png
    /// in the same frame layout.
    pub player: Image,
    /// Emerald's latin_normal font: one 16x16 RGBA cell per printable ASCII
    /// char (32..=126), laid out horizontally, mapped through charmap.txt.
    pub font: Image,
    /// Advance width per glyph (same indexing as `font`).
    pub font_widths: Vec<u8>,
    /// Brendan surfing (192x32: 6 frames of 32x32; 0=S 1=N 2=W, E flipped).
    pub surf_player: Image,
    /// The surf blob (96x32: 3 frames of 32x32; S, N, W).
    pub surf_blob: Image,
    /// Brendan on the Mach bike (288x32: 9 frames of 32x32, walking layout).
    pub bike: Image,
    /// Door open animations from pret's field_door.c: 3 frames of 16x32 each.
    pub doors: Vec<DoorData>,
    /// Bitmask over metatile behaviors: TILE_FLAG_SURFABLE, straight from
    /// pret's sTileBitAttributes (bit b of byte b/8 = behavior b surfable).
    pub surfable: [u8; 32],
    /// Per-behavior warp trigger: 0 = step-on, 1..4 = arrow warp that fires
    /// only when pressing South/North/East/West while standing on it.
    pub warp_dir: [u8; 256],
    pub start: Start,
}

/// One animated door: matched by (tileset, absolute metatile id).
#[derive(Debug, PartialEq)]
pub struct DoorData {
    pub tileset: u16,
    pub metatile: u16,
    /// 16x96 RGBA: three 16x32 open-animation frames, top to bottom.
    pub frames: Image,
}

#[derive(Debug, PartialEq)]
pub struct TilesetData {
    pub name: String,
    pub is_secondary: bool,
    /// GBA 4bpp tiles: 32 bytes per 8x8 tile, low nibble = left pixel.
    pub tiles_4bpp: Vec<u8>,
    /// 16 palettes x 16 colors, RGBA8. Color 0 is transparent. Slots 0-5 are
    /// meaningful for primary tilesets, 6-11 for secondary (GBA VRAM layout).
    pub palettes: [[u8; 4]; 256],
    /// 8 u16 entries per metatile: tile id | hflip<<10 | vflip<<11 | pal<<12.
    /// First 4 = bottom layer quadrants, last 4 = top layer.
    pub metatiles: Vec<u16>,
    /// One u16 per metatile: behavior 0..8 bits, layer type bits 12..16
    /// (0 normal: top above sprites; 1 covered: top below; 2 split: above).
    pub attributes: Vec<u16>,
}

#[derive(Debug, PartialEq)]
pub struct LayoutData {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub pair: u16,
    /// One u16 per cell: metatile id bits 0..10, collision 10..12, elevation
    /// 12..16 — pret's mapgrid word, unchanged.
    pub blockdata: Vec<u16>,
}

/// A realm ready to expand: realm 0 is the stitched overworld (many maps),
/// the rest are single-map realms (interiors and disconnected outdoors).
#[derive(Debug, PartialEq)]
pub struct RealmData {
    pub width: u32,
    pub height: u32,
    pub maps: Vec<MapData>,
}

#[derive(Debug, PartialEq)]
pub struct MapData {
    pub id: String,
    pub music: String,
    pub layout: u16,
    /// Map origin in realm metatile coords (0,0 for single-map realms).
    pub x: i32,
    pub y: i32,
    pub warps: Vec<WarpData>,
}

/// Realm-local warp edge (coords already include the map origin).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WarpData {
    pub x: i32,
    pub y: i32,
    pub to_realm: u16,
    pub to_x: i32,
    pub to_y: i32,
}

#[derive(Debug, PartialEq)]
pub struct Image {
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Start {
    pub realm: u16,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug)]
pub struct PackError(pub &'static str);

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "world pack: {}", self.0)
    }
}
impl std::error::Error for PackError {}

// ── writing ──────────────────────────────────────────────────────────────

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
    }
    fn str(&mut self, v: &str) {
        self.bytes(v.as_bytes());
    }
    fn u16s(&mut self, v: &[u16]) {
        self.u32(v.len() as u32);
        for &x in v {
            self.u16(x);
        }
    }
}

impl WorldPack {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.extend_from_slice(MAGIC);

        w.u32(self.tilesets.len() as u32);
        for t in &self.tilesets {
            w.str(&t.name);
            w.u8(t.is_secondary as u8);
            w.bytes(&t.tiles_4bpp);
            for c in &t.palettes {
                w.0.extend_from_slice(c);
            }
            w.u16s(&t.metatiles);
            w.u16s(&t.attributes);
        }

        w.u32(self.pairs.len() as u32);
        for &(p, s) in &self.pairs {
            w.u16(p);
            w.u16(s);
        }

        w.u32(self.layouts.len() as u32);
        for l in &self.layouts {
            w.str(&l.id);
            w.u32(l.width);
            w.u32(l.height);
            w.u16(l.pair);
            w.u16s(&l.blockdata);
        }

        w.u32(self.realms.len() as u32);
        for r in &self.realms {
            w.u32(r.width);
            w.u32(r.height);
            w.u32(r.maps.len() as u32);
            for m in &r.maps {
                w.str(&m.id);
                w.str(&m.music);
                w.u16(m.layout);
                w.i32(m.x);
                w.i32(m.y);
                w.u32(m.warps.len() as u32);
                for wp in &m.warps {
                    w.i32(wp.x);
                    w.i32(wp.y);
                    w.u16(wp.to_realm);
                    w.i32(wp.to_x);
                    w.i32(wp.to_y);
                }
            }
        }

        w.u32(self.player.w);
        w.u32(self.player.h);
        w.bytes(&self.player.rgba);

        w.u32(self.font.w);
        w.u32(self.font.h);
        w.bytes(&self.font.rgba);
        w.bytes(&self.font_widths);

        for img in [&self.surf_player, &self.surf_blob, &self.bike] {
            w.u32(img.w);
            w.u32(img.h);
            w.bytes(&img.rgba);
        }
        w.u32(self.doors.len() as u32);
        for d in &self.doors {
            w.u16(d.tileset);
            w.u16(d.metatile);
            w.u32(d.frames.w);
            w.u32(d.frames.h);
            w.bytes(&d.frames.rgba);
        }
        w.0.extend_from_slice(&self.surfable);
        w.0.extend_from_slice(&self.warp_dir);

        w.u16(self.start.realm);
        w.i32(self.start.x);
        w.i32(self.start.y);

        w.0
    }
}

// ── reading ──────────────────────────────────────────────────────────────

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PackError> {
        let s = self.b.get(self.p..self.p + n).ok_or(PackError("truncated"))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, PackError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, PackError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, PackError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, PackError> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn len(&mut self) -> Result<usize, PackError> {
        let n = self.u32()? as usize;
        if n > self.b.len() - self.p {
            return Err(PackError("length exceeds data"));
        }
        Ok(n)
    }
    fn bytes(&mut self) -> Result<Vec<u8>, PackError> {
        let n = self.len()?;
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> Result<String, PackError> {
        String::from_utf8(self.bytes()?).map_err(|_| PackError("bad utf8"))
    }
    fn u16s(&mut self) -> Result<Vec<u16>, PackError> {
        let n = self.len()?;
        let s = self.take(n * 2)?;
        Ok(s.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }
}

impl WorldPack {
    pub fn from_bytes(b: &[u8]) -> Result<WorldPack, PackError> {
        let mut r = R { b, p: 0 };
        if r.take(8)? != MAGIC {
            return Err(PackError("bad magic (regenerate with worldgen)"));
        }

        let mut tilesets = Vec::new();
        for _ in 0..r.u32()? {
            let name = r.str()?;
            let is_secondary = r.u8()? != 0;
            let tiles_4bpp = r.bytes()?;
            let mut palettes = [[0u8; 4]; 256];
            for c in &mut palettes {
                c.copy_from_slice(r.take(4)?);
            }
            let metatiles = r.u16s()?;
            let attributes = r.u16s()?;
            tilesets.push(TilesetData {
                name,
                is_secondary,
                tiles_4bpp,
                palettes,
                metatiles,
                attributes,
            });
        }

        let mut pairs = Vec::new();
        for _ in 0..r.u32()? {
            pairs.push((r.u16()?, r.u16()?));
        }

        let mut layouts = Vec::new();
        for _ in 0..r.u32()? {
            layouts.push(LayoutData {
                id: r.str()?,
                width: r.u32()?,
                height: r.u32()?,
                pair: r.u16()?,
                blockdata: r.u16s()?,
            });
        }

        let mut realms = Vec::new();
        for _ in 0..r.u32()? {
            let width = r.u32()?;
            let height = r.u32()?;
            let mut maps = Vec::new();
            for _ in 0..r.u32()? {
                let id = r.str()?;
                let music = r.str()?;
                let layout = r.u16()?;
                let x = r.i32()?;
                let y = r.i32()?;
                let mut warps = Vec::new();
                for _ in 0..r.u32()? {
                    warps.push(WarpData {
                        x: r.i32()?,
                        y: r.i32()?,
                        to_realm: r.u16()?,
                        to_x: r.i32()?,
                        to_y: r.i32()?,
                    });
                }
                maps.push(MapData { id, music, layout, x, y, warps });
            }
            realms.push(RealmData { width, height, maps });
        }

        let w = r.u32()?;
        let h = r.u32()?;
        let rgba = r.bytes()?;
        if rgba.len() != (w * h * 4) as usize {
            return Err(PackError("player image size mismatch"));
        }
        let player = Image { rgba, w, h };

        let w = r.u32()?;
        let h = r.u32()?;
        let rgba = r.bytes()?;
        if rgba.len() != (w * h * 4) as usize {
            return Err(PackError("font image size mismatch"));
        }
        let font = Image { rgba, w, h };
        let font_widths = r.bytes()?;

        let mut img = || -> Result<Image, PackError> {
            let w = r.u32()?;
            let h = r.u32()?;
            let rgba = r.bytes()?;
            if rgba.len() != (w * h * 4) as usize {
                return Err(PackError("image size mismatch"));
            }
            Ok(Image { rgba, w, h })
        };
        let surf_player = img()?;
        let surf_blob = img()?;
        let bike = img()?;
        let mut doors = Vec::new();
        for _ in 0..r.u32()? {
            let tileset = r.u16()?;
            let metatile = r.u16()?;
            let w = r.u32()?;
            let h = r.u32()?;
            let rgba = r.bytes()?;
            if rgba.len() != (w * h * 4) as usize {
                return Err(PackError("door image size mismatch"));
            }
            doors.push(DoorData { tileset, metatile, frames: Image { rgba, w, h } });
        }
        let surfable: [u8; 32] = r.take(32)?.try_into().unwrap();
        let warp_dir: [u8; 256] = r.take(256)?.try_into().unwrap();

        let start = Start { realm: r.u16()?, x: r.i32()?, y: r.i32()? };

        Ok(WorldPack {
            tilesets,
            pairs,
            layouts,
            realms,
            player,
            font,
            font_widths,
            surf_player,
            surf_blob,
            bike,
            doors,
            surfable,
            warp_dir,
            start,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pack = WorldPack {
            tilesets: vec![TilesetData {
                name: "gTileset_Test".into(),
                is_secondary: false,
                tiles_4bpp: vec![0xAB; 64],
                palettes: [[1, 2, 3, 255]; 256],
                metatiles: vec![0x1234; 16],
                attributes: vec![0x1000, 0],
            }],
            pairs: vec![(0, 0)],
            layouts: vec![LayoutData {
                id: "LAYOUT_TEST".into(),
                width: 2,
                height: 1,
                pair: 0,
                blockdata: vec![1, 0x0401],
            }],
            realms: vec![RealmData {
                width: 2,
                height: 1,
                maps: vec![MapData {
                    id: "MAP_TEST".into(),
                    music: "MUS_TEST".into(),
                    layout: 0,
                    x: 0,
                    y: 0,
                    warps: vec![WarpData { x: 1, y: 0, to_realm: 0, to_x: 0, to_y: 0 }],
                }],
            }],
            player: Image { rgba: vec![7; 16 * 32 * 4], w: 16, h: 32 },
            font: Image { rgba: vec![9; 16 * 16 * 4], w: 16, h: 16 },
            font_widths: vec![6],
            surf_player: Image { rgba: vec![1; 192 * 32 * 4], w: 192, h: 32 },
            surf_blob: Image { rgba: vec![2; 96 * 32 * 4], w: 96, h: 32 },
            bike: Image { rgba: vec![3; 288 * 32 * 4], w: 288, h: 32 },
            doors: vec![DoorData {
                tileset: 0,
                metatile: 1,
                frames: Image { rgba: vec![4; 16 * 96 * 4], w: 16, h: 96 },
            }],
            surfable: [0xAA; 32],
            warp_dir: [0; 256],
            start: Start { realm: 0, x: 1, y: 0 },
        };
        let bytes = pack.to_bytes();
        assert_eq!(WorldPack::from_bytes(&bytes).unwrap(), pack);
        assert!(WorldPack::from_bytes(&bytes[..bytes.len() - 3]).is_err());
        assert!(WorldPack::from_bytes(b"NOTMAGIC").is_err());
    }
}
