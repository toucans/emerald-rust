//! Pure game logic. Emits plain `types` data; no backend crate's vocabulary,
//! no I/O, no hidden globals. The sim advances in fixed steps ([`SIM_DT`]) and
//! rendering interpolates between the previous and current sim state — see
//! README "Simulation: fixed-step, interpolated render".
//!
//! The whole world is resident in RAM: `World::new` parses the world pack
//! (pret-derived, written by `worldgen`), bakes the indexed graphics to RGBA
//! atlases, and expands every realm. `frame()` culls to the camera-visible
//! rect — resident ≠ "draw everything".

pub mod bake;
pub mod data;
pub mod map;
pub mod player;
pub mod ui;

use bake::Baked;
use map::{RealmGraph, RealmId, METATILE};
use player::{Avatar, Facing, Player, StepEvents};
use types::{Flip, Frame, Input, Quad, Rect, Rgba, TextureId};

/// Conservative GL texture-size limit for the offscreen target (WebGL2
/// guarantees 2048; virtually everything real supports 4096).
const MAX_TARGET: f32 = 4096.0;

/// The GBA's sim/audio tick rate. Displays run 60/120/144 Hz; rendering
/// interpolates, the sim never varies.
pub const SIM_HZ: f64 = 59.7275;
pub const SIM_DT: f64 = 1.0 / SIM_HZ;

/// Fixed-timestep accumulator: feed it elapsed wall time, run `step()` the
/// returned number of times, then render with the returned alpha. Pure — the
/// adapter supplies the clock, per "the backend drives the loop".
#[derive(Default)]
pub struct FixedStep {
    acc: f64,
}

impl FixedStep {
    /// Cap on catch-up steps per render; beyond it we drop time rather than
    /// spiral (e.g. after the tab was backgrounded).
    const MAX_STEPS: u32 = 5;

    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `(steps_to_run, render_alpha)`.
    pub fn advance(&mut self, elapsed_secs: f64) -> (u32, f32) {
        self.acc += elapsed_secs.clamp(0.0, 0.25);
        let mut steps = 0;
        while self.acc >= SIM_DT {
            self.acc -= SIM_DT;
            steps += 1;
            if steps == Self::MAX_STEPS {
                self.acc = 0.0;
                break;
            }
        }
        (steps, (self.acc / SIM_DT) as f32)
    }
}

pub const TILE: f32 = METATILE;

/// Draw layers, GBA-style: independent producers push quads without knowing
/// the global order. "Covered" metatile tops sit above the bottom layer but
/// below the player; normal/split tops cover the player (tree canopies).
const LAYER_BG: i16 = 0;
const LAYER_BG_TOP: i16 = 1;
const LAYER_PLAYER: i16 = 10;
const LAYER_FG: i16 = 20;
const LAYER_UI: i16 = 100;

/// Player sprite frames in pret's walking.png order (16x32 each).
const FRAME_STAND_DOWN: u32 = 0;
const FRAME_STAND_UP: u32 = 1;
const FRAME_STAND_SIDE: u32 = 2;
const FRAME_WALK_DOWN: [u32; 2] = [3, 4];
const FRAME_WALK_UP: [u32; 2] = [5, 6];
const FRAME_WALK_SIDE: [u32; 2] = [7, 8];

/// The camera lives in the game: `{ pos, zoom }`, zoom a continuous scalar.
/// World quads fold both into `dst`; UI quads ignore them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// World-pixel position the camera looks at (screen center).
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

/// One sim state — everything that moves stores into this, so the renderer
/// keeps `prev` + `curr` and lerps. Plain data, serializable by construction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// The active realm — camera, movement, and rendering operate within it.
    pub realm: RealmId,
    pub player: Player,
    pub cam: Camera,
    /// Levitation mode: walk through everything; the sprite hovers with a
    /// 1 px bob instead of the walk animation.
    pub levitate: bool,
    /// Start-menu cursor; None = closed. Transient — not part of saves.
    pub menu: Option<u8>,
    /// Sim frame counter driving UI/bob cadence (the tileset-anim interval).
    pub ticks: u32,
    /// Warp/door sequence in progress (input is ignored while Some).
    /// Transient — not part of saves.
    pub transition: Option<Transition>,
    /// Last step's input, for press-edge detection. Transient.
    prev_input: Input,
}

/// The GBA's warp choreography: door opens (16 frames) → player walks in →
/// fade out (12) → realm switch → fade in (12) → walk off the arrival tile
/// (doors get a closing animation behind the player). Levitate mode skips
/// all of it — warps stay instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    DoorOpen { x: i32, y: i32, t: u8 },
    /// Player mid-walk onto the open door; the warp fires on arrival.
    DoorWait { x: i32, y: i32 },
    FadeOut { warp: map::Warp, t: u8, door: bool },
    FadeIn { t: u8, door: bool },
    /// Walking off the arrival tile; doors close behind (`door_xy`).
    ExitWalk { door_xy: Option<(i32, i32)> },
    DoorClose { x: i32, y: i32, t: u8 },
}

const DOOR_T: u8 = 16; // 4 anim steps x 4 frames, pret's sDoorOpenAnimFrames
const FADE_T: u8 = 12;

impl State {
    /// Serialize to a small stable byte form — the whole sim state is this
    /// struct (pure function of (state, input)), so this IS a save file.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![b'E', b'S', 3]; // magic + version
        b.extend(self.realm.to_le_bytes());
        b.extend(self.player.x.to_le_bytes());
        b.extend(self.player.y.to_le_bytes());
        b.push(match self.player.facing {
            Facing::Up => 0,
            Facing::Down => 1,
            Facing::Left => 2,
            Facing::Right => 3,
        });
        let (motion, arg) = match self.player.motion {
            player::Motion::Idle => (0u8, 0.0f32),
            player::Motion::Turning { steps_left } => (1, steps_left as f32),
            player::Motion::Moving { remaining_px, speed } => {
                (if speed >= player::RUN_PX_PER_STEP { 3 } else { 2 }, remaining_px)
            }
        };
        b.push(motion);
        b.extend(arg.to_le_bytes());
        b.push(self.player.step_loop);
        b.extend(self.player.current_map.unwrap_or(u16::MAX).to_le_bytes());
        b.extend(self.cam.x.to_le_bytes());
        b.extend(self.cam.y.to_le_bytes());
        b.extend(self.cam.zoom.to_le_bytes());
        b.push(self.levitate as u8);
        b.push(match self.player.avatar {
            Avatar::Walking => 0,
            Avatar::Surfing => 1,
            Avatar::Bike { tier, .. } => 2 + tier.min(2),
        });
        b.push(self.player.elevation & 0xF);
        b
    }

    pub fn from_bytes(b: &[u8]) -> Result<State, data::PackError> {
        let err = data::PackError("bad save state");
        if b.len() != 37 || b[..3] != [b'E', b'S', 3] {
            return Err(err);
        }
        let avatar = match b[35] {
            0 => Avatar::Walking,
            1 => Avatar::Surfing,
            t @ 2..=4 => Avatar::Bike { tier: t - 2, momentum: (t - 2) + (t - 2) / 2 },
            _ => return Err(err),
        };
        let u16_ = |i: usize| u16::from_le_bytes(b[i..i + 2].try_into().unwrap());
        let i32_ = |i: usize| i32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let f32_ = |i: usize| f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let facing = match b[13] {
            0 => Facing::Up,
            1 => Facing::Down,
            2 => Facing::Left,
            3 => Facing::Right,
            _ => return Err(err),
        };
        let motion = match (b[14], f32_(15)) {
            (0, _) => player::Motion::Idle,
            (1, n) if (1.0..=player::TURN_STEPS as f32).contains(&n) => {
                player::Motion::Turning { steps_left: n as u8 }
            }
            (m @ (2 | 3), px) if px > 0.0 && px <= METATILE => player::Motion::Moving {
                remaining_px: px,
                speed: if m == 3 { player::RUN_PX_PER_STEP } else { player::WALK_PX_PER_STEP },
            },
            _ => return Err(err),
        };
        Ok(State {
            realm: u16_(3),
            player: Player {
                x: i32_(5),
                y: i32_(9),
                facing,
                motion,
                step_loop: b[19] & 1,
                avatar,
                elevation: b[36] & 0xF,
                current_map: match u16_(20) {
                    u16::MAX => None,
                    m => Some(m),
                },
            },
            cam: Camera { x: f32_(22), y: f32_(26), zoom: f32_(30) },
            levitate: b[34] != 0,
            menu: None,
            ticks: 0,
            transition: None,
            prev_input: Input::default(),
        })
    }
}

