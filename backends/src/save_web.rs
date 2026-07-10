//! Browser save persistence (wasm) — the page's JS (web/index.html) polls
//! the latest sim save through these exports and mirrors it to localStorage;
//! at boot it pushes the stored bytes back and the game loop restores them.
//! Same single-threaded polling pattern as audio_web: the wasm main loop and
//! the page's timers interleave on one thread, so no locking is needed.

use std::cell::RefCell;

thread_local! {
    static SAVE: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static RESTORE: RefCell<Restore> = const {
        RefCell::new(Restore { buf: Vec::new(), ready: false })
    };
}

struct Restore {
    buf: Vec<u8>,
    ready: bool,
}

/// Game loop: publish the current save state for JS to pick up.
pub fn publish(bytes: Vec<u8>) {
    SAVE.with_borrow_mut(|s| *s = bytes);
}

/// Game loop: a save pushed by JS at boot, once.
pub fn take_restore() -> Option<Vec<u8>> {
    RESTORE.with_borrow_mut(|r| {
        r.ready.then(|| {
            r.ready = false;
            std::mem::take(&mut r.buf)
        })
    })
}

/// JS: the published save's location (read len first; 0 = nothing yet).
#[unsafe(no_mangle)]
pub extern "C" fn emerald_save_ptr() -> *const u8 {
    SAVE.with_borrow(|s| s.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn emerald_save_len() -> u32 {
    SAVE.with_borrow(|s| s.len() as u32)
}

/// JS: get a buffer for `len` restore bytes, fill it, then commit.
#[unsafe(no_mangle)]
pub extern "C" fn emerald_restore_buffer(len: u32) -> *mut u8 {
    RESTORE.with_borrow_mut(|r| {
        r.ready = false;
        r.buf.resize(len as usize, 0);
        r.buf.as_mut_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn emerald_restore_commit() {
    RESTORE.with_borrow_mut(|r| r.ready = true);
}
