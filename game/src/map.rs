//! World model: a graph of realms (README "World model"). A realm is one
//! self-contained coordinate space. The overworld is ONE large realm — the
//! stitched coordinate space carried over from pokeemerald_SDL3, where each
//! constituent map owns a rectangle and a lookup detects map entry (music
//! changes). Each interior is its own small realm. Warps are edges: a warp
//! switches the active realm and places the player at the target point.
//!
//! The whole world is resident: `RealmGraph::from_pack` expands every realm's
//! cells up front (no streaming — README sizes the world at a few MB).
//! Rendering still culls to the camera-visible rect each frame.

use crate::data::{LayoutData, RealmData, WorldPack};

/// One metatile = 16x16 px, the unit of the grid and of player movement.
pub const METATILE: f32 = 16.0;

/// Index into [`RealmGraph::realms`].
pub type RealmId = u16;
/// Index into [`Realm::maps`].
pub type MapIdx = u16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MapInfo {
    pub id: String,
    pub music: String,
    /// Position of this map's top-left in the realm's metatile coords.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A warp edge out of a realm: standing on (x, y) moves the player to
/// (to_x, to_y) in `to_realm`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Warp {
    pub x: i32,
    pub y: i32,
    pub to_realm: RealmId,
    pub to_x: i32,
    pub to_y: i32,
}

/// One grid cell: which tileset pair renders it, which metatile, and pret's
/// mapgrid collision + elevation bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub pair: u16,
    pub metatile: u16,
    pub collision: u8,
    /// pret's elevation nibble: 0 = transition (never blocks), 15 = bridges
    /// (never block), otherwise must match the walker's elevation. Water is
    /// 1, plain ground 3 — this is what keeps walkers off water.
    pub elevation: u8,
}

// packed cell word:
// metatile 0..10 | collision 10..12 | pair 12..20 | present 20 | elevation 21..25
const PRESENT: u32 = 1 << 20;

pub struct Realm {
    pub width: i32,
    pub height: i32,
    pub maps: Vec<MapInfo>,
    pub warps: Vec<Warp>,
    cells: Vec<u32>,
}

impl Realm {
    /// Expand a pack realm: stitch every member map's layout blockdata into
    /// this realm's single grid (the C's overworld_init, minus the pixels).
    pub fn from_data(rd: &RealmData, layouts: &[LayoutData]) -> Self {
        let (width, height) = (rd.width as i32, rd.height as i32);
        let mut cells = vec![0u32; (width * height) as usize];
        let mut maps = Vec::new();
        let mut warps = Vec::new();
        for m in &rd.maps {
            let l = &layouts[m.layout as usize];
            for y in 0..l.height as i32 {
                for x in 0..l.width as i32 {
                    let word = l.blockdata[(y * l.width as i32 + x) as usize] as u32;
                    let (metatile, collision) = (word & 0x3FF, (word >> 10) & 0x3);
                    let elevation = (word >> 12) & 0xF;
                    cells[((m.y + y) * width + m.x + x) as usize] = PRESENT
                        | elevation << 21
                        | (l.pair as u32) << 12
                        | collision << 10
                        | metatile;
                }
            }
            maps.push(MapInfo {
                id: m.id.clone(),
                music: m.music.clone(),
                x: m.x,
                y: m.y,
                width: l.width as i32,
                height: l.height as i32,
            });
            warps.extend(m.warps.iter().map(|w| Warp {
                x: w.x,
                y: w.y,
                to_realm: w.to_realm,
                to_x: w.to_x,
                to_y: w.to_y,
            }));
        }
        Self { width, height, maps, warps, cells }
    }

    pub fn cell(&self, x: i32, y: i32) -> Option<Cell> {
        if !self.in_bounds(x, y) {
            return None;
        }
        let w = self.cells[(y * self.width + x) as usize];
        (w & PRESENT != 0).then_some(Cell {
            pair: (w >> 12 & 0xFF) as u16,
            metatile: (w & 0x3FF) as u16,
            collision: (w >> 10 & 0x3) as u8,
            elevation: (w >> 21 & 0xF) as u8,
        })
    }

    /// pret's mapgrid collision bits decide passability. Tiles outside any
    /// map (gaps in the stitched overworld) stay traversable void, matching
    /// the C project's behavior.
    pub fn passable(&self, x: i32, y: i32) -> bool {
        self.cell(x, y).is_none_or(|c| c.collision == 0)
    }

    /// Which map owns this tile — the C project's `tile_map` lookup.
    pub fn map_at(&self, x: i32, y: i32) -> Option<MapIdx> {
        self.maps
            .iter()
            .position(|m| {
                (m.x..m.x + m.width).contains(&x) && (m.y..m.y + m.height).contains(&y)
            })
            .map(|i| i as MapIdx)
    }

    pub fn warp_at(&self, x: i32, y: i32) -> Option<Warp> {
        self.warps.iter().copied().find(|w| w.x == x && w.y == y)
    }

    pub fn in_bounds(&self, x: i32, y: i32) -> bool {
        (0..self.width).contains(&x) && (0..self.height).contains(&y)
    }
}

