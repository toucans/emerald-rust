//! The stub second backend: no graphics crate at all. It exists to prove the
//! seam — the same game, the same loop shape, a different single adapter file.
//! Runs the sim for a few simulated seconds with scripted input and prints
//! what it would have drawn. Also handy for CI smoke tests.

use types::{Backend, Frame, Input, TextureId};

struct HeadlessBackend {
    atlases: Vec<(u32, u32)>,
    tick: u32,
    frames: u64,
    quads_drawn: u64,
}

impl HeadlessBackend {
    fn new() -> Self {
        Self { atlases: Vec::new(), tick: 0, frames: 0, quads_drawn: 0 }
    }
}

impl Backend for HeadlessBackend {
    fn load_atlas(&mut self, rgba: &[u8], w: u32, h: u32) -> TextureId {
        assert_eq!(rgba.len(), (w * h * 4) as usize, "atlas must be RGBA8");
        self.atlases.push((w, h));
        TextureId(self.atlases.len() as u32 - 1)
    }

    fn poll_input(&mut self) -> Input {
        // scripted: walk right while zooming out, then down while zooming in
        self.tick += 1;
        Input {
            right: self.tick < 120,
            down: self.tick >= 120,
            zoom_out: self.tick < 120,
            zoom_in: self.tick >= 120,
            ..Default::default()
        }
    }

    fn draw_frame(&mut self, frame: &Frame) {
        assert!(frame.target_size.0 > 0 && frame.target_size.1 > 0);
        self.frames += 1;
        self.quads_drawn += (frame.offscreen.len() + frame.screen.len()) as u64;
    }
}

fn main() {
    let mut backend = HeadlessBackend::new();

    let pack = std::fs::read("assets/world.bin")
        .expect("assets/world.bin missing — regenerate with: cargo run -p worldgen");
    let mut world = game::World::new(&pack, &mut |rgba, w, h| backend.load_atlas(rgba, w, h))
        .expect("bad world pack — regenerate with: cargo run -p worldgen");

    let mut clock = game::FixedStep::new();
    let mut frame = Frame::default();

    // 4 simulated seconds of 60 Hz render frames — same loop as the real adapter
    for _ in 0..240 {
        let input = backend.poll_input();
        let (steps, alpha) = clock.advance(1.0 / 60.0);
        for _ in 0..steps {
            world.step(input);
        }
        world.frame(alpha, 240.0, 160.0, &mut frame);
        backend.draw_frame(&frame);
    }

    println!(
        "headless: {} frames, {} quads drawn, last frame {}+{} quads into a {}x{} target",
        backend.frames,
        backend.quads_drawn,
        frame.offscreen.len(),
        frame.screen.len(),
        frame.target_size.0,
        frame.target_size.1
    );
    assert!(backend.frames == 240 && !frame.offscreen.is_empty());
}
