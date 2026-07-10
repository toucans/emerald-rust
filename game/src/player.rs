//! Player movement, ported from pokeemerald_SDL3's player.c + camera.c and
//! re-expressed for the fixed-step sim:
//!
//! - Tile-by-tile movement on the overworld grid. Pressing a direction the
//!   player isn't facing turns first (8 frames); pressing the faced direction
//!   commits a one-tile move. The tile coordinate updates at move START (as in
//!   the C), and the visual glide covers the remaining pixels over the
//!   following steps.
//! - The C slides the *camera* 16 px at CAMERA_SPEED (360 px/s) while the
//!   player sprite stays screen-centered. With our `{pos, zoom}` camera the
//!   equivalent — and interpolation-friendly — shape is: the player's world
//!   position glides between tiles and the camera stays locked on the player.
//!   Same speed, same feel.
//! - `stepLoop` alternates which walk frame the next step uses, as in the C.
//! - Collision comes from pret's mapgrid collision bits via
//!   `Realm::passable` (the C project didn't have collision yet; the real
//!   data demanded it).

use crate::map::{MapIdx, Realm, RealmGraph, RealmId, Warp, METATILE};
use types::Input;

/// pret MOVE_SPEED_NORMAL: 1 px per frame, 16 frames per tile.
pub const WALK_PX_PER_STEP: f32 = 1.0;
/// pret MOVE_SPEED_FAST_1 (running shoes): 2 px per frame, 8 frames per tile.
pub const RUN_PX_PER_STEP: f32 = 2.0;
/// pret PLAYER_SPEED_FASTEST: the Mach bike's top tier, 4 frames per tile.
pub const BIKE_TOP_PX_PER_STEP: f32 = 4.0;
/// C: TURN_ANIMATION_DURATION 8/60 s → 8 frames.
pub const TURN_STEPS: u8 = 8;

/// What the player is riding. The Mach bike carries its speed tier
/// (bikeFrameCounter in pret): 1 → 2 → 4 px per frame, climbing one tier per
/// tile while pedaling and coasting back down when the pad is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Avatar {
    Walking,
    Surfing,
    /// `tier` is pret's bikeFrameCounter (selects the speed); `momentum` is
    /// its bikeSpeed (tier + tier/2) — how many tiles the bike coasts after
    /// the pad is released, one tier down per tile. A single tapped tile
    /// leaves momentum at 0: stop dead, exactly like the hardware.
    Bike { tier: u8, momentum: u8 },
}

const BIKE_SPEEDS: [f32; 3] = [WALK_PX_PER_STEP, RUN_PX_PER_STEP, BIKE_TOP_PX_PER_STEP];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Facing {
    Up,
    Down,
    Left,
    Right,
}

impl Facing {
    pub fn delta(self) -> (i32, i32) {
        match self {
            Facing::Up => (0, -1),
            Facing::Down => (0, 1),
            Facing::Left => (-1, 0),
            Facing::Right => (1, 0),
        }
    }
}

/// PLAYER_IDLE / PLAYER_TURNING / PLAYER_MOVING from the C, with the per-state
/// bookkeeping (anim timer, camera target) folded into the variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Idle,
    Turning { steps_left: u8 },
    /// `remaining_px` counts 16→0 along the current facing. `speed` is locked
    /// at move start (Emerald keeps a tile's speed even if B is released).
    Moving { remaining_px: f32, speed: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Player {
    /// Overworld tile coords (the C's player.x/y): updated at move START.
    pub x: i32,
    pub y: i32,
    pub facing: Facing,
    pub motion: Motion,
    /// Alternates each completed step/turn; picks walk frame 1 vs 2.
    pub step_loop: u8,
    pub avatar: Avatar,
    /// pret's currentElevation: updated from each tile stood on (except
    /// transition/bridge tiles); mismatches block movement — this is what
    /// keeps walkers off water (1) and lets surfers stay on it.
    pub elevation: u8,
    /// The map the player is standing in (None = outside any map, the C's
    /// `outside_map` + `currentMap` pair).
    pub current_map: Option<MapIdx>,
}

/// Fired by the sim; consumed by `World::step` (warps) and the adapter/audio
/// layer (music changes).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StepEvents {
    /// Player crossed into a different map of the active realm this step
    /// (the C prints + switches music here).
    pub entered_map: Option<MapIdx>,
    /// Player finished a step onto a warp tile: switch realms.
    pub warp: Option<Warp>,
    /// Player pushed against an animated door with a warp: the door-open
    /// sequence should run instead of a plain move.
    pub door_enter: Option<(i32, i32)>,
    /// A sound effect to play (se_* song name).
    pub sfx: Option<&'static str>,
}