/// The start menu, Emerald-style (start_menu.c's list adapted to what this
/// build can do). Index = cursor position.
const MENU_LEVITATE: u8 = 0;
const MENU_ZOOM: u8 = 1;
const MENU_SAVE: u8 = 2;
const MENU_LOAD: u8 = 3;
const MENU_EXIT: u8 = 4;
const MENU_LEN: u8 = 5;

const ZOOM_RATE: f32 = 1.02; // multiplicative per sim step
const ZOOM_MIN: f32 = 0.5;
const ZOOM_MAX: f32 = 6.0;

pub struct World {
    graph: RealmGraph,
    baked: Baked,
    atlas_tex: Vec<TextureId>,
    player_tex: TextureId,
    surf_player_tex: TextureId,
    surf_blob_tex: TextureId,
    bike_tex: TextureId,
    door_tex: Vec<TextureId>,
    font_tex: TextureId,
    font_widths: Vec<u8>,
    /// The in-memory save slot behind the menu's SAVE/LOAD.
    snapshot: Option<Vec<u8>>,
    prev: State,
    curr: State,
}

impl World {
    /// Parse the world pack, bake atlases, upload them through the caller's
    /// `upload` (the one place the adapter's `Backend::load_atlas` is fed),
    /// and expand every realm — the whole world, resident.
    pub fn new(
        pack_bytes: &[u8],
        upload: &mut dyn FnMut(&[u8], u32, u32) -> TextureId,
    ) -> Result<Self, data::PackError> {
        let pack = data::WorldPack::from_bytes(pack_bytes)?;
        let baked = bake::bake(&pack);
        let atlas_tex =
            baked.atlases.iter().map(|a| upload(&a.rgba, a.w, a.h)).collect();
        let player_tex = upload(&pack.player.rgba, pack.player.w, pack.player.h);
        let surf_player_tex =
            upload(&pack.surf_player.rgba, pack.surf_player.w, pack.surf_player.h);
        let surf_blob_tex = upload(&pack.surf_blob.rgba, pack.surf_blob.w, pack.surf_blob.h);
        let bike_tex = upload(&pack.bike.rgba, pack.bike.w, pack.bike.h);
        let door_tex = pack
            .doors
            .iter()
            .map(|d| upload(&d.frames.rgba, d.frames.w, d.frames.h))
            .collect();
        let font_tex = upload(&pack.font.rgba, pack.font.w, pack.font.h);
        let font_widths = pack.font_widths.clone();
        let graph = RealmGraph::from_pack(&pack);

        let start = pack.start;
        let player = Player::spawn(start.x, start.y, graph.realm(start.realm));
        let (px, py) = player.world_px();
        let cam = Camera { x: px + METATILE / 2.0, y: py + METATILE / 2.0, zoom: 2.0 };
        let s = State {
            realm: start.realm,
            player,
            cam,
            levitate: false,
            menu: None,
            ticks: 0,
            transition: None,
            prev_input: Input::default(),
        };
        Ok(Self {
            graph,
            baked,
            atlas_tex,
            player_tex,
            surf_player_tex,
            surf_blob_tex,
            bike_tex,
            door_tex,
            font_tex,
            font_widths,
            snapshot: None,
            prev: s,
            curr: s,
        })
    }

    pub fn state(&self) -> &State {
        &self.curr
    }

    pub fn graph(&self) -> &RealmGraph {
        &self.graph
    }

    /// The MUS_* id of the map the player is standing in — what should be
    /// playing right now. The adapter feeds this to the music engine.
    pub fn current_music(&self) -> Option<&str> {
        let s = &self.curr;
        match s.player.avatar {
            Avatar::Surfing => return Some("MUS_SURF"),
            Avatar::Bike { .. } => return Some("MUS_CYCLING"),
            Avatar::Walking => {}
        }
        let m = self.graph.realm(s.realm).maps.get(s.player.current_map? as usize)?;
        Some(&m.music)
    }

    /// Save the sim: the state alone is the save (save-anywhere for free).
    pub fn save_state(&self) -> Vec<u8> {
        self.curr.to_bytes()
    }