/// The whole world, resident in RAM. Realm 0 is the overworld. Also carries
/// the metatile-behavior tables (per tileset), pret's surfable-behavior mask,
/// and the animated-door index — the world model the sim consults.
pub struct RealmGraph {
    pub realms: Vec<Realm>,
    /// (primary, secondary) tileset per pair index.
    pairs: Vec<(u16, u16)>,
    /// Behavior byte per metatile, per tileset.
    behaviors: Vec<Vec<u8>>,
    /// Bit b of byte b/8: behavior b is surfable water.
    surfable: [u8; 32],
    /// Per-behavior warp trigger (0 step-on, 1..4 = S/N/E/W arrow).
    warp_dir: [u8; 256],
    /// (tileset, metatile) → index into the pack's door table.
    doors: std::collections::HashMap<(u16, u16), u16>,
}

pub const OVERWORLD: RealmId = 0;

impl RealmGraph {
    pub fn realm(&self, id: RealmId) -> &Realm {
        &self.realms[id as usize]
    }

    pub fn from_pack(pack: &WorldPack) -> Self {
        Self {
            realms: pack.realms.iter().map(|r| Realm::from_data(r, &pack.layouts)).collect(),
            pairs: pack.pairs.clone(),
            behaviors: pack
                .tilesets
                .iter()
                .map(|t| t.attributes.iter().map(|&a| (a & 0xFF) as u8).collect())
                .collect(),
            surfable: pack.surfable,
            warp_dir: pack.warp_dir,
            doors: pack
                .doors
                .iter()
                .enumerate()
                .map(|(i, d)| ((d.tileset, d.metatile), i as u16))
                .collect(),
        }
    }

    /// The metatile behavior at a cell (pret's attribute low byte).
    pub fn behavior(&self, cell: Cell) -> u8 {
        let Some(&(prim, sec)) = self.pairs.get(cell.pair as usize) else { return 0 };
        let (ts, local) = if cell.metatile < 512 {
            (prim, cell.metatile)
        } else {
            (sec, cell.metatile - 512)
        };
        self.behaviors
            .get(ts as usize)
            .and_then(|b| b.get(local as usize))
            .copied()
            .unwrap_or(0)
    }

    /// Is this cell surfable water (pret's TILE_FLAG_SURFABLE)?
    pub fn surfable(&self, cell: Cell) -> bool {
        let b = self.behavior(cell);
        self.surfable[(b / 8) as usize] & (1 << (b % 8)) != 0
    }

    /// Arrow-warp direction of the tile's behavior (None = step-on warp).
    pub fn arrow_dir_at(&self, realm: RealmId, x: i32, y: i32) -> Option<u8> {
        let cell = self.realm(realm).cell(x, y)?;
        match self.warp_dir[self.behavior(cell) as usize] {
            0 => None,
            d => Some(d),
        }
    }

    pub fn surfable_at(&self, realm: RealmId, x: i32, y: i32) -> bool {
        self.realm(realm).cell(x, y).is_some_and(|c| self.surfable(c))
    }

    /// The animated door at a cell, if any (index into the pack's doors).
    pub fn door_at(&self, realm: RealmId, x: i32, y: i32) -> Option<u16> {
        let cell = self.realm(realm).cell(x, y)?;
        let &(prim, sec) = self.pairs.get(cell.pair as usize)?;
        let ts = if cell.metatile < 512 { prim } else { sec };
        self.doors.get(&(ts, cell.metatile)).copied()
    }

    /// Small hand-built world for logic tests: an overworld realm of two
    /// adjacent maps, a door warp at (5, 5) into a house realm with an exit
    /// mat at (4, 7), and a solid boulder at (20, 10).
    pub fn synthetic() -> Self {
        let mk = |width: i32, height: i32, maps: Vec<MapInfo>, warps, solid: &[(i32, i32)]| {
            let mut cells = Vec::new();
            for y in 0..height {
                for x in 0..width {
                    let collision = solid.contains(&(x, y)) as u32;
                    cells.push(PRESENT | collision << 10);
                }
            }
            Realm { width, height, maps, warps, cells }
        };
        let overworld = mk(
            60,
            40,
            vec![
                MapInfo {
                    id: "SYNTH_WEST".into(),
                    music: "MUS_TEST_W".into(),
                    x: 0,
                    y: 0,
                    width: 30,
                    height: 40,
                },
                MapInfo {
                    id: "SYNTH_EAST".into(),
                    music: "MUS_TEST_E".into(),
                    x: 30,
                    y: 0,
                    width: 30,
                    height: 40,
                },
            ],
            vec![Warp { x: 5, y: 5, to_realm: 1, to_x: 4, to_y: 6 }],
            &[(20, 10)],
        );
        let house = mk(
            9,
            8,
            vec![MapInfo {
                id: "SYNTH_HOUSE".into(),
                music: "MUS_TEST_W".into(),
                x: 0,
                y: 0,
                width: 9,
                height: 8,
            }],
            vec![Warp { x: 4, y: 7, to_realm: OVERWORLD, to_x: 5, to_y: 6 }],
            &[],
        );
        Self {
            realms: vec![overworld, house],
            pairs: vec![(0, 0)],
            behaviors: vec![vec![0; 1024]],
            surfable: [0; 32],
            warp_dir: [0; 256],
            doors: std::collections::HashMap::new(),
        }
    }
}