impl Player {
    pub fn spawn(x: i32, y: i32, realm: &Realm) -> Self {
        Self {
            x,
            y,
            facing: Facing::Down,
            motion: Motion::Idle,
            step_loop: 0,
            avatar: Avatar::Walking,
            elevation: 3,
            current_map: realm.map_at(x, y),
        }
    }

    /// World-pixel position of the player's tile top-left, mid-glide included.
    pub fn world_px(&self) -> (f32, f32) {
        let (tx, ty) = (self.x as f32 * METATILE, self.y as f32 * METATILE);
        match self.motion {
            Motion::Moving { remaining_px, .. } => {
                let (dx, dy) = self.facing.delta();
                (tx - dx as f32 * remaining_px, ty - dy as f32 * remaining_px)
            }
            _ => (tx, ty),
        }
    }

    /// Can the player step onto (nx, ny)? pret's rules: collision bits, the
    /// elevation match (transition 0 / bridge 15 always pass), and the
    /// surfable-behavior split between walkers and surfers.
    fn can_enter(&self, g: &RealmGraph, realm: RealmId, nx: i32, ny: i32, levitate: bool) -> bool {
        let r = g.realm(realm);
        if !r.in_bounds(nx, ny) {
            return false;
        }
        if levitate {
            return true;
        }
        let Some(cell) = r.cell(nx, ny) else {
            return true; // stitched-overworld gaps stay traversable void
        };
        if cell.collision != 0 {
            return false;
        }
        let surf = g.surfable(cell);
        if (self.avatar == Avatar::Surfing) != surf {
            // dismounting (surf → land) is allowed; boarding water on foot is
            // not — the elevation check below also enforces it, but dismount
            // must bypass that check
            return self.avatar == Avatar::Surfing && !surf;
        }
        if cell.elevation != 0 && cell.elevation != 15 && self.elevation != 0 {
            return cell.elevation == self.elevation;
        }
        true
    }

    fn speed(&self, run: bool) -> f32 {
        match self.avatar {
            Avatar::Walking => {
                if run {
                    RUN_PX_PER_STEP
                } else {
                    WALK_PX_PER_STEP
                }
            }
            Avatar::Surfing => RUN_PX_PER_STEP,
            Avatar::Bike { tier, .. } => BIKE_SPEEDS[(tier as usize).min(2)],
        }
    }

    /// After a pedaled move begins: momentum = pret's bikeSpeed for the tier
    /// that chose this tile's speed, then the counter climbs.
    fn bike_accelerate(&mut self) {
        if let Avatar::Bike { tier, momentum } = &mut self.avatar {
            *momentum = *tier + *tier / 2;
            *tier = (*tier + 1).min(2);
        }
    }

    fn bike_still(&mut self) {
        if let Avatar::Bike { tier, momentum } = &mut self.avatar {
            *tier = 0;
            *momentum = 0;
        }
    }

    /// Begin a move onto (nx, ny), which the caller has validated. Handles
    /// the surf mount/dismount avatar flips.
    fn begin_move(
        &mut self,
        g: &RealmGraph,
        realm: RealmId,
        dir: Facing,
        run: bool,
        full: bool,
        pedaling: bool,
    ) {
        let (dx, dy) = dir.delta();
        let (nx, ny) = (self.x + dx, self.y + dy);
        self.facing = dir;
        if self.avatar == Avatar::Surfing && !g.surfable_at(realm, nx, ny) {
            self.avatar = Avatar::Walking; // step off the blob onto shore
        }
        let speed = self.speed(run);
        self.x = nx;
        self.y = ny;
        let remaining = if full { METATILE } else { METATILE - speed };
        self.motion = Motion::Moving { remaining_px: remaining, speed };
        if pedaling {
            self.bike_accelerate(); // coasting never climbs back up
        }
    }

    /// Forced single step (door walk-ins, warp arrivals): no collision, no
    /// chaining, walk speed.
    pub fn force_move(&mut self, dir: Facing) {
        let (dx, dy) = dir.delta();
        self.facing = dir;
        self.x += dx;
        self.y += dy;
        self.motion = Motion::Moving { remaining_px: METATILE, speed: WALK_PX_PER_STEP };
    }

    /// Mount the surf blob onto the faced water tile (caller validated).
    pub fn mount_surf(&mut self, g: &RealmGraph, realm: RealmId) {
        self.avatar = Avatar::Surfing;
        let (dx, dy) = self.facing.delta();
        if let Some(cell) = g.realm(realm).cell(self.x + dx, self.y + dy) {
            if cell.elevation != 0 && cell.elevation != 15 {
                self.elevation = cell.elevation;
            }
        }
        self.force_move(self.facing);
    }