    /// Restore a saved sim state. Snaps prev = curr — a load is a discrete
    /// jump, never interpolated (same rule as warps).
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), data::PackError> {
        let s = State::from_bytes(bytes)?;
        if s.realm as usize >= self.graph.realms.len()
            || !self.graph.realm(s.realm).in_bounds(s.player.x, s.player.y)
        {
            return Err(data::PackError("save state out of world bounds"));
        }
        self.curr = s;
        self.prev = s;
        Ok(())
    }

    /// Advance one fixed sim step: player_update + camera_update, ported,
    /// plus the start menu (which freezes the overworld while open, as in
    /// Emerald). Returns the step's events for the caller.
    pub fn step(&mut self, input: Input) -> StepEvents {
        self.prev = self.curr;
        let mut events = StepEvents::default();

        let was = self.curr.prev_input;
        self.curr.prev_input = input;
        self.curr.ticks = self.curr.ticks.wrapping_add(1);
        let pressed = |now: bool, before: bool| now && !before;

        if let Some(cursor) = self.curr.menu {
            if pressed(input.up, was.up) {
                self.curr.menu = Some((cursor + MENU_LEN - 1) % MENU_LEN);
            }
            if pressed(input.down, was.down) {
                self.curr.menu = Some((cursor + 1) % MENU_LEN);
            }
            if pressed(input.b, was.b) || pressed(input.start, was.start) {
                self.curr.menu = None;
            } else if pressed(input.a, was.a) {
                match cursor {
                    MENU_LEVITATE => self.curr.levitate = !self.curr.levitate,
                    MENU_ZOOM => {
                        // cycle the pixel-perfect integer zooms: 1x..4x
                        let z = self.curr.cam.zoom.floor() as i32;
                        self.curr.cam.zoom = ((z % 4) + 1) as f32;
                    }
                    MENU_SAVE => {
                        self.snapshot = Some(self.curr.to_bytes());
                        self.curr.menu = None;
                    }
                    MENU_LOAD => {
                        if let Some(snap) = self.snapshot.clone() {
                            let _ = self.load_state(&snap);
                        }
                    }
                    _ => self.curr.menu = None,
                }
            }
            return events;
        }
        // warp/door choreography: input is ignored until it finishes
        if let Some(tr) = self.curr.transition {
            self.advance_transition(tr, &mut events);
            return events;
        }

        if pressed(input.start, was.start) {
            self.curr.menu = Some(0);
            return events;
        }
        // Select toggles the Mach bike (not on water) — mid-stride too, like
        // the real registered-item bike: the in-flight tile finishes at its
        // old speed, the new avatar takes over from the next one
        if pressed(input.select, was.select) {
            self.curr.player.avatar = match self.curr.player.avatar {
                Avatar::Walking => Avatar::Bike { tier: 0, momentum: 0 },
                Avatar::Bike { .. } => Avatar::Walking,
                Avatar::Surfing => Avatar::Surfing,
            };
        }
        // A facing surfable water: mount the blob (from foot or bike)
        if pressed(input.a, was.a)
            && self.curr.player.motion == player::Motion::Idle
            && self.curr.player.avatar != Avatar::Surfing
        {
            let p = self.curr.player;
            let (dx, dy) = p.facing.delta();
            let (nx, ny) = (p.x + dx, p.y + dy);
            let realm = self.graph.realm(self.curr.realm);
            if self.graph.surfable_at(self.curr.realm, nx, ny) && realm.passable(nx, ny) {
                self.curr.player.mount_surf(&self.graph, self.curr.realm);
                events.sfx = Some("se_m_surf");
            }
        }

        let s = &mut self.curr;
        let levitate = s.levitate;
        s.player.step(input, &self.graph, s.realm, input.b, levitate, &mut events);

        if let Some((x, y)) = events.door_enter {
            s.transition = Some(Transition::DoorOpen { x, y, t: 0 });
            events.sfx = Some("se_door");
        }

        if let Some(w) = events.warp {
            if levitate {
                // levitate keeps warps instant — no animations
                s.realm = w.to_realm;
                s.player.apply_warp(w, self.graph.realm(w.to_realm));
            } else {
                s.transition = Some(Transition::FadeOut { warp: w, t: 0, door: false });
                events.sfx = Some("se_exit");
            }
        }

        if input.zoom_in {
            s.cam.zoom = (s.cam.zoom * ZOOM_RATE).min(ZOOM_MAX);
        }
        if input.zoom_out {
            s.cam.zoom = (s.cam.zoom / ZOOM_RATE).max(ZOOM_MIN);
        }

        // camera locked on the player (the C slides the camera to the same
        // end position; see player.rs module docs)
        let (px, py) = s.player.world_px();
        s.cam.x = px + METATILE / 2.0;
        s.cam.y = py + METATILE / 2.0;

        if events.warp.is_some() {
            // a warp is a discrete switch between coordinate spaces — never
            // interpolate across it (transition effects come with the
            // offscreen post pass)
            self.prev = self.curr;
        }

        events
    }

    fn advance_transition(&mut self, tr: Transition, events: &mut StepEvents) {
        let s = &mut self.curr;
        match tr {
            Transition::DoorOpen { x, y, t } => {
                if t + 1 >= DOOR_T {
                    s.player.force_move(s.player.facing);
                    s.transition = Some(Transition::DoorWait { x, y });
                } else {
                    s.transition = Some(Transition::DoorOpen { x, y, t: t + 1 });
                }
            }
            Transition::DoorWait { x, y } => {
                let mut ev = StepEvents::default();
                s.player.step(Input::default(), &self.graph, s.realm, false, false, &mut ev);
                let (px, py) = s.player.world_px();
                s.cam.x = px + METATILE / 2.0;
                s.cam.y = py + METATILE / 2.0;
                if let Some(w) = ev.warp {
                    s.transition = Some(Transition::FadeOut { warp: w, t: 0, door: true });
                } else if s.player.motion == player::Motion::Idle {
                    s.transition = None; // door without a warp (shouldn't happen)
                    let _ = (x, y);
                }
            }
            Transition::FadeOut { warp, t, door } => {
                if t + 1 >= FADE_T {
                    s.realm = warp.to_realm;
                    s.player.apply_warp(warp, self.graph.realm(warp.to_realm));
                    let (px, py) = s.player.world_px();
                    s.cam.x = px + METATILE / 2.0;
                    s.cam.y = py + METATILE / 2.0;
                    s.transition = Some(Transition::FadeIn { t: 0, door });
                    self.prev = self.curr; // discrete jump, never interpolated
                } else {
                    s.transition = Some(Transition::FadeOut { warp, t: t + 1, door });
                }
            }
            Transition::FadeIn { t, door } => {
                if t + 1 >= FADE_T {
                    // walk off the arrival tile: out of an exterior door
                    // (facing down, closing it behind) or forward off the
                    // entry mat inside — the real game's warp choreography
                    let arrived_door =
                        self.graph.door_at(s.realm, s.player.x, s.player.y).is_some();
                    let _ = door;
                    if arrived_door {
                        // exits land ON the exterior door: walk down off it
                        // and close it behind. Interior arrivals just stand
                        // where the warp put them, like the real game.
                        let door_xy = (s.player.x, s.player.y);
                        s.player.force_move(Facing::Down);
                        events.sfx = Some("se_door");
                        s.transition = Some(Transition::ExitWalk { door_xy: Some(door_xy) });
                    } else {
                        s.transition = None;
                    }
                } else {
                    s.transition = Some(Transition::FadeIn { t: t + 1, door });
                }
            }
            Transition::ExitWalk { door_xy } => {
                let mut ev = StepEvents::default();
                s.player.step(Input::default(), &self.graph, s.realm, false, false, &mut ev);
                let (px, py) = s.player.world_px();
                s.cam.x = px + METATILE / 2.0;
                s.cam.y = py + METATILE / 2.0;
                if s.player.motion == player::Motion::Idle {
                    s.transition = door_xy.map(|(x, y)| Transition::DoorClose { x, y, t: 0 });
                }
            }
            Transition::DoorClose { x, y, t } => {
                s.transition =
                    (t + 1 < DOOR_T).then_some(Transition::DoorClose { x, y, t: t + 1 });
            }
        }
    }

    /// Fill `out` with this frame, interpolating prev→curr by `alpha`.
    /// Pure; reuses the caller's buffers — no allocation in the steady state.
    ///
    /// The world is rendered into the offscreen target at an *integer* pixel
    /// scale (crisp under nearest sampling); the screen pass maps the target
    /// to the continuous zoom (smooth under linear minification) and carries
    /// the UI. This is the crisp-and-smooth zoom technique from the README —
    /// and the target is where realm-transition/post effects will composite.
    pub fn frame(&self, alpha: f32, screen_w: f32, screen_h: f32, out: &mut Frame) {
        out.clear();
        let cam = Camera {
            x: lerp(self.prev.cam.x, self.curr.cam.x, alpha),
            y: lerp(self.prev.cam.y, self.curr.cam.y, alpha),
            zoom: lerp(self.prev.cam.zoom, self.curr.cam.zoom, alpha),
        };

        // integer offscreen scale: big enough to keep the screen pass a
        // minification, small enough to fit conservative GL texture limits
        let mut scale = cam.zoom.ceil().max(1.0);
        while scale > 1.0
            && (screen_w / cam.zoom * scale > MAX_TARGET || screen_h / cam.zoom * scale > MAX_TARGET)
        {
            scale -= 1.0;
        }
        // quantize the target up to 64px steps: the extra margin renders a
        // sliver more world (cropped by the screen edges), and the backend's
        // render target only needs reallocating at step boundaries instead of
        // every wheel notch — that reallocation was the zoom glitchiness
        let tw = ((screen_w / cam.zoom * scale) / 64.0).ceil() * 64.0;
        let th = ((screen_h / cam.zoom * scale) / 64.0).ceil() * 64.0;
        let (tw, th) = (tw.min(MAX_TARGET), th.min(MAX_TARGET));
        out.target_size = (tw as u32, th as u32);

        // world → offscreen target: (world - cam) * scale + target_center
        let to_target = |wx: f32, wy: f32| -> (f32, f32) {
            ((wx - cam.x) * scale + tw / 2.0, (wy - cam.y) * scale + th / 2.0)
        };
        let tile_px = METATILE * scale;

        // world cells — resident data, culled to the camera-visible rect
        let realm = self.graph.realm(self.curr.realm);
        let x0 = ((cam.x - tw / 2.0 / scale) / METATILE).floor().max(0.0) as i32;
        let y0 = ((cam.y - th / 2.0 / scale) / METATILE).floor().max(0.0) as i32;
        let x1 = (((cam.x + tw / 2.0 / scale) / METATILE).ceil() as i32).min(realm.width);
        let y1 = (((cam.y + th / 2.0 / scale) / METATILE).ceil() as i32).min(realm.height);
        for ty in y0..y1 {
            for tx in x0..x1 {
                let Some(cell) = realm.cell(tx, ty) else { continue };
                let d = self.baked.draw(cell.pair, cell.metatile);
                let (sx, sy) = to_target(tx as f32 * METATILE, ty as f32 * METATILE);
                let dst = Rect::new(sx, sy, tile_px, tile_px);
                if d.bottom != bake::CellRef::NONE {
                    out.offscreen.push(Quad {
                        tex: self.atlas_tex[d.bottom.atlas()],
                        src: d.bottom.src(),
                        dst,
                        layer: LAYER_BG,
                        tint: Rgba::WHITE,
                        flip: Flip::None,
                    });
                }
                if d.top != bake::CellRef::NONE {
                    out.offscreen.push(Quad {
                        tex: self.atlas_tex[d.top.atlas()],
                        src: d.top.src(),
                        dst,
                        layer: if d.top_above_player { LAYER_FG } else { LAYER_BG_TOP },
                        tint: Rgba::WHITE,
                        flip: Flip::None,
                    });
                }
            }
        }

        // player — interpolated glide; 16x32 sprite whose feet sit on the
        // tile, so its top is one tile above
        let (px0, py0) = self.prev.player.world_px();
        let (px1, py1) = self.curr.player.world_px();
        let (px, py) = (lerp(px0, px1, alpha), lerp(py0, py1, alpha));
        let p = &self.curr.player;
        let (stand, walk, flip) = match p.facing {
            Facing::Down => (FRAME_STAND_DOWN, FRAME_WALK_DOWN, Flip::None),
            Facing::Up => (FRAME_STAND_UP, FRAME_WALK_UP, Flip::None),
            Facing::Left => (FRAME_STAND_SIDE, FRAME_WALK_SIDE, Flip::None),
            Facing::Right => (FRAME_STAND_SIDE, FRAME_WALK_SIDE, Flip::H),
        };
        // levitate: no walk cycle — the stand frame hovering with a 1 px bob
        // at the tileset-animation interval (grass sways every 16 frames)
        let levitating = self.curr.levitate;
        let bob = if levitating && (self.curr.ticks / 16) % 2 == 1 { 1.0 } else { 0.0 };
        match p.avatar {
            Avatar::Surfing => {
                // the blob and rider are 32x32, centered on the tile, bobbing
                // together at the tileset-anim interval like the real blob
                let sbob = if (self.curr.ticks / 16) % 2 == 1 { 1.0 } else { 0.0 };
                // the rider's sitting frames come in pairs (0/2/4, pret's
                // sPicTable_BrendanSurfing); the blob sheet is plain 0/1/2
                let (rider, blob, sflip) = match p.facing {
                    Facing::Down => (0.0, 0.0, Flip::None),
                    Facing::Up => (2.0, 1.0, Flip::None),
                    Facing::Left => (4.0, 2.0, Flip::None),
                    Facing::Right => (4.0, 2.0, Flip::H),
                };
                let (bx, by) = to_target(px - 8.0, py - METATILE + sbob);
                out.offscreen.push(Quad {
                    tex: self.surf_blob_tex,
                    src: Rect::new(blob * 32.0, 0.0, 32.0, 32.0),
                    dst: Rect::new(bx, by, tile_px * 2.0, tile_px * 2.0),
                    layer: LAYER_PLAYER - 1,
                    tint: Rgba::WHITE,
                    flip: sflip,
                });
                let (sx, sy) = to_target(px - 8.0, py - METATILE - 6.0 + sbob);
                out.offscreen.push(Quad {
                    tex: self.surf_player_tex,
                    src: Rect::new(rider * 32.0, 0.0, 32.0, 32.0),
                    dst: Rect::new(sx, sy, tile_px * 2.0, tile_px * 2.0),
                    layer: LAYER_PLAYER,
                    tint: Rgba::WHITE,
                    flip: sflip,
                });
            }
            Avatar::Bike { .. } => {
                // 32x32 frames, same stand/pedal layout as the walking sheet;
                // levitating keeps the bike, standing pose, hovering bob
                let frame = if !levitating && p.anim_walking() {
                    walk[p.step_loop as usize & 1]
                } else {
                    stand
                };
                let (sx, sy) = to_target(px - 8.0, py - METATILE - bob);
                out.offscreen.push(Quad {
                    tex: self.bike_tex,
                    src: Rect::new(frame as f32 * 32.0, 0.0, 32.0, 32.0),
                    dst: Rect::new(sx, sy, tile_px * 2.0, tile_px * 2.0),
                    layer: LAYER_PLAYER,
                    tint: Rgba::WHITE,
                    flip,
                });
            }
            _ => {
                let running = !levitating && p.anim_running() && p.avatar == Avatar::Walking;
                let frame = if !levitating && p.anim_walking() {
                    walk[p.step_loop as usize & 1]
                } else {
                    stand
                };
                let row = if running { 32.0 } else { 0.0 };
                let (sx, sy) = to_target(px, py - METATILE - bob);
                out.offscreen.push(Quad {
                    tex: self.player_tex,
                    src: Rect::new(frame as f32 * 16.0, row, 16.0, 32.0),
                    dst: Rect::new(sx, sy, tile_px, tile_px * 2.0),
                    layer: LAYER_PLAYER,
                    tint: Rgba::WHITE,
                    flip,
                });
            }
        }

        // door animation frames over their tile (16x32: the door + above)
        if let Some((dx, dy, frame)) = match self.curr.transition {
            Some(Transition::DoorOpen { x, y, t }) => {
                Some((x, y, (t / 4) as i32 - 1)) // -1 = still closed
            }
            Some(Transition::DoorWait { x, y }) => Some((x, y, 2)),
            // while fading out on the door tile, the door stays open under
            // the player until the warp actually happens
            Some(Transition::FadeOut { door: true, .. }) => {
                Some((self.curr.player.x, self.curr.player.y, 2))
            }
            Some(Transition::ExitWalk { door_xy: Some((x, y)) }) => Some((x, y, 2)),
            Some(Transition::DoorClose { x, y, t }) => Some((x, y, 2 - (t / 4) as i32)),
            _ => None,
        } {
            if frame >= 0 {
                if let Some(di) = self.graph.door_at(self.curr.realm, dx, dy) {
                    let (sx, sy) =
                        to_target(dx as f32 * METATILE, (dy - 1) as f32 * METATILE);
                    out.offscreen.push(Quad {
                        tex: self.door_tex[di as usize],
                        src: Rect::new(0.0, frame.min(2) as f32 * 32.0, 16.0, 32.0),
                        dst: Rect::new(sx, sy, tile_px, tile_px * 2.0),
                        // under the player: they walk visibly into the open
                        // doorway, as on the GBA
                        layer: LAYER_PLAYER - 2,
                        tint: Rgba::WHITE,
                        flip: Flip::None,
                    });
                }
            }
        }

        // ── screen pass: composite the target at the continuous zoom ─────
        let zw = tw * cam.zoom / scale;
        let zh = th * cam.zoom / scale;
        // snap the composite to whole pixels: fractional offsets make linear
        // sampling blur unevenly (the "weird AA"), and at integer zoom this
        // snap makes the blit exactly 1:1 pixel-perfect
        out.screen.push(Quad {
            tex: TextureId::TARGET,
            src: Rect::new(0.0, 0.0, tw, th),
            dst: Rect::new(
                ((screen_w - zw) / 2.0).round(),
                ((screen_h - zh) / 2.0).round(),
                zw,
                zh,
            ),
            layer: 0,
            tint: Rgba::WHITE,
            flip: Flip::None,
        });

        // warp fade: a black overlay over everything (including the menu)
        if let Some(a) = match self.curr.transition {
            Some(Transition::FadeOut { t, .. }) => Some(t as f32 / FADE_T as f32),
            Some(Transition::FadeIn { t, .. }) => Some(1.0 - t as f32 / FADE_T as f32),
            _ => None,
        } {
            out.screen.push(Quad {
                tex: self.font_tex,
                src: Rect::new(95.0 * 16.0 + 4.0, 4.0, 8.0, 8.0),
                dst: Rect::new(0.0, 0.0, screen_w, screen_h),
                layer: 200,
                tint: Rgba { r: 0, g: 0, b: 0, a: (a.clamp(0.0, 1.0) * 255.0) as u8 },
                flip: Flip::None,
            });
        }

        // start menu — screen space at an integer UI scale, Emerald's font
        if let Some(cursor) = self.curr.menu {
            let ui = ui::Ui {
                tex: self.font_tex,
                widths: &self.font_widths,
                scale: ui::Ui::scale_for(screen_w, screen_h),
            };
            let levitate_label =
                if self.curr.levitate { "LEVITATE: ON" } else { "LEVITATE: OFF" };
            let z = self.curr.cam.zoom;
            let zoom_label = if z.fract() == 0.0 {
                format!("ZOOM: {z:.0}x")
            } else {
                format!("ZOOM: {z:.1}x")
            };
            let items = [
                (levitate_label, true),
                (zoom_label.as_str(), true),
                ("SAVE", true),
                ("LOAD", self.snapshot.is_some()),
                ("EXIT", true),
            ];
            ui.menu(&mut out.screen, &items, cursor, screen_w);
        }
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::*;

    /// A miniature but complete pack: one tileset (2 tiles, 1 palette), two
    /// overworld maps sharing a layout, a house realm, a door warp at (5, 5)
    /// and an exit mat, matching the synthetic logic-test world.
    fn test_pack() -> WorldPack {
        let mut tiles_4bpp = vec![0x11u8; 32]; // tile 0: all color 1
        tiles_4bpp.extend([0x22u8; 32]); // tile 1: all color 2
        let mut palettes = [[0u8; 4]; 256];
        palettes[1] = [40, 200, 40, 255];
        palettes[2] = [200, 200, 40, 255];
        // metatiles: 0 land, 1 floor (covered top), 2 water, 3 door
        let mut metatiles = vec![0u16; 8];
        metatiles[4..8].copy_from_slice(&[0x3FF; 4]);
        metatiles.extend([1u16; 8]); // 1: floor
        metatiles.extend([0u16; 8]); // 2: water (looks like land, behaves wet)
        metatiles.extend([1u16; 8]); // 3: door
        metatiles.extend([1u16; 8]); // 4: interior exit (south-arrow warp)
        let ts = TilesetData {
            name: "gTileset_Fixture".into(),
            is_secondary: false,
            tiles_4bpp,
            palettes,
            metatiles,
            attributes: vec![0, 1 << 12, 0x10, 0, 0x11],
        };
        // land at elevation 3; the eastern strip x>=25 is water at elevation
        // 1 (surfable behavior 0x10); the door tile gets stamped after
        let blockdata = |w: u32, h: u32, mt: u16| -> Vec<u16> {
            vec![(3 << 12) | mt; (w * h) as usize]
        };
        WorldPack {
            tilesets: vec![ts],
            pairs: vec![(0, 0)],
            layouts: vec![
                LayoutData {
                    id: "LAYOUT_HALF".into(),
                    width: 30,
                    height: 40,
                    pair: 0,
                    blockdata: {
                        let mut b = blockdata(30, 40, 0);
                        for y in 0..40 {
                            for x in 25..30 {
                                b[y * 30 + x] = (1 << 12) | 2; // water, elevation 1
                            }
                        }
                        b[5 * 30 + 5] = 3; // the animated door (elevation 0)
                        b
                    },
                },
                LayoutData {
                    id: "LAYOUT_HOUSE".into(),
                    width: 9,
                    height: 8,
                    pair: 0,
                    blockdata: {
                        let mut b = blockdata(9, 8, 1);
                        b[7 * 9 + 4] = (3 << 12) | 4; // the exit arrow tile
                        b
                    },
                },
            ],
            realms: vec![
                RealmData {
                    width: 60,
                    height: 40,
                    maps: vec![
                        MapData {
                            id: "MAP_WEST".into(),
                            music: "MUS_W".into(),
                            layout: 0,
                            x: 0,
                            y: 0,
                            warps: vec![WarpData {
                                x: 5,
                                y: 5,
                                to_realm: 1,
                                to_x: 4,
                                to_y: 7,
                            }],
                        },
                        MapData {
                            id: "MAP_EAST".into(),
                            music: "MUS_E".into(),
                            layout: 0,
                            x: 30,
                            y: 0,
                            warps: vec![],
                        },
                    ],
                },
                RealmData {
                    width: 9,
                    height: 8,
                    maps: vec![MapData {
                        id: "MAP_HOUSE".into(),
                        music: "MUS_W".into(),
                        layout: 1,
                        x: 0,
                        y: 0,
                        warps: vec![WarpData { x: 4, y: 7, to_realm: 0, to_x: 5, to_y: 5 }],
                    }],
                },
            ],
            player: Image { rgba: vec![128; 144 * 64 * 4], w: 144, h: 64 },
            font: Image { rgba: vec![200; 96 * 16 * 16 * 4], w: 96 * 16, h: 16 },
            font_widths: vec![6; 96],
            surf_player: Image { rgba: vec![1; 192 * 32 * 4], w: 192, h: 32 },
            surf_blob: Image { rgba: vec![2; 96 * 32 * 4], w: 96, h: 32 },
            bike: Image { rgba: vec![3; 288 * 32 * 4], w: 288, h: 32 },
            doors: vec![DoorData {
                tileset: 0,
                metatile: 3,
                frames: Image { rgba: vec![4; 16 * 96 * 4], w: 16, h: 96 },
            }],
            surfable: {
                let mut m = [0u8; 32];
                m[0x10 / 8] |= 1 << (0x10 % 8); // fixture water behavior 0x10
                m
            },
            warp_dir: {
                let mut m = [0u8; 256];
                m[0x11] = 1; // behavior 0x11: south-arrow warp (interior exits)
                m
            },
            start: Start { realm: 0, x: 10, y: 10 },
        }
    }

    fn test_world() -> World {
        let bytes = test_pack().to_bytes();
        let mut next = 0u32;
        World::new(&bytes, &mut |_, _, _| {
            next += 1;
            TextureId(next - 1)
        })
        .unwrap()
    }

    #[test]
    fn fixed_step_yields_sim_rate() {
        let mut fs = FixedStep::new();
        let mut steps = 0;
        // one second of 60 Hz render frames → ~59.7275 sim steps
        for _ in 0..60 {
            let (n, alpha) = fs.advance(1.0 / 60.0);
            steps += n;
            assert!((0.0..1.0).contains(&alpha));
        }
        assert!(steps == 59 || steps == 60, "got {steps}");
    }

    #[test]
    fn render_interpolates_between_sim_states() {
        let mut world = test_world();
        // get the player mid-glide: one step starts the move
        world.step(Input { down: true, ..Default::default() });
        world.step(Input { down: true, ..Default::default() });
        let player_quad = |alpha: f32| {
            let mut f = Frame::default();
            world.frame(alpha, 240.0, 160.0, &mut f);
            // camera is locked to the player, so read a world tile instead:
            // its dst shifts opposite to the camera between alphas
            f.offscreen[0].dst
        };
        let a0 = player_quad(0.0);
        let a1 = player_quad(1.0);
        let mid = player_quad(0.5);
        assert_ne!(a0.y, a1.y, "a glide must move the view between sim states");
        let expect = (a0.y + a1.y) / 2.0;
        assert!((mid.y - expect).abs() < 1e-3, "alpha=0.5 lies halfway");
    }

    #[test]
    fn sim_is_deterministic_for_a_given_input_sequence() {
        let script = |world: &mut World| {
            for i in 0..200u32 {
                let input = Input {
                    right: i % 3 != 0,
                    down: i % 5 == 0,
                    zoom_in: i % 7 == 0,
                    ..Default::default()
                };
                world.step(input);
            }
        };
        let mut a = test_world();
        let mut b = test_world();
        script(&mut a);
        script(&mut b);
        assert_eq!(a.curr, b.curr, "fixed-step sim must be deterministic");
    }

    #[test]
    fn max_catchup_is_bounded() {
        let mut fs = FixedStep::new();
        let (steps, alpha) = fs.advance(10.0); // huge hitch (breakpoint, bg tab)
        assert_eq!(steps, FixedStep::MAX_STEPS, "hitches must not spiral");
        assert_eq!(alpha, 0.0, "leftover time is dropped after a hitch");
    }

    #[test]
    fn frame_is_pure_and_culled() {
        let mut world = test_world();
        world.step(Input { right: true, ..Default::default() });
        let mut a = Frame::default();
        let mut b = Frame::default();
        world.frame(0.5, 240.0, 160.0, &mut a);
        world.frame(0.5, 240.0, 160.0, &mut b);
        assert_eq!(a, b, "frame() must be pure");
        let ow = world.graph.realm(map::OVERWORLD);
        let map_tiles = (ow.width * ow.height) as usize;
        assert!(a.offscreen.len() < map_tiles, "culling must bound quads");
        // screen pass: just the composited target while the menu is closed
        assert_eq!(a.screen.len(), 1);
        assert_eq!(a.screen[0].tex, TextureId::TARGET);
    }

    #[test]
    fn step_interpolation_state() {
        let mut world = test_world();
        let before = world.curr;
        world.step(Input { right: true, ..Default::default() });
        assert_eq!(world.prev, before, "prev must hold the pre-step state");
        assert_ne!(world.curr.player, before.player);
    }

    #[test]
    fn zoom_is_continuous_crisp_and_leaves_ui_alone() {
        let mut world = test_world();
        let mut f = Frame::default();
        world.frame(1.0, 240.0, 160.0, &mut f);

        // hold zoom_in: the visible world extent (target px / integer scale)
        // must shrink continuously — no fixed steps — while offscreen tiles
        // stay at a crisp integer scale
        let mut last_extent = f32::INFINITY;
        for _ in 0..30 {
            world.step(Input { zoom_in: true, ..Default::default() });
            world.frame(1.0, 240.0, 160.0, &mut f);
            let zoom = world.curr.cam.zoom;
            let scale = zoom.ceil();
            let extent = f.target_size.0 as f32 / scale;
            // the target is quantized up to 64px steps (render-target reuse);
            // the visible extent still tracks 240/zoom within that margin
            let want = 240.0 / zoom;
            assert!(
                extent >= want - 0.001 && extent <= want + 64.0 / scale + 1.0,
                "extent tracks 240/zoom"
            );
            assert!(extent <= last_extent + 0.001, "zoom in never grows the extent");
            last_extent = extent;
            let tile = f.offscreen[0].dst.w;
            assert_eq!(tile, METATILE * scale, "offscreen tiles keep integer scale");
            // the composited target covers the screen at the continuous zoom
            let target = &f.screen[0];
            assert_eq!(target.tex, TextureId::TARGET);
            assert!(target.dst.w >= 240.0);
            assert!(target.dst.w <= 240.0 + (64.0 + 1.0) * zoom / scale + zoom + 1.0);
        }
        // the menu is screen-space: its quads are identical at any zoom
        world.step(Input { start: true, ..Default::default() });
        world.step(Input::default());
        let mut a = Frame::default();
        world.frame(1.0, 240.0, 160.0, &mut a);
        let menu_a: Vec<Quad> = a.screen[1..].to_vec();
        for _ in 0..20 {
            world.step(Input { zoom_out: true, ..Default::default() });
        }
        let mut b = Frame::default();
        world.frame(1.0, 240.0, 160.0, &mut b);
        assert!(!menu_a.is_empty(), "open menu must emit UI quads");
        assert_eq!(menu_a, b.screen[1..].to_vec(), "UI quads stay screen-space");
    }

    #[test]
    fn zoom_is_clamped() {
        let mut world = test_world();
        for _ in 0..2000 {
            world.step(Input { zoom_in: true, ..Default::default() });
        }
        assert_eq!(world.curr.cam.zoom, ZOOM_MAX);
        for _ in 0..2000 {
            world.step(Input { zoom_out: true, ..Default::default() });
        }
        assert_eq!(world.curr.cam.zoom, ZOOM_MIN);
    }

    #[test]
    fn warp_switches_realm_and_never_interpolates_across() {
        let mut world = test_world();
        // steer the player just below the door, then walk up into it
        let mut warped = false;
        for _ in 0..500 {
            let p = &world.state().player;
            let input = if p.y > 6 {
                Input { up: true, ..Default::default() }
            } else if p.x > 5 {
                Input { left: true, ..Default::default() }
            } else {
                Input { up: true, ..Default::default() }
            };
            let ev = world.step(input);
            if ev.warp.is_some() || world.curr.transition.is_some() {
                warped = true;
                break;
            }
        }
        assert!(warped, "route to the door must fire the warp");
        // the warp now runs the fade choreography: realm switches mid-fade
        // full door choreography: open 16 + walk in 16 + fade 12+12 + the
        // arrival walk off the mat
        let mut fade_steps = 0;
        while world.curr.transition.is_some() {
            world.step(Input::default());
            fade_steps += 1;
            assert!(fade_steps < 150, "transition must finish");
            if world.curr.realm == 1 {
                assert_eq!(world.prev.realm, 1, "no interpolation across a warp");
            }
        }
        assert!(fade_steps >= 40, "the choreography takes real frames");
        assert_eq!(world.curr.realm, 1, "active realm switched to the interior");
        // arrivals stand exactly where the warp put them (no auto-step)
        assert_eq!((world.curr.player.x, world.curr.player.y), (4, 7));
        // rendering now draws the interior realm, bounded by its small size
        let mut f = Frame::default();
        world.frame(1.0, 240.0, 160.0, &mut f);
        let interior = world.graph.realm(1);
        // house metatile has bottom + covered top = up to 2 quads per cell
        assert!(f.offscreen.len() <= (interior.width * interior.height * 2 + 1) as usize);
        // covered tops must sit below the player layer
        assert!(f
            .offscreen
            .iter()
            .filter(|q| q.layer == LAYER_BG_TOP)
            .all(|q| q.layer < LAYER_PLAYER));
    }

    #[test]
    fn camera_follows_the_player_glide() {
        let mut world = test_world();
        for _ in 0..12 {
            world.step(Input { down: true, ..Default::default() });
        }
        let (px, py) = world.curr.player.world_px();
        assert_eq!(world.curr.cam.x, px + METATILE / 2.0);
        assert_eq!(world.curr.cam.y, py + METATILE / 2.0);
    }

    #[test]
    fn save_load_roundtrips_and_continues_deterministically() {
        let mut a = test_world();
        // wander into a mid-glide, mid-zoom state
        for i in 0..37u32 {
            a.step(Input { right: true, zoom_in: i % 2 == 0, ..Default::default() });
        }
        let save = a.save_state();
        // transient fields (menu/ticks/prev input) are not saved; the saved
        // portion must roundtrip byte-exactly
        assert_eq!(State::from_bytes(&save).unwrap().to_bytes(), save, "roundtrip");

        // diverge a, then restore into a fresh world
        for _ in 0..50 {
            a.step(Input { up: true, ..Default::default() });
        }
        let mut b = test_world();
        b.load_state(&save).unwrap();
        assert_eq!(b.prev, b.curr, "a load never interpolates");

        // the restored sim continues exactly like the original would have
        let mut a2 = test_world();
        a2.load_state(&save).unwrap();
        for _ in 0..50 {
            a2.step(Input { down: true, ..Default::default() });
            b.step(Input { down: true, ..Default::default() });
        }
        assert_eq!(a2.curr, b.curr);

        // corrupt saves are rejected
        assert!(State::from_bytes(&save[..10]).is_err());
        let mut evil = save.clone();
        evil[3] = 0xFF;
        evil[4] = 0xFF; // realm 65535
        assert!(b.load_state(&evil).is_err());
    }

    #[test]
    fn b_runs_at_prets_running_speed_with_the_running_row() {
        let mut world = test_world();
        // face down (spawn facing) and hold B+down: 32 held steps = 4 tiles
        // at pret's running speed (8 frames per tile, chained gaplessly)
        let run = Input { down: true, b: true, ..Default::default() };
        let (_, py0) = world.curr.player.world_px();
        world.step(run);
        assert!(world.curr.player.anim_running());
        let mut f = Frame::default();
        world.frame(1.0, 240.0, 160.0, &mut f);
        let player_quad = f.offscreen.iter().find(|q| q.src.h == 32.0).unwrap();
        assert_eq!(player_quad.src.y, 32.0, "running uses the second sprite row");
        for _ in 0..31 {
            world.step(run);
        }
        let (_, py1) = world.curr.player.world_px();
        assert_eq!(py1 - py0, 64.0, "running: 8 frames per tile, chained");
        while world.curr.player.motion != player::Motion::Idle {
            world.step(Input::default());
        }

        // walking: 32 held steps = 32 px = 2 tiles (16 frames per tile)
        let walk = Input { down: true, ..Default::default() };
        let (_, py0) = world.curr.player.world_px();
        for _ in 0..32 {
            world.step(walk);
        }
        let (_, py1) = world.curr.player.world_px();
        assert_eq!(py1 - py0, 32.0, "walking: 16 frames per tile, chained");
    }

    #[test]
    fn start_menu_opens_navigates_and_toggles_levitate() {
        let mut world = test_world();
        let press = |w: &mut World, input: Input| {
            w.step(input);
            w.step(Input::default()); // release
        };
        press(&mut world, Input { start: true, ..Default::default() });
        assert_eq!(world.curr.menu, Some(0), "Enter opens the menu");

        // movement is frozen while the menu is open (Emerald behavior);
        // left/right don't move the cursor, so they prove it cleanly
        let (x0, y0) = (world.curr.player.x, world.curr.player.y);
        for _ in 0..20 {
            world.step(Input { right: true, ..Default::default() });
        }
        assert_eq!((world.curr.player.x, world.curr.player.y), (x0, y0));
        assert_eq!(world.curr.menu, Some(0));
        world.step(Input::default());

        // A on LEVITATE toggles it and keeps the menu open
        press(&mut world, Input { a: true, ..Default::default() });
        assert!(world.curr.levitate);
        assert_eq!(world.curr.menu, Some(0));

        // cursor wraps; B closes
        press(&mut world, Input { up: true, ..Default::default() });
        assert_eq!(world.curr.menu, Some(4));
        press(&mut world, Input { b: true, ..Default::default() });
        assert_eq!(world.curr.menu, None);

        // levitate: straight through the solid boulder at (20, 10) — walk
        // right from (19, 10) — and the sprite bobs instead of walking
        world.curr.player.x = 19;
        world.curr.player.y = 10;
        world.curr.player.facing = player::Facing::Right;
        world.prev = world.curr;
        for _ in 0..40 {
            world.step(Input { right: true, ..Default::default() });
        }
        assert!(world.curr.player.x > 20, "levitation walks through collision");
        let mut f = Frame::default();
        world.frame(1.0, 240.0, 160.0, &mut f);
        let pq = f.offscreen.iter().find(|q| q.src.h == 32.0).unwrap();
        assert_eq!(pq.src.y, 0.0, "levitating never uses the running row");

        // menu SAVE then LOAD restores the snapshot position
        press(&mut world, Input { start: true, ..Default::default() });
        press(&mut world, Input { down: true, ..Default::default() });
        press(&mut world, Input { down: true, ..Default::default() });
        press(&mut world, Input { a: true, ..Default::default() }); // SAVE closes
        let saved = (world.curr.player.x, world.curr.player.y);
        for _ in 0..60 {
            world.step(Input { down: true, ..Default::default() });
        }
        assert_ne!((world.curr.player.x, world.curr.player.y), saved);
        press(&mut world, Input { start: true, ..Default::default() });
        for _ in 0..3 {
            press(&mut world, Input { down: true, ..Default::default() });
        }
        press(&mut world, Input { a: true, ..Default::default() }); // LOAD
        assert_eq!((world.curr.player.x, world.curr.player.y), saved);
        assert!(world.curr.levitate, "levitate survives save/load");
    }

    #[test]
    fn water_blocks_walkers_but_surfing_works_end_to_end() {
        let mut world = test_world();
        // stand just west of the water strip at x=25 and face it
        world.curr.player.x = 24;
        world.curr.player.y = 10;
        world.curr.player.facing = player::Facing::Right;
        world.prev = world.curr;

        // walking into water: blocked (elevation 1 vs walker at 3)
        for _ in 0..30 {
            world.step(Input { right: true, ..Default::default() });
        }
        assert_eq!(world.curr.player.x, 24, "water must block walkers");

        // A mounts the blob and hops on; MUS_SURF overrides the map music
        let ev = world.step(Input { a: true, ..Default::default() });
        assert_eq!(ev.sfx, Some("se_m_surf"));
        assert_eq!(world.curr.player.avatar, Avatar::Surfing);
        assert_eq!(world.current_music(), Some("MUS_SURF"));
        for _ in 0..20 {
            world.step(Input::default());
        }
        assert_eq!(world.curr.player.x, 25, "mounted onto the water");
        assert_eq!(world.curr.player.elevation, 1);

        // surfing moves at 8 frames per tile
        let (px0, _) = world.curr.player.world_px();
        for _ in 0..16 {
            world.step(Input { right: true, ..Default::default() });
        }
        let (px1, _) = world.curr.player.world_px();
        assert_eq!(px1 - px0, 32.0, "surf speed = 2 px per frame");

        // heading back to shore dismounts automatically
        for _ in 0..60 {
            world.step(Input { left: true, ..Default::default() });
        }
        assert_eq!(world.curr.player.avatar, Avatar::Walking);
        assert!(world.curr.player.x < 25);
        assert_eq!(world.curr.player.elevation, 3);
    }

    #[test]
    fn mach_bike_accelerates_and_coasts_like_the_real_one() {
        let mut world = test_world();
        // Select toggles the bike; MUS_CYCLING overrides
        world.step(Input { select: true, ..Default::default() });
        assert!(matches!(world.curr.player.avatar, Avatar::Bike { tier: 0, .. }));
        assert_eq!(world.current_music(), Some("MUS_CYCLING"));
        world.step(Input::default());

        // tile 1 at 16 frames, tile 2 at 8, tile 3+ at 4 (pret's three tiers)
        let hold = Input { down: true, ..Default::default() };
        let y0 = world.curr.player.world_px().1;
        for _ in 0..16 {
            world.step(hold);
        }
        assert_eq!(world.curr.player.world_px().1 - y0, 16.0, "tier 0 = walk speed");
        for _ in 0..8 {
            world.step(hold);
        }
        assert_eq!(world.curr.player.world_px().1 - y0, 32.0, "tier 1 = 2 px/frame");
        for _ in 0..4 {
            world.step(hold);
        }
        assert_eq!(world.curr.player.world_px().1 - y0, 48.0, "tier 2 = 4 px/frame");

        // release from top speed: the in-flight tile finishes, then pret's
        // bikeSpeed momentum coasts 3 more tiles at 4, 2, 1 px per frame
        let mut coasted = 0.0;
        let y1 = world.curr.player.world_px().1;
        for _ in 0..80 {
            world.step(Input::default());
            if world.curr.player.motion == player::Motion::Idle {
                coasted = world.curr.player.world_px().1 - y1;
                break;
            }
        }
        assert_eq!(coasted, 64.0, "coasts down through the tiers");
        assert!(matches!(world.curr.player.avatar, Avatar::Bike { tier: 0, .. }));

        // a single tap moves exactly one tile — momentum is still 0
        world.step(Input { down: true, ..Default::default() });
        let y2 = world.curr.player.world_px().1;
        let mut steps = 0;
        while world.curr.player.motion != player::Motion::Idle {
            world.step(Input::default());
            steps += 1;
            assert!(steps < 40);
        }
        assert_eq!(world.curr.player.world_px().1 - y2, 15.0, "tap = one tile, no coast");

        world.step(Input { select: true, ..Default::default() });
        assert_eq!(world.curr.player.avatar, Avatar::Walking);

        // Select works mid-stride: toggling while a tile is in flight
        // switches the avatar without interrupting the move
        world.step(Input { down: true, ..Default::default() });
        assert_ne!(world.curr.player.motion, player::Motion::Idle);
        world.step(Input { down: true, select: true, ..Default::default() });
        assert!(matches!(world.curr.player.avatar, Avatar::Bike { .. }));
        assert_ne!(world.curr.player.motion, player::Motion::Idle);
    }

    #[test]
    fn door_roundtrip_matches_the_real_games_choreography() {
        let mut world = test_world();
        // inside the house after the door entry (same route as the warp test)
        world.curr.player.x = 5;
        world.curr.player.y = 6;
        world.curr.player.facing = player::Facing::Up;
        world.prev = world.curr;
        let ev = world.step(Input { up: true, ..Default::default() });
        assert_eq!(ev.door_enter, Some((5, 5)), "walking up into the door");
        assert_eq!(ev.sfx, Some("se_door"));
        assert!(matches!(world.curr.transition, Some(Transition::DoorOpen { .. })));
        let mut n = 0;
        while world.curr.transition.is_some() {
            world.step(Input::default());
            n += 1;
            assert!(n < 150);
        }
        assert_eq!(world.curr.realm, 1);
        assert_eq!((world.curr.player.x, world.curr.player.y), (4, 7), "standing on the exit");

        // walking around the interior — including BACK ONTO the exit tile —
        // must not warp; only pressing down while standing on it does
        // 15 held steps + release: exactly one tile (holding through the
        // boundary would chain into a second)
        for _ in 0..15 {
            world.step(Input { up: true, ..Default::default() });
        }
        for _ in 0..3 {
            world.step(Input::default());
        }
        assert_eq!((world.curr.player.x, world.curr.player.y), (4, 6));
        assert!(world.curr.transition.is_none());
        // hold down: walk onto the exit tile and keep pressing — the arrow
        // warp fires from the continued press, fading back outside
        let mut out_sfx = false;
        for _ in 0..240 {
            let ev = world.step(Input { down: true, ..Default::default() });
            if ev.sfx == Some("se_exit") {
                out_sfx = true;
            }
            if world.curr.realm == 0 && world.curr.transition.is_none() {
                break;
            }
        }
        assert!(out_sfx, "leaving plays the exit sound");
        assert_eq!(world.curr.realm, 0);
        assert_eq!(
            (world.curr.player.x, world.curr.player.y),
            (5, 6),
            "auto-stepped down off the door"
        );

        // levitate skips all of it: instant warp, no transition
        world.curr.levitate = true;
        world.prev = world.curr;
        for _ in 0..40 {
            world.step(Input { up: true, ..Default::default() });
            assert!(world.curr.transition.is_none(), "levitate warps stay instant");
            if world.curr.realm == 1 {
                break;
            }
        }
        assert_eq!(world.curr.realm, 1, "levitate walked through the door tile");

        // and levitate still EXITS: the arrow warp fires instantly too
        for _ in 0..80 {
            world.step(Input { down: true, ..Default::default() });
            assert!(world.curr.transition.is_none(), "levitate warps stay instant");
            if world.curr.realm == 0 {
                break;
            }
        }
        assert_eq!(world.curr.realm, 0, "levitate exits through the arrow tile");
    }

    #[test]
    fn bake_dedupes_and_renders_the_fixture() {
        let pack = test_pack();
        let baked = bake::bake(&pack);
        assert_eq!(baked.atlases.len(), 1);
        let d0 = baked.draw(0, 0);
        assert_ne!(d0.bottom, bake::CellRef::NONE);
        assert_eq!(d0.top, bake::CellRef::NONE, "out-of-range top tile is empty");
        let d1 = baked.draw(0, 1);
        assert!(!d1.top_above_player, "layer type 1 (covered) stays below the player");
        // atlas pixel of metatile 0 bottom = palette color 1
        let src = d0.bottom.src();
        let at = ((src.y as u32 * bake::ATLAS_SIZE + src.x as u32) * 4) as usize;
        assert_eq!(&baked.atlases[0].rgba[at..at + 4], &[40, 200, 40, 255]);
    }
}
