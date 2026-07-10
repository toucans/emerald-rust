//! The macroquad adapter — the ONLY file in the repo allowed to name a
//! macroquad type. Implements `types::Backend` (render the offscreen pass
//! into a render target, composite the screen pass) and owns the event loop:
//! poll input, `step()` the sim at fixed dt, `frame(alpha)`, `draw_frame`.
//! macroquad is a framework that wants to own everything; it is confined here.

use macroquad::prelude as mq;
use types::{Backend, Flip, Frame, Input, Quad, TextureId};

#[cfg(not(target_arch = "wasm32"))]
#[path = "audio_cpal.rs"]
mod audio_cpal;

#[cfg(target_arch = "wasm32")]
#[path = "audio_web.rs"]
mod audio_web;

#[cfg(target_arch = "wasm32")]
#[path = "save_web.rs"]
mod save_web;

/// Music + SFX: the m4a Mixer renders into the AudioSink port; each loop
/// pass keeps ~150 ms queued and fires one-shot sound effects (doors, warps,
/// surf). Song packs arrive via `set_music` — see `Songs` for the fetching.
#[cfg(not(target_arch = "wasm32"))]
struct Music {
    mixer: audio::Mixer,
    sink: audio_cpal::CpalSink,
    scratch: Vec<f32>,
    chunk: Vec<i16>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Music {
    fn open(sfx_pack: Option<&[u8]>) -> Option<Music> {
        let sink = audio_cpal::CpalSink::open()?;
        let mixer = audio::Mixer::new(sfx_pack, sink.sample_rate).ok()?;
        Some(Music { mixer, sink, scratch: Vec::new(), chunk: Vec::new() })
    }

    fn tend(&mut self, sfx: Option<&str>) {
        use types::AudioSink;
        if let Some(name) = sfx {
            self.mixer.play_sfx(name);
        }
        let target = self.sink.sample_rate as usize * 2 * 3 / 20; // 150 ms
        while self.sink.buffered() < target {
            self.chunk.resize(2 * 738, 0);
            let mut chunk = std::mem::take(&mut self.chunk);
            self.mixer.render_i16(&mut self.scratch, &mut chunk);
            self.sink.queue(&chunk);
            self.chunk = chunk;
        }
    }
}

/// Per-song streaming: musicgen writes one pack per song, so playing a map's
/// music means fetching `assets/music/<name>.bin` — native reads disk, web
/// fetches over HTTP (the browser cache makes revisits free). The fetch runs
/// as a macroquad coroutine so the game loop never blocks; until it lands,
/// the previous song keeps playing.
struct Songs {
    current: Option<String>,
    loading: Option<(String, mq::coroutines::Coroutine<Option<Vec<u8>>>)>,
}

impl Songs {
    fn new() -> Songs {
        Songs { current: None, loading: None }
    }

    /// Returns a freshly fetched pack when `want` just arrived.
    fn tend(&mut self, want: Option<&str>) -> Option<Vec<u8>> {
        if let Some(name) = want.map(str::to_lowercase) {
            let busy = self.current.as_deref() == Some(&name)
                || self.loading.as_ref().is_some_and(|(n, _)| *n == name);
            if !busy {
                let path = format!("assets/music/{name}.bin");
                let co = mq::coroutines::start_coroutine(async move {
                    mq::load_file(&path).await.ok()
                });
                self.loading = Some((name, co));
            }
        }
        let (name, co) = self.loading.as_ref()?;
        let bytes = co.retrieve()?;
        // a missing song file still becomes `current` — never refetch a 404
        // every frame; the previous song simply keeps playing
        self.current = Some(name.clone());
        self.loading = None;
        bytes
    }
}

struct MacroquadBackend {
    atlases: Vec<mq::Texture2D>,
    sorted: Vec<Quad>,
    target: Option<mq::RenderTarget>,
}

impl MacroquadBackend {
    fn new() -> Self {
        Self { atlases: Vec::new(), sorted: Vec::new(), target: None }
    }

    /// (Re)create the offscreen target when the requested size changes.
    fn target_for(&mut self, w: u32, h: u32) -> mq::RenderTarget {
        let stale = self
            .target
            .as_ref()
            .is_none_or(|t| t.texture.width() as u32 != w || t.texture.height() as u32 != h);
        if stale {
            // sample_count 0: macroquad's default of 1 still takes its MSAA
            // resolve path, whose blit is WebGL2-only — and miniquad 0.4's
            // web glue is WebGL1. 0 = a plain color texture, everywhere.
            let t = mq::render_target_ex(
                w,
                h,
                mq::RenderTargetParams { sample_count: 0, depth: false },
            );
            // linear: the screen pass minifies the integer-scaled world —
            // this is the smooth half of crisp+smooth zoom
            t.texture.set_filter(mq::FilterMode::Linear);
            self.target = Some(t);
        }
        self.target.clone().unwrap()
    }