    /// One fixed sim step of the player_update + camera_update pair.
    /// `run` = B held; `levitate` = walk through everything.
    pub fn step(
        &mut self,
        input: Input,
        g: &RealmGraph,
        realm: RealmId,
        run: bool,
        levitate: bool,
        events: &mut StepEvents,
    ) {
        match self.motion {
            Motion::Idle => {
                let Some(dir) = held_direction(input) else { return };
                if self.facing != dir {
                    self.facing = dir;
                    self.bike_still();
                    self.motion = Motion::Turning { steps_left: TURN_STEPS };
                } else if let Some(w) = self.arrow_warp(g, realm, dir) {
                    // arrow exits fire even in levitate mode (the warp is
                    // then applied instantly, without the fade)
                    events.warp = Some(w);
                } else {
                    let (dx, dy) = dir.delta();
                    let (nx, ny) = (self.x + dx, self.y + dy);
                    if !levitate
                        && g.realm(realm).warp_at(nx, ny).is_some()
                        && g.door_at(realm, nx, ny).is_some()
                    {
                        events.door_enter = Some((nx, ny));
                    } else if self.can_enter(g, realm, nx, ny, levitate) {
                        self.begin_move(g, realm, dir, run, false, true);
                    } else {
                        self.bike_still();
                    }
                }
            }
            Motion::Turning { steps_left } => {
                let left = steps_left - 1;
                self.motion = if left == 0 {
                    self.step_loop ^= 1;
                    Motion::Idle
                } else {
                    Motion::Turning { steps_left: left }
                };
            }
            Motion::Moving { remaining_px, speed } => {
                let left = remaining_px - speed;
                if left <= 0.0 {
                    self.motion = Motion::Idle;
                    self.step_loop ^= 1;
                    self.step_taken(g, realm, events);
                    // In motion, turning is instant (turn-in-place only
                    // happens from a standstill): a held direction chains
                    // straight into the next tile — gapless, as on the GBA.
                    // The Mach bike also COASTS: released pad = keep rolling
                    // forward, dropping one tier per tile until still.
                    if events.warp.is_none() && events.door_enter.is_none() {
                        let held = held_direction(input);
                        let coast = match (&mut self.avatar, held) {
                            (Avatar::Bike { tier, momentum }, None) if *momentum > 0 => {
                                *momentum -= 1;
                                *tier = (*momentum).min(2);
                                Some(self.facing)
                            }
                            _ => None,
                        };
                        if let Some(dir) = held.or(coast) {
                            if let Some(w) = self.arrow_warp(g, realm, dir) {
                                events.warp = Some(w);
                                return;
                            }
                            let (dx, dy) = dir.delta();
                            let (nx, ny) = (self.x + dx, self.y + dy);
                            if !levitate
                                && g.realm(realm).warp_at(nx, ny).is_some()
                                && g.door_at(realm, nx, ny).is_some()
                            {
                                self.facing = dir;
                                events.door_enter = Some((nx, ny));
                            } else if self.can_enter(g, realm, nx, ny, levitate) {
                                self.begin_move(
                                    g,
                                    realm,
                                    dir,
                                    run && coast.is_none(),
                                    true,
                                    coast.is_none(),
                                );
                            } else {
                                self.facing = dir;
                                self.bike_still();
                            }
                        } else {
                            self.bike_still();
                        }
                    }
                } else {
                    self.motion = Motion::Moving { remaining_px: left, speed };
                }
            }
        }
    }

    /// The C's stepTaken: refresh which map we're standing in, report a
    /// change so the caller can switch music, check for a warp edge, and
    /// track elevation like pret's ObjectEventUpdateElevation.
    fn step_taken(&mut self, g: &RealmGraph, realm_id: RealmId, events: &mut StepEvents) {
        let realm = g.realm(realm_id);
        let here = realm.map_at(self.x, self.y);
        if here.is_some() && here != self.current_map {
            events.entered_map = here;
        }
        // when outside any map (overworld gaps are traversable) keep the last
        // map for music, as the C does via outside_map + currentMap
        self.current_map = here.or(self.current_map);
        if let Some(cell) = realm.cell(self.x, self.y) {
            // pret adopts the tile's elevation INCLUDING 0 (transition):
            // standing on stairs lets you step to any elevation next. Only
            // bridges (15) leave it untouched.
            if cell.elevation != 15 {
                self.elevation = cell.elevation;
            }
        }
        // arrow-warp tiles fire on a directional press, not on arrival
        if g.arrow_dir_at(realm_id, self.x, self.y).is_none() {
            events.warp = realm.warp_at(self.x, self.y);
        }
    }

