//! The WebAudio adapter (wasm) — the browser-side twin of audio_cpal.rs.
//! The m4a engine lives inside the game's wasm instance; the page's JS (see
//! web/index.html) creates an AudioWorklet — its feeder runs on the main
//! thread, the same thread as this wasm — and pulls PCM through the two
//! exports below. The mixer is built lazily on the first pull because only
//! the AudioContext knows the real output sample rate. Song packs are
//! streamed: the game loop fetches one file per song and hands it over here.

use std::cell::RefCell;

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State {
        sfx_pack: None,
        song_pack: None,
        mixer: None,
        pending_sfx: Vec::new(),
        buf: Vec::new(),
    });
}

struct State {
    /// The resident se_* pack, handed over once at startup.
    sfx_pack: Option<Vec<u8>>,
    /// A freshly fetched single-song pack awaiting the render thread.
    song_pack: Option<Vec<u8>>,
    mixer: Option<audio::Mixer>,
    pending_sfx: Vec<String>,
    buf: Vec<f32>,
}

/// Called once from the game loop with the fetched sfx pack.
pub fn init(sfx_pack: Option<Vec<u8>>) {
    STATE.with_borrow_mut(|s| s.sfx_pack = sfx_pack);
}

/// A fetched song pack: swapped in at the next PCM pull.
pub fn set_music(pack_bytes: Vec<u8>) {
    STATE.with_borrow_mut(|s| s.song_pack = Some(pack_bytes));
}

/// Called from the game loop: queue any one-shot sound effect.
pub fn tend(sfx: Option<&str>) {
    if let Some(name) = sfx {
        STATE.with_borrow_mut(|s| s.pending_sfx.push(name.to_string()));
    }
}

/// JS: get the pointer to an f32 buffer able to hold `frames` stereo frames.
#[unsafe(no_mangle)]
pub extern "C" fn emerald_audio_buffer(frames: u32) -> *mut f32 {
    STATE.with_borrow_mut(|s| {
        s.buf.resize(frames as usize * 2, 0.0);
        s.buf.as_mut_ptr()
    })
}

/// JS: render `frames` stereo frames at `rate` into the buffer.
#[unsafe(no_mangle)]
pub extern "C" fn emerald_audio_render(frames: u32, rate: u32) {
    STATE.with_borrow_mut(|s| {
        s.buf.resize(frames as usize * 2, 0.0);
        if s.mixer.is_none() {
            let sfx = s.sfx_pack.take();
            s.mixer = audio::Mixer::new(sfx.as_deref(), rate).ok();
        }
        let Some(mixer) = &mut s.mixer else {
            s.buf.fill(0.0);
            return;
        };
        if let Some(bytes) = s.song_pack.take() {
            let _ = mixer.set_music(&bytes);
        }
        for name in s.pending_sfx.drain(..) {
            mixer.play_sfx(&name);
        }
        mixer.render(&mut s.buf);
    })
}