    fn draw_pass(&mut self, quads: &[Quad], target_tex: Option<&mq::Texture2D>) {
        self.sorted.clear();
        self.sorted.extend_from_slice(quads);
        self.sorted.sort_by_key(|q| q.layer); // stable: vec order breaks ties

        for q in &self.sorted {
            let tex = if q.tex == TextureId::TARGET {
                match target_tex {
                    Some(t) => t,
                    None => continue, // TARGET is only valid in the screen pass
                }
            } else {
                match self.atlases.get(q.tex.0 as usize) {
                    Some(t) => t,
                    None => continue,
                }
            };
            let tint = mq::Color::from_rgba(q.tint.r, q.tint.g, q.tint.b, q.tint.a);
            let (flip_x, flip_y) = match q.flip {
                Flip::None => (false, false),
                Flip::H => (true, false),
                Flip::V => (false, true),
                Flip::Both => (true, true),
            };
            // Camera2D's y-up projection mirrors the offscreen pass into the
            // target; sampling the target texture mirrors it back — no manual
            // flip needed (verified pixel-by-pixel in the browser).
            mq::draw_texture_ex(
                tex,
                q.dst.x,
                q.dst.y,
                tint,
                mq::DrawTextureParams {
                    dest_size: Some(mq::vec2(q.dst.w, q.dst.h)),
                    source: Some(mq::Rect::new(q.src.x, q.src.y, q.src.w, q.src.h)),
                    flip_x,
                    flip_y,
                    ..Default::default()
                },
            );
        }
    }
}

impl Backend for MacroquadBackend {
    fn load_atlas(&mut self, rgba: &[u8], w: u32, h: u32) -> TextureId {
        let tex = mq::Texture2D::from_rgba8(w as u16, h as u16, rgba);
        // nearest: world pixels stay hard inside the integer-scaled target —
        // the crisp half of crisp+smooth zoom
        tex.set_filter(mq::FilterMode::Nearest);
        self.atlases.push(tex);
        TextureId(self.atlases.len() as u32 - 1)
    }

    fn poll_input(&mut self) -> Input {
        use mq::KeyCode as K;
        let (_, wheel) = mq::mouse_wheel();
        Input {
            up: mq::is_key_down(K::Up),
            down: mq::is_key_down(K::Down),
            left: mq::is_key_down(K::Left),
            right: mq::is_key_down(K::Right),
            a: mq::is_key_down(K::Z),
            b: mq::is_key_down(K::X),
            start: mq::is_key_down(K::Enter),
            select: mq::is_key_down(K::Backspace),
            zoom_in: mq::is_key_down(K::E) || wheel > 0.0,
            zoom_out: mq::is_key_down(K::Q) || wheel < 0.0,
        }
    }

    fn draw_frame(&mut self, frame: &Frame) {
        let (tw, th) = frame.target_size;
        let target = self.target_for(tw.max(1), th.max(1));

        // pass 1: the world into the offscreen target, in target pixels
        mq::set_camera(&mq::Camera2D {
            zoom: mq::vec2(2.0 / tw as f32, 2.0 / th as f32),
            target: mq::vec2(tw as f32 / 2.0, th as f32 / 2.0),
            render_target: Some(target.clone()),
            ..Default::default()
        });
        mq::clear_background(mq::BLACK);
        self.draw_pass(&frame.offscreen, None);

        // pass 2: composite to the window
        mq::set_default_camera();
        mq::clear_background(mq::BLACK);
        self.draw_pass(&frame.screen, Some(&target.texture.clone()));
        // macroquad presents implicitly on next_frame().await
    }
}

#[macroquad::main("Emerald")]
async fn main() {
    let mut backend = MacroquadBackend::new();

    // native: reads the file; web: fetches the same relative URL
    let pack = mq::load_file("assets/world.bin")
        .await
        .expect("assets/world.bin missing — regenerate with: cargo run -p worldgen");
    let mut world = game::World::new(&pack, &mut |rgba, w, h| backend.load_atlas(rgba, w, h))
        .expect("bad world pack — regenerate with: cargo run -p worldgen");

    // the small always-resident sound-effect pack; songs stream one by one
    let sfx_pack = mq::load_file("assets/music/sfx.bin").await.ok();
    if sfx_pack.is_none() {
        eprintln!("assets/music/sfx.bin missing — regenerate with: cargo run -p musicgen");
    }

    #[cfg(not(target_arch = "wasm32"))]
    let mut music = Music::open(sfx_pack.as_deref()); // None = no audio device
    #[cfg(target_arch = "wasm32")]
    audio_web::init(sfx_pack);

    let mut songs = Songs::new();
    let mut clock = game::FixedStep::new();
    let mut frame = Frame::default();

    // web: the last save published to the page (mirrored to localStorage)
    #[cfg(target_arch = "wasm32")]
    let mut last_save: Vec<u8> = Vec::new();

    loop {
        let input = backend.poll_input();
        let (steps, alpha) = clock.advance(mq::get_frame_time() as f64);
        let mut sfx = None;
        for _ in 0..steps {
            let ev = world.step(input);
            sfx = ev.sfx.or(sfx);
        }

        if let Some(pack) = songs.tend(world.current_music()) {
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(m) = &mut music {
                let _ = m.mixer.set_music(&pack);
            }
            #[cfg(target_arch = "wasm32")]
            audio_web::set_music(pack);
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(m) = &mut music {
            m.tend(sfx);
        }
        #[cfg(target_arch = "wasm32")]
        {
            audio_web::tend(sfx);
            if let Some(bytes) = save_web::take_restore() {
                let _ = world.load_state(&bytes); // stale/corrupt: keep spawn
            }
            let save = world.save_state();
            if save != last_save {
                save_web::publish(save.clone());
                last_save = save;
            }
        }

        world.frame(alpha, mq::screen_width(), mq::screen_height(), &mut frame);
        backend.draw_frame(&frame);
        mq::next_frame().await;
    }
}
