//! The cpal audio adapter — the ONLY file in the repo allowed to name a cpal
//! type. Implements `types::AudioSink`: a ring buffer the engine's PCM is
//! pushed into; cpal's callback thread drains it (underrun = silence). Native
//! only; the web build gets a WebAudio adapter instead.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use types::AudioSink;

pub struct CpalSink {
    ring: Arc<Mutex<VecDeque<i16>>>,
    _stream: cpal::Stream,
    pub sample_rate: u32,
}

impl CpalSink {
    /// Open the default output device. `None` (no device / unsupported
    /// format) simply means the game runs without sound.
    pub fn open() -> Option<CpalSink> {
        let device = cpal::default_host().default_output_device()?;
        let config = device.default_output_config().ok()?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        if channels == 0 {
            return None;
        }
        let ring: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));
        let cb_ring = ring.clone();

        // engine output is stereo; up/down-mix to the device's channel count
        let fill = move |out: &mut [f32]| {
            let mut ring = cb_ring.lock().unwrap();
            for frame in out.chunks_mut(channels) {
                let l = ring.pop_front().unwrap_or(0) as f32 / 32768.0;
                let r = ring.pop_front().unwrap_or(0) as f32 / 32768.0;
                for (i, o) in frame.iter_mut().enumerate() {
                    *o = match i {
                        0 => l,
                        1 => r,
                        _ => 0.0,
                    };
                }
                if channels == 1 {
                    frame[0] = 0.5 * (l + r);
                }
            }
        };

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device
                .build_output_stream(
                    &config.into(),
                    move |data: &mut [f32], _| fill(data),
                    |e| eprintln!("audio: {e}"),
                    None,
                )
                .ok()?,
            _ => return None,
        };
        stream.play().ok()?;
        Some(CpalSink { ring, _stream: stream, sample_rate })
    }

    /// Stereo samples currently queued (for the adapter's refill loop).
    pub fn buffered(&self) -> usize {
        self.ring.lock().unwrap().len()
    }
}

impl AudioSink for CpalSink {
    fn queue(&mut self, samples: &[i16]) {
        self.ring.lock().unwrap().extend(samples);
    }
}
