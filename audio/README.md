# audio — the m4a engine

The GBA m4a/MP2K music engine as pure logic: score + samples in, float stereo PCM
out, no I/O, no device; native and wasm from one source. The design doc is
[docs/audio-engine.md](../docs/audio-engine.md) — read it before touching audio.
Fed to the `AudioSink` adapters in [`backends/`](../backends/).

- `engine.rs` — sequencing + dispatch ported from pret as ground truth (frame tick,
  tempo accumulator, TrkVolPitSet math, envelopes, LFO) with keysplits/drumsets
  resolved at note-on against the raw tables; hi-fi output stage (its float math
  ported from pokeemerald_SDL3's engine, which was verified sample-exact against GBA
  recordings).
- `data.rs` — the music-pack format, defined once and shared with `musicgen`.
- `Mixer` (lib.rs) — music + SFX: two engines over one pack, one mixed stereo
  stream. BGM loops; se_* one-shots (doors, warps, surf) play on top — the same
  separate-players-one-DAC shape as the GBA.
- `src/bin/verify.rs` — the acceptance harness: `cargo run -p audio --bin verify
  --release [song ...] [--wav out.wav]`. Statically resolves every referenced note,
  renders a full pass, and proves every note-bearing track sounds. All 473 songs
  (204 mus_* + 269 se_*) pass; out-of-range keys in the authored data are
  classified quirks, not failures.
- `examples/inspect.rs` — print a song's voice-resolution chain from the pack.