    /// Standing on an arrow-warp tile and pressing its direction fires the
    /// warp (even though the move itself is into a wall/realm edge).
    fn arrow_warp(&self, g: &RealmGraph, realm: RealmId, dir: Facing) -> Option<Warp> {
        let d = g.arrow_dir_at(realm, self.x, self.y)?;
        let wanted = match d {
            1 => Facing::Down,
            2 => Facing::Up,
            3 => Facing::Right,
            _ => Facing::Left,
        };
        (dir == wanted).then(|| g.realm(realm).warp_at(self.x, self.y))?
    }

    /// Apply a warp edge: land in the target realm, facing kept, motion
    /// reset — a warp is a discrete switch, never interpolated.
    pub fn apply_warp(&mut self, warp: Warp, target: &Realm) {
        self.x = warp.to_x;
        self.y = warp.to_y;
        self.motion = Motion::Idle;
        self.current_map = target.map_at(self.x, self.y);
        if let Some(cell) = target.cell(self.x, self.y) {
            if cell.elevation != 15 {
                self.elevation = cell.elevation;
            }
        }
    }

    /// Whether a foot frame shows this frame, per pret's anim tables: walking
    /// is foot 8 frames + mid 8 (half the tile); running is foot 5 + mid 3
    /// (the first 10 px of the 16 px tile at 2 px/frame).
    pub fn anim_walking(&self) -> bool {
        match self.motion {
            Motion::Moving { remaining_px, speed } => {
                if speed >= RUN_PX_PER_STEP {
                    remaining_px > METATILE * 3.0 / 8.0
                } else {
                    remaining_px > METATILE / 2.0
                }
            }
            Motion::Turning { steps_left } => steps_left > TURN_STEPS / 2,
            Motion::Idle => false,
        }
    }

    /// True mid-glide at running speed (selects the running sprite row).
    pub fn anim_running(&self) -> bool {
        matches!(self.motion, Motion::Moving { speed, .. } if speed >= RUN_PX_PER_STEP)
    }
}

