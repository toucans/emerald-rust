//! The Rust m4a/MP2K engine (docs/audio-engine.md): authentic sequencing +
//! instrument dispatch ported from pret as ground truth, feeding a hi-fi
//! 44.1 kHz float output stage. Pure logic — score + samples in, PCM out; no
//! I/O, no audio device; compiles to native and wasm from one source.

pub mod data;
pub mod engine;

pub use engine::Engine;

/// Music + sound effects: two engines, one mixed stereo stream. BGM loops;
/// SFX play once on top (the GBA mixes SE the same way — separate players,
/// one DAC). The SFX engine holds the small always-resident se_* pack; the
/// music engine is swapped per streamed single-song pack (musicgen writes
/// one file per song so the browser only fetches what it plays).
pub struct Mixer {
    pub music: Option<Engine>,
    pub sfx: Option<Engine>,
    sample_rate: u32,
    scratch: Vec<f32>,
}

impl Mixer {
    /// `sfx_pack`: the resident sound-effect pack (None = SFX stay silent).
    pub fn new(sfx_pack: Option<&[u8]>, sample_rate: u32) -> Result<Mixer, data::PackError> {
        let sfx = match sfx_pack {
            Some(bytes) => Some(Engine::new(data::MusicPack::from_bytes(bytes)?, sample_rate)),
            None => None,
        };
        Ok(Mixer { music: None, sfx, sample_rate, scratch: Vec::new() })
    }

    /// Swap in a fetched song pack and loop its first song, replacing
    /// whatever was playing.
    pub fn set_music(&mut self, pack_bytes: &[u8]) -> Result<(), data::PackError> {
        let mut engine = Engine::new(data::MusicPack::from_bytes(pack_bytes)?, self.sample_rate);
        engine.play(0, true);
        self.music = Some(engine);
        Ok(())
    }

    pub fn play_sfx(&mut self, name: &str) -> bool {
        match self.sfx.as_mut().and_then(|e| e.song_index(name).map(|i| (e, i))) {
            Some((engine, i)) => {
                engine.play(i, false);
                true
            }
            None => false,
        }
    }

    pub fn render(&mut self, out: &mut [f32]) {
        match &mut self.music {
            Some(engine) => engine.render(out),
            None => out.fill(0.0),
        }
        if let Some(sfx) = &mut self.sfx {
            self.scratch.clear();
            self.scratch.resize(out.len(), 0.0);
            sfx.render(&mut self.scratch);
            for (o, s) in out.iter_mut().zip(&self.scratch) {
                *o = (*o + s).clamp(-1.0, 1.0);
            }
        }
    }

    pub fn render_i16(&mut self, scratch: &mut Vec<f32>, out: &mut [i16]) {
        scratch.clear();
        scratch.resize(out.len(), 0.0);
        self.render(scratch);
        for (o, s) in out.iter_mut().zip(scratch.iter()) {
            *o = (s * 32767.0) as i16;
        }
    }
}
