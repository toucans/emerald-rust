//! worldgen — from-scratch pret → world.bin extraction.
//!
//! Reads a pret/pokeemerald source clone (never the ROM, never the C
//! project's loaders) and writes the WorldPack: raw indexed tilesets, layouts
//! with pret's mapgrid words intact, and the realm graph — the overworld
//! stitched into one coordinate space by BFS over map connections (carrying
//! over pokeemerald_SDL3's model), every other map its own realm, warps
//! resolved to realm edges.
//!
//! Usage: cargo run -p worldgen -- [path-to-pret-clone] [output]
//! Defaults: ~/pokeemerald  assets/world.bin

use game::data::{
    DoorData, Image, LayoutData, MapData, RealmData, Start, TilesetData, WarpData, WorldPack,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args().skip(1);
    let pret = PathBuf::from(
        args.next().unwrap_or_else(|| format!("{}/pokeemerald", std::env::var("HOME").unwrap())),
    );
    let out = PathBuf::from(args.next().unwrap_or_else(|| "assets/world.bin".into()));

    let pack = extract(&pret);
    let bytes = pack.to_bytes();
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    std::fs::write(&out, &bytes).unwrap();
    println!(
        "wrote {} ({:.1} MB): {} tilesets, {} pairs, {} layouts, {} realms",
        out.display(),
        bytes.len() as f64 / 1e6,
        pack.tilesets.len(),
        pack.pairs.len(),
        pack.layouts.len(),
        pack.realms.len()
    );
}