/// C: if(input & 0b1111) with up > down > left > right priority.
fn held_direction(input: Input) -> Option<Facing> {
    if input.up {
        Some(Facing::Up)
    } else if input.down {
        Some(Facing::Down)
    } else if input.left {
        Some(Facing::Left)
    } else if input.right {
        Some(Facing::Right)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(dir: Facing) -> Input {
        match dir {
            Facing::Up => Input { up: true, ..Default::default() },
            Facing::Down => Input { down: true, ..Default::default() },
            Facing::Left => Input { left: true, ..Default::default() },
            Facing::Right => Input { right: true, ..Default::default() },
        }
    }

    #[test]
    fn turning_takes_eight_steps_and_does_not_move() {
        let graph = crate::map::RealmGraph::synthetic();
        let world = graph.realm(crate::map::OVERWORLD);
        let mut p = Player::spawn(10, 10, world);
        let mut ev = StepEvents::default();
        p.step(press(Facing::Left), &graph, crate::map::OVERWORLD, false, false, &mut ev);
        assert_eq!(p.facing, Facing::Left);
        assert!(matches!(p.motion, Motion::Turning { steps_left: TURN_STEPS }));
        for _ in 0..TURN_STEPS {
            p.step(press(Facing::Left), &graph, crate::map::OVERWORLD, false, false, &mut ev);
        }
        // turn finished, still on the same tile — next step starts the move
        assert_eq!((p.x, p.y), (10, 10));
    }

    #[test]
    fn moving_updates_tile_at_start_and_glides() {
        let graph = crate::map::RealmGraph::synthetic();
        let world = graph.realm(crate::map::OVERWORLD);
        let mut p = Player::spawn(10, 10, world);
        let mut ev = StepEvents::default();
        p.step(press(Facing::Down), &graph, crate::map::OVERWORLD, false, false, &mut ev); // already facing down → move
        assert_eq!((p.x, p.y), (10, 11), "tile updates at move start (as in C)");
        let (_, py) = p.world_px();
        assert!(py < 11.0 * METATILE, "glide starts behind the target tile");
        // release the pad: the glide finishes without chaining into a new tile
        let mut steps = 1;
        while p.motion != Motion::Idle {
            p.step(Input::default(), &graph, crate::map::OVERWORLD, false, false, &mut ev);
            steps += 1;
            assert!(steps < 60, "move must terminate");
        }
        assert_eq!(p.world_px(), (10.0 * METATILE, 11.0 * METATILE));
        // pret walking speed: 16 frames per tile
        assert_eq!(steps, 16);

        // held direction chains gaplessly: 32 more steps = exactly 32 px
        // (the tile COORD runs one ahead — it updates at each move start)
        let (_, py0) = p.world_px();
        for _ in 0..32 {
            p.step(press(Facing::Down), &graph, crate::map::OVERWORLD, false, false, &mut ev);
        }
        let (_, py1) = p.world_px();
        assert_eq!(py1 - py0, 32.0, "no idle gap between chained tiles");

        // mid-motion turns are instant — no Turning state while moving
        p.step(press(Facing::Down), &graph, crate::map::OVERWORLD, false, false, &mut ev);
        assert!(matches!(p.motion, Motion::Moving { .. }));
        for _ in 0..40 {
            p.step(press(Facing::Right), &graph, crate::map::OVERWORLD, false, false, &mut ev);
            assert!(
                !matches!(p.motion, Motion::Turning { .. }),
                "no turn-in-place while in motion"
            );
        }
        assert_eq!(p.facing, Facing::Right);
        assert!(p.x > 10, "kept moving in the new direction without a pause");
    }

    #[test]
    fn crossing_a_map_boundary_fires_entered_map() {
        let graph = crate::map::RealmGraph::synthetic();
        let world = graph.realm(crate::map::OVERWORLD);
        // spawn just west of the SYNTH_EAST border at x=30
        let mut p = Player::spawn(29, 10, world);
        assert_eq!(p.current_map, Some(0));
        let mut entered = None;
        for _ in 0..40 {
            let mut ev = StepEvents::default();
            p.step(press(Facing::Right), &graph, crate::map::OVERWORLD, false, false, &mut ev);
            if ev.entered_map.is_some() {
                entered = ev.entered_map;
                break;
            }
        }
        assert_eq!(entered, Some(1), "walking east must enter SYNTH_EAST");
        assert_eq!(p.current_map, Some(1));
    }

    #[test]
    fn cannot_leave_the_overworld_bounds() {
        let graph = crate::map::RealmGraph::synthetic();
        let world = graph.realm(crate::map::OVERWORLD);
        let mut p = Player::spawn(0, 0, world);
        let mut ev = StepEvents::default();
        p.step(press(Facing::Up), &graph, crate::map::OVERWORLD, false, false, &mut ev); // turn
        for _ in 0..TURN_STEPS {
            p.step(press(Facing::Up), &graph, crate::map::OVERWORLD, false, false, &mut ev);
        }
        p.step(press(Facing::Up), &graph, crate::map::OVERWORLD, false, false, &mut ev); // would move off-grid
        assert_eq!((p.x, p.y), (0, 0));
        assert_eq!(p.motion, Motion::Idle);
    }

    #[test]
    fn collision_blocks_movement() {
        let graph = crate::map::RealmGraph::synthetic();
        let world = graph.realm(crate::map::OVERWORLD);
        // the synthetic boulder sits at (20, 10); approach from the left
        let mut p = Player::spawn(19, 10, world);
        let mut ev = StepEvents::default();
        p.step(press(Facing::Right), &graph, crate::map::OVERWORLD, false, false, &mut ev); // turn
        for _ in 0..TURN_STEPS {
            p.step(press(Facing::Right), &graph, crate::map::OVERWORLD, false, false, &mut ev);
        }
        p.step(press(Facing::Right), &graph, crate::map::OVERWORLD, false, false, &mut ev); // blocked move attempt
        assert_eq!((p.x, p.y), (19, 10), "boulder tile must block");
        assert_eq!(p.motion, Motion::Idle);
    }

    #[test]
    fn stepping_onto_a_warp_tile_fires_the_warp_edge() {
        let graph = crate::map::RealmGraph::synthetic();
        let world = graph.realm(crate::map::OVERWORLD);
        // the synthetic door is at (5, 5); approach from (5, 4)
        let mut p = Player::spawn(5, 4, world);
        let mut warp = None;
        for _ in 0..20 {
            let mut ev = StepEvents::default();
            p.step(press(Facing::Down), &graph, crate::map::OVERWORLD, false, false, &mut ev);
            if ev.warp.is_some() {
                warp = ev.warp;
                break;
            }
        }
        let w = warp.expect("stepping on the door tile must fire its warp");
        assert_eq!(w.to_realm, 1);
        p.apply_warp(w, graph.realm(w.to_realm));
        assert_eq!((p.x, p.y), (4, 6));
        assert_eq!(p.motion, Motion::Idle);
        assert_eq!(p.current_map, Some(0), "inside the house map now");
    }
}