fn extract(pret: &Path) -> WorldPack {
    // ── tilesets: parse the Tileset structs in src/data/tilesets/headers.h,
    // then resolve each symbol to its file via graphics.h/graphics.c
    // (tiles + palettes) and metatiles.h (metatiles + attributes) ─────────
    let headers_h = read(pret, "src/data/tilesets/headers.h");
    let gfx_src =
        read(pret, "src/data/tilesets/graphics.h") + &read2(pret, "src/graphics.c");
    let metatiles_h = read(pret, "src/data/tilesets/metatiles.h");

    let mut ts_names: Vec<String> = Vec::new();
    let mut ts_defs: HashMap<String, (bool, String, String, String, String)> = HashMap::new();
    for chunk in headers_h.split("const struct Tileset ").skip(1) {
        let Some(name) = chunk.split_whitespace().next() else { continue };
        if !name.starts_with("gTileset_") {
            continue;
        }
        let body = chunk.split_once("};").map(|(b, _)| b).unwrap_or(chunk);
        let field = |f: &str| -> Option<String> {
            let after = body.split_once(&format!(".{f} = "))?.1;
            Some(after.split([',', '\n']).next()?.trim().to_string())
        };
        let is_secondary = field("isSecondary").as_deref() == Some("TRUE");
        let (Some(tiles), Some(pals), Some(mts), Some(attrs)) = (
            field("tiles"),
            field("palettes"),
            field("metatiles"),
            field("metatileAttributes"),
        ) else {
            continue;
        };
        ts_names.push(name.to_string());
        ts_defs.insert(name.to_string(), (is_secondary, tiles, pals, mts, attrs));
    }
    assert!(!ts_names.is_empty(), "no tilesets found in headers.h");

    // path of the INC* binding for `sym` in `hay`: `sym[]... ("<path>"`
    let bound_path = |hay: &str, sym: &str| -> String {
        let at = hay
            .find(&format!(" {sym}["))
            .unwrap_or_else(|| panic!("no binding for {sym}"));
        let rest = &hay[at..];
        let q = rest.find('(').unwrap();
        rest[q..].split('"').nth(1).unwrap_or_else(|| panic!("no path for {sym}")).to_string()
    };

    let ts_index: HashMap<String, u16> =
        ts_names.iter().enumerate().map(|(i, n)| (n.clone(), i as u16)).collect();

    let tilesets: Vec<TilesetData> = ts_names
        .iter()
        .map(|name| {
            let (is_secondary, tiles_sym, pal_sym, mt_sym, attr_sym) = &ts_defs[name];
            let (idx, w, _h) = decode_indexed_png(&pret.join(bound_path(&gfx_src, tiles_sym)));
            let tiles_4bpp = to_gba_4bpp(&idx, w);
            // palettes: 16 INCGFX_U16 entries; the first gives the directory
            let pal_dir = pret.join(bound_path(&gfx_src, pal_sym));
            let pal_dir = pal_dir.parent().unwrap();
            let mut palettes = [[0u8; 4]; 256];
            for p in 0..16 {
                let f = pal_dir.join(format!("{p:02}.pal"));
                if let Ok(s) = std::fs::read_to_string(&f) {
                    for (c, rgb) in parse_jasc(&s).into_iter().enumerate() {
                        let a = if c == 0 { 0 } else { 255 }; // color 0 = transparent
                        palettes[p * 16 + c] = [rgb[0], rgb[1], rgb[2], a];
                    }
                }
            }
            let metatiles = read_u16s(&pret.join(bound_path(&metatiles_h, mt_sym)));
            let attributes = read_u16s(&pret.join(bound_path(&metatiles_h, attr_sym)));
            TilesetData {
                name: name.clone(),
                is_secondary: *is_secondary,
                tiles_4bpp,
                palettes,
                metatiles,
                attributes,
            }
        })
        .collect();

    // ── layouts ───────────────────────────────────────────────────────────
    let layouts_json: Value =
        serde_json::from_str(&read(pret, "data/layouts/layouts.json")).unwrap();
    let mut pairs: Vec<(u16, u16)> = Vec::new();
    let mut pair_index: HashMap<(u16, u16), u16> = HashMap::new();
    let mut layouts: Vec<LayoutData> = Vec::new();
    let mut layout_index: HashMap<String, u16> = HashMap::new();
    for l in layouts_json["layouts"].as_array().unwrap() {
        if l.is_null() {
            continue;
        }
        let id = l["id"].as_str().unwrap().to_string();
        // LAYOUT_UNUSED_OUTDOOR_AREA has literal "0" tilesets — skip it
        let (Some(&prim), Some(&sec)) = (
            ts_index.get(l["primary_tileset"].as_str().unwrap()),
            ts_index.get(l["secondary_tileset"].as_str().unwrap()),
        ) else {
            continue;
        };
        let pair = *pair_index.entry((prim, sec)).or_insert_with(|| {
            pairs.push((prim, sec));
            pairs.len() as u16 - 1
        });
        let width = l["width"].as_u64().unwrap() as u32;
        let height = l["height"].as_u64().unwrap() as u32;
        let mut blockdata = read_u16s(&pret.join(l["blockdata_filepath"].as_str().unwrap()));
        if blockdata.len() != (width * height) as usize {
            // a couple of pret's UNUSED_ layouts have stray sizes; make them sane
            eprintln!("note: {id} blockdata {} != {}x{height}, fixing up", blockdata.len(), width);
            blockdata.resize((width * height) as usize, 0);
        }
        layout_index.insert(id.clone(), layouts.len() as u16);
        layouts.push(LayoutData { id, width, height, pair, blockdata });
    }

    // ── maps ──────────────────────────────────────────────────────────────
    let groups: Value = serde_json::from_str(&read(pret, "data/maps/map_groups.json")).unwrap();
    let map_names: Vec<String> = groups["group_order"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| groups[g.as_str().unwrap()].as_array().unwrap())
        .map(|m| m.as_str().unwrap().to_string())
        .collect();

    struct RawMap {
        id: String, // MAP_X constant
        music: String,
        layout: u16,
        connections: Vec<(String, i64, String)>, // (direction, offset, dest MAP_X)
        warps: Vec<(i32, i32, String, usize)>,   // (x, y, dest MAP_X, dest warp idx)
    }
    let mut raws: Vec<RawMap> = Vec::new();
    for name in &map_names {
        let j: Value =
            serde_json::from_str(&read(pret, &format!("data/maps/{name}/map.json"))).unwrap();
        let layout = match layout_index.get(j["layout"].as_str().unwrap()) {
            Some(&i) => i,
            None => continue, // layout absent from layouts.json (unused stub)
        };
        let connections = j["connections"]
            .as_array()
            .map(|cs| {
                cs.iter()
                    .filter_map(|c| {
                        let dir = c["direction"].as_str()?.to_string();
                        matches!(dir.as_str(), "up" | "down" | "left" | "right").then(|| {
                            (dir, c["offset"].as_i64().unwrap(), c["map"].as_str().unwrap().into())
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let warps = j["warp_events"]
            .as_array()
            .map(|ws| {
                ws.iter()
                    .filter_map(|w| {
                        let dest: String = w["dest_map"].as_str()?.into();
                        let idx: usize = w["dest_warp_id"].as_str()?.parse().ok()?;
                        Some((w["x"].as_i64()? as i32, w["y"].as_i64()? as i32, dest, idx))
                    })
                    .collect()
            })
            .unwrap_or_default();
        raws.push(RawMap {
            id: j["id"].as_str().unwrap().to_string(),
            music: j["music"].as_str().unwrap_or("MUS_NONE").to_string(),
            layout,
            connections,
            warps,
        });
    }
    let raw_index: HashMap<String, usize> =
        raws.iter().enumerate().map(|(i, m)| (m.id.clone(), i)).collect();

    // ── stitch the overworld realm: BFS over directional connections ─────
    // (carries over pokeemerald_SDL3's single overworld coordinate space)
    let lw = |m: &RawMap| layouts[m.layout as usize].width as i32;
    let lh = |m: &RawMap| layouts[m.layout as usize].height as i32;
    let mut pos: HashMap<usize, (i32, i32)> = HashMap::new();
    let seed = raw_index["MAP_LITTLEROOT_TOWN"];
    pos.insert(seed, (0, 0));
    let mut queue = std::collections::VecDeque::from([seed]);
    while let Some(i) = queue.pop_front() {
        let (x, y) = pos[&i];
        let conns = raws[i].connections.clone();
        for (dir, off, dest) in conns {
            let Some(&j) = raw_index.get(&dest) else { continue };
            // Emerald's connection graph has ONE inconsistent cycle: the
            // western cluster (Petalburg..Dewford..Fallarbor) sits 2 tiles
            // higher than the Route 102 pair claims (the GBA only ever uses
            // connections pairwise, so it never notices). Any global stitch
            // must break exactly one pair; breaking this one satisfies every
            // other connection and reproduces pokeemerald_SDL3's positions —
            // the alternative breaks THREE seams (Verdanturf/Route 116,
            // Fallarbor/Route 114, Dewford/Route 107), all visibly torn.
            if matches!(
                (raws[i].id.as_str(), dest.as_str()),
                ("MAP_ROUTE102", "MAP_PETALBURG_CITY") | ("MAP_PETALBURG_CITY", "MAP_ROUTE102")
            ) {
                continue;
            }
            let o = off as i32;
            let p = match dir.as_str() {
                "up" => (x + o, y - lh(&raws[j])),
                "down" => (x + o, y + lh(&raws[i])),
                "left" => (x - lw(&raws[j]), y + o),
                "right" => (x + lw(&raws[i]), y + o),
                _ => unreachable!(),
            };
            match pos.get(&j) {
                // Emerald's connections are not globally consistent (the GBA
                // only ever uses them pairwise) — first placement wins, same
                // as pokeemerald_SDL3's generated positions
                Some(&q) if q != p => {
                    eprintln!("note: {dest} placed at {q:?}, ignoring conflicting {p:?}")
                }
                Some(_) => {}
                None => {
                    pos.insert(j, p);
                    queue.push_back(j);
                }
            }
        }
    }
    let minx = pos.values().map(|p| p.0).min().unwrap();
    let miny = pos.values().map(|p| p.1).min().unwrap();
    let maxx = pos.iter().map(|(&i, p)| p.0 + lw(&raws[i])).max().unwrap();
    let maxy = pos.iter().map(|(&i, p)| p.1 + lh(&raws[i])).max().unwrap();
    println!(
        "overworld: {} maps stitched, {}x{} metatiles",
        pos.len(),
        maxx - minx,
        maxy - miny
    );
    for check in ["MAP_LITTLEROOT_TOWN", "MAP_ROUTE115", "MAP_RUSTBORO_CITY"] {
        let (x, y) = pos[&raw_index[check]];
        println!("  {check} at ({}, {})", x - minx, y - miny);
    }

    // ── realm assignment: 0 = overworld, then one per remaining map ──────
    let mut realm_of: HashMap<usize, u16> = HashMap::new();
    let mut origin_of: HashMap<usize, (i32, i32)> = HashMap::new();
    for (&i, &(x, y)) in &pos {
        realm_of.insert(i, 0);
        origin_of.insert(i, (x - minx, y - miny));
    }
    let mut next_realm = 1u16;
    for i in 0..raws.len() {
        if !realm_of.contains_key(&i) {
            realm_of.insert(i, next_realm);
            origin_of.insert(i, (0, 0));
            next_realm += 1;
        }
    }

    // warp resolution: dest (map, warp idx) → (realm, realm-local x, y)
    let resolve = |dest: &str, idx: usize| -> Option<(u16, i32, i32)> {
        let &j = raw_index.get(dest)?;
        let &(wx, wy, ..) = raws[j].warps.get(idx)?;
        let (ox, oy) = origin_of[&j];
        Some((realm_of[&j], ox + wx, oy + wy))
    };

    let mut realms: Vec<RealmData> = Vec::new();
    realms.push(RealmData {
        width: (maxx - minx) as u32,
        height: (maxy - miny) as u32,
        maps: Vec::new(),
    });
    // realm order must match realm_of; build single-map realms in raws order
    for i in 0..raws.len() {
        let r = realm_of[&i];
        if r as usize >= realms.len() {
            let m = &raws[i];
            realms.push(RealmData {
                width: layouts[m.layout as usize].width,
                height: layouts[m.layout as usize].height,
                maps: Vec::new(),
            });
        }
    }
    for (i, m) in raws.iter().enumerate() {
        let (ox, oy) = origin_of[&i];
        let warps = m
            .warps
            .iter()
            .filter_map(|&(x, y, ref dest, idx)| {
                let (to_realm, to_x, to_y) = resolve(dest, idx)?;
                Some(WarpData { x: ox + x, y: oy + y, to_realm, to_x, to_y })
            })
            .collect();
        realms[realm_of[&i] as usize].maps.push(MapData {
            id: m.id.clone(),
            music: m.music.clone(),
            layout: m.layout,
            x: ox,
            y: oy,
            warps,
        });
    }

    // ── player sprite: brendan walking + running sheets stacked (rows 0/1;
    // both are 9 frames of 16x32 in the same layout), baked to RGBA here ──
    let pal = parse_jasc(&read(pret, "graphics/object_events/palettes/brendan.pal"));
    let mut player = Image { rgba: Vec::new(), w: 0, h: 0 };
    for sheet in ["walking", "running"] {
        let (idx, w, h) = decode_indexed_png(
            &pret.join(format!("graphics/object_events/pics/people/brendan/{sheet}.png")),
        );
        for &c in &idx {
            let [r, g, b] = pal[c as usize];
            player.rgba.extend([r, g, b, if c == 0 { 0 } else { 255 }]);
        }
        player.w = w;
        player.h += h;
    }

    // ── the latin_normal font: 2bpp glyph sheet in 16x16 cells, indexed by
    // the Emerald text encoding (charmap.txt), advance widths from fonts.c.
    // Baked to one horizontal strip of printable-ASCII glyphs. ────────────
    let (font, font_widths) = extract_font(pret);

    // ── avatar sheets: surfing + mach bike (player palette), and the surf
    // blob — whose template has no palette tag: on hardware it renders with
    // OAM palette 0, the player's own palette ─────────────────────────────
    let bake_sheet = |rel: &str| -> Image {
        let (idx, w, h) = decode_indexed_png(&pret.join(rel));
        let mut rgba = Vec::with_capacity(idx.len() * 4);
        for &c in &idx {
            let [r, g, b] = pal[c as usize];
            rgba.extend([r, g, b, if c == 0 { 0 } else { 255 }]);
        }
        Image { rgba, w, h }
    };
    let surf_player = bake_sheet("graphics/object_events/pics/people/brendan/surfing.png");
    let bike = bake_sheet("graphics/object_events/pics/people/brendan/mach_bike.png");
    let surf_blob = bake_sheet("graphics/field_effects/pics/surf_blob.png");

    // ── door open animations (src/field_door.c) ───────────────────────────
    let doors = extract_doors(pret, &ts_index, &tilesets);

    // ── surfable behaviors: pret's own sTileBitAttributes table ───────────
    let surfable = extract_surfable(pret);
    let warp_dir = extract_warp_dirs(pret);

    // start in front of the Mauville City Pokémon Center: one tile south of
    // its door warp, derived from the map data rather than hardcoded coords
    let mv = raw_index["MAP_MAUVILLE_CITY"];
    let (mvx, mvy) = origin_of[&mv];
    let (pcx, pcy, ..) = *raws[mv]
        .warps
        .iter()
        .find(|w| w.2 == "MAP_MAUVILLE_CITY_POKEMON_CENTER_1F")
        .expect("Mauville has a Pokémon Center door");
    let start = Start { realm: 0, x: mvx + pcx, y: mvy + pcy + 1 };
    let ow = &realms[0];
    let at_start = ow.maps.iter().find(|m| {
        let l = &layouts[m.layout as usize];
        (m.x..m.x + l.width as i32).contains(&start.x)
            && (m.y..m.y + l.height as i32).contains(&start.y)
    });
    println!(
        "start ({}, {}) lands in {:?}",
        start.x,
        start.y,
        at_start.map(|m| m.id.as_str())
    );

    WorldPack {
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
    }
}

// ── file helpers ─────────────────────────────────────────────────────────

fn read(pret: &Path, rel: &str) -> String {
    std::fs::read_to_string(pret.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Like `read`, but tolerates absence (older/newer pret trees move files).
fn read2(pret: &Path, rel: &str) -> String {
    std::fs::read_to_string(pret.join(rel)).unwrap_or_default()
}

fn read_u16s(p: &Path) -> Vec<u16> {
    let b = std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
}

/// Decode an indexed PNG to one byte per pixel (palette indices).
fn decode_indexed_png(p: &Path) -> (Vec<u8>, u32, u32) {
    let f = std::fs::File::open(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    let mut dec = png::Decoder::new(f);
    dec.set_transformations(png::Transformations::IDENTITY);
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Indexed, "{}", p.display());
    let (w, h) = (info.width, info.height);
    let mut idx = Vec::with_capacity((w * h) as usize);
    let row_bytes = info.line_size;
    for y in 0..h as usize {
        let row = &buf[y * row_bytes..];
        match info.bit_depth {
            png::BitDepth::Two => {
                for x in 0..w as usize {
                    let b = row[x / 4];
                    idx.push((b >> (6 - 2 * (x % 4))) & 0x3);
                }
            }
            png::BitDepth::Four => {
                for x in 0..w as usize {
                    let b = row[x / 2];
                    idx.push(if x % 2 == 0 { b >> 4 } else { b & 0xF });
                }
            }
            png::BitDepth::Eight => idx.extend_from_slice(&row[..w as usize]),
            d => panic!("{}: unsupported bit depth {d:?}", p.display()),
        }
    }
    (idx, w, h)
}

/// Repack pixel indices (image row-major, 8x8 tiles in rows of w/8) into GBA
/// 4bpp tile order: 32 bytes per tile, low nibble = left pixel.
fn to_gba_4bpp(idx: &[u8], w: u32) -> Vec<u8> {
    let tiles_x = (w / 8) as usize;
    let tiles = idx.len() / 64;
    let mut out = Vec::with_capacity(tiles * 32);
    for t in 0..tiles {
        let (tx, ty) = (t % tiles_x, t / tiles_x);
        for y in 0..8 {
            for x in [0, 2, 4, 6] {
                let at = (ty * 8 + y) * w as usize + tx * 8 + x;
                out.push(idx[at] & 0xF | (idx[at + 1] << 4));
            }
        }
    }
    out
}

/// Doors: parse metatile_labels.h for METATILE_<Tileset>_* values and
/// field_door.c for the door table (metatile, tiles png, per-tile palette
/// slots), then bake each door's three 16x32 open frames to RGBA with the
/// owning tilesets' palettes (slots 0-5 = the paired primary = General for
/// every overworld door; 6-11 = the door tileset's own).
fn extract_doors(
    pret: &Path,
    ts_index: &HashMap<String, u16>,
    tilesets: &[TilesetData],
) -> Vec<DoorData> {
    let labels = read(pret, "include/constants/metatile_labels.h");
    let mut label_val: HashMap<String, u16> = HashMap::new();
    for line in labels.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("#define METATILE_") {
            let mut it = rest.split_whitespace();
            if let (Some(name), Some(v)) = (it.next(), it.next()) {
                if let Ok(v) = u16::from_str_radix(v.trim_start_matches("0x"), 16) {
                    label_val.insert(name.to_string(), v);
                }
            }
        }
    }

    let door_c = read(pret, "src/field_door.c");
    // tiles symbols → png paths
    let mut tile_paths: HashMap<String, String> = HashMap::new();
    for line in door_c.lines() {
        if let Some((sym, rest)) = line
            .trim()
            .strip_prefix("static const u8 sDoorAnimTiles_")
            .and_then(|r| r.split_once("[] = INCGFX_U8(\""))
        {
            if let Some(path) = rest.split('"').next() {
                tile_paths.insert(format!("sDoorAnimTiles_{sym}"), path.to_string());
            }
        }
    }
    // palette slot arrays
    let mut pal_slots: HashMap<String, Vec<usize>> = HashMap::new();
    for chunk in door_c.split("static const u8 sDoorAnimPalettes_").skip(1) {
        let Some((name, rest)) = chunk.split_once("[] = {") else { continue };
        let Some((body, _)) = rest.split_once('}') else { continue };
        let slots = body.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        pal_slots.insert(format!("sDoorAnimPalettes_{name}"), slots);
    }

    let general = ts_index.get("gTileset_General").copied();
    let mut out = Vec::new();
    let table = door_c.split("sDoorAnimGraphicsTable[] =").nth(1).unwrap_or("");
    for line in table.split_once('}').map(|_| table).unwrap_or("").lines() {
        let line = line.trim();
        if line.starts_with("};") {
            break;
        }
        let Some(rest) = line.strip_prefix("{METATILE_") else { continue };
        let fields: Vec<&str> = rest.trim_end_matches("},").split(',').map(str::trim).collect();
        if fields.len() < 5 {
            continue;
        }
        let label = fields[0];
        let Some(&metatile) = label_val.get(label) else { continue };
        let ts_name = label.split('_').next().unwrap_or("");
        let Some(&tileset) = ts_index.get(&format!("gTileset_{ts_name}")) else { continue };
        let Some(path) = tile_paths.get(fields[3]) else { continue };
        let Some(slots) = pal_slots.get(fields[4]) else { continue };
        if fields[2] != "1" {
            continue; // the lone 2-wide door (battle tower multi) — skip
        }

        let (idx, w, h) = decode_indexed_png(&pret.join(path));
        if w != 16 || h < 96 {
            continue;
        }
        // per-tile palette slots: 2x4 tiles per 16x32 frame; slots <6 come
        // from the paired primary (General for all overworld doors)
        let door_ts = &tilesets[tileset as usize];
        let prim = general.map(|g| &tilesets[g as usize]).unwrap_or(door_ts);
        let mut rgba = vec![0u8; 16 * 96 * 4];
        for (i, &c) in idx.iter().take(16 * 96).enumerate() {
            if c == 0 {
                continue;
            }
            let (x, y) = (i % 16, i / 16);
            let tile_in_frame = (x / 8) + ((y % 32) / 8) * 2;
            let slot = slots.get(tile_in_frame).copied().unwrap_or(0);
            let src = if slot < 6 && door_ts.is_secondary { prim } else { door_ts };
            let color = src.palettes[slot * 16 + c as usize];
            rgba[i * 4..i * 4 + 4].copy_from_slice(&color);
        }
        out.push(DoorData { tileset, metatile, frames: Image { rgba, w: 16, h: 96 } });
    }
    println!("doors extracted: {}", out.len());
    out
}

/// Surfable = pret's TILE_FLAG_SURFABLE rows in sTileBitAttributes, resolved
/// through the MB_* enum order in metatile_behaviors.h. No hardcoded values.
fn extract_surfable(pret: &Path) -> [u8; 32] {
    let hdr = read(pret, "include/constants/metatile_behaviors.h");
    let mut val = 0u16;
    let mut mb_val: HashMap<String, u16> = HashMap::new();
    for line in hdr.lines() {
        let line = line.split("//").next().unwrap().trim().trim_end_matches(',');
        if let Some(name) = line.strip_prefix("MB_") {
            let name = name.split_whitespace().next().unwrap_or("");
            if let Some((n, v)) = name.split_once('=') {
                let _ = (n, v); // no explicit values in this enum
            }
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
                mb_val.insert(name.to_string(), val);
                val += 1;
            }
        }
    }
    let src = read(pret, "src/metatile_behavior.c");
    let table = src.split("sTileBitAttributes[").nth(1).unwrap_or("").split("};").next().unwrap_or("");
    let mut mask = [0u8; 32];
    for line in table.lines() {
        if !line.contains("TILE_FLAG_SURFABLE") {
            continue;
        }
        let Some(name) = line.split('[').nth(1).and_then(|r| r.split(']').next()) else {
            continue;
        };
        if let Some(&v) = mb_val.get(name.trim().trim_start_matches("MB_")) {
            mask[(v / 8) as usize] |= 1 << (v % 8);
        }
    }
    println!("surfable behaviors: {}", mask.iter().map(|b| b.count_ones()).sum::<u32>());
    mask
}

/// Arrow warps fire on a directional press while standing on the tile, not
/// on stepping onto it. Resolve the MB_*_ARROW_WARP names through the enum.
fn extract_warp_dirs(pret: &Path) -> [u8; 256] {
    let hdr = read(pret, "include/constants/metatile_behaviors.h");
    let mut val = 0u16;
    let mut out = [0u8; 256];
    for line in hdr.lines() {
        let line = line.split("//").next().unwrap().trim().trim_end_matches(',');
        if let Some(name) = line.strip_prefix("MB_") {
            let name = name.split_whitespace().next().unwrap_or("");
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                let dir = match name {
                    "SOUTH_ARROW_WARP" | "WATER_SOUTH_ARROW_WARP" => 1,
                    "NORTH_ARROW_WARP" => 2,
                    "EAST_ARROW_WARP" => 3,
                    "WEST_ARROW_WARP" => 4,
                    _ => 0,
                };
                if let Some(slot) = out.get_mut(val as usize) {
                    *slot = dir;
                }
                val += 1;
            }
        }
    }
    out
}

/// Bake Emerald's latin_normal font for printable ASCII: glyph cells looked
/// up through charmap.txt, colored with the standard dark-gray text + light
/// shadow, advance widths from gFontNormalLatinGlyphWidths in src/fonts.c.
fn extract_font(pret: &Path) -> (Image, Vec<u8>) {
    // charmap.txt: 'A'         = BB  (single-byte latin entries only)
    let mut code_of: HashMap<char, usize> = HashMap::new();
    for line in read(pret, "charmap.txt").lines() {
        let line = line.split('@').next().unwrap().trim();
        let Some((lhs, rhs)) = line.split_once('=') else { continue };
        let (lhs, rhs) = (lhs.trim(), rhs.trim());
        let mut chars = lhs.chars();
        if let (Some('\''), Some(c), Some('\''), None, Ok(v)) = (
            chars.next(),
            chars.next(),
            chars.next(),
            chars.next(),
            usize::from_str_radix(rhs, 16),
        ) {
            if rhs.len() == 2 {
                code_of.entry(c).or_insert(v);
            }
        }
    }
    code_of.insert(' ', 0); // charmap spells space as a literal blank

    // gFontNormalLatinGlyphWidths: 256 comma-separated bytes
    let fonts_c = read(pret, "src/fonts.c");
    let table = fonts_c
        .split_once("gFontNormalLatinGlyphWidths[] = {")
        .expect("width table")
        .1
        .split_once("};")
        .unwrap()
        .0;
    let widths: Vec<u8> = table
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();

    let (px, sheet_w, _h) = decode_indexed_png(&pret.join("graphics/fonts/latin_normal.png"));
    let cols = (sheet_w / 16) as usize;

    const FIRST: u8 = 32;
    const LAST: u8 = 126;
    // + one extra solid-white cell at the end: the UI's fill/border source
    let n = (LAST - FIRST + 1) as usize + 1;
    let mut strip = vec![0u8; n * 16 * 16 * 4];
    for y in 0..16usize {
        for x in 0..16usize {
            let at = ((y * n * 16) + (n - 1) * 16 + x) * 4;
            strip[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let mut font_widths = vec![0u8; n];
    font_widths[n - 1] = 16;
    for (slot, ch) in (FIRST..=LAST).enumerate() {
        let Some(&code) = code_of.get(&(ch as char)) else { continue };
        font_widths[slot] = widths.get(code).copied().unwrap_or(6).max(3);
        let (cx, cy) = (code % cols, code / cols);
        for y in 0..16usize {
            for x in 0..16usize {
                let v = px[(cy * 16 + y) * sheet_w as usize + cx * 16 + x];
                // 2bpp: 0 transparent, 1 text, 2 shadow
                let rgba: [u8; 4] = match v {
                    1 => [96, 96, 104, 255],
                    2 => [208, 208, 216, 255],
                    _ => [0, 0, 0, 0],
                };
                let at = ((y * n * 16) + slot * 16 + x) * 4;
                strip[at..at + 4].copy_from_slice(&rgba);
            }
        }
    }
    (Image { rgba: strip, w: n as u32 * 16, h: 16 }, font_widths)
}

/// JASC-PAL: "JASC-PAL\n0100\n<n>\nR G B\n..." → RGB triples.
fn parse_jasc(s: &str) -> Vec<[u8; 3]> {
    s.lines()
        .skip(3)
        .filter_map(|l| {
            let mut it = l.split_whitespace().map(|v| v.parse::<u8>().ok());
            Some([it.next()??, it.next()??, it.next()??])
        })
        .collect()
}
