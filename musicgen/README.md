# musicgen — pret → assets/music/

Build-time extractor: reads the pret/pokeemerald clone and writes `assets/music/` —
one always-resident `sfx.bin` (every se_* effect) plus one self-contained pack per
mus_* song (its events + the voicegroup/sample closure it reaches, indices
remapped), in the format defined once in
[`audio/src/data.rs`](../audio/src/data.rs). Per-song packs are what let the web
build stream a song only when a map asks for it. Std-only.

    cargo run -p musicgen --release   # [pret-path] [output], defaults ~/pokeemerald

It emits the RAW structures the engine's runtime dispatch needs — voicegroups with
`voice_keysplit`/`voice_keysplit_all` entries intact, keysplit tables (with the GBA's
contiguous-in-ROM adjacency baked in exactly), the 8-bit sample bank (pret's WAVs via
wav2agb's semantics: smpl loops, agbp exact pitch, agbl exact loop end), PSG wave
patterns, and every song — the mus_* soundtrack AND the se_* sound effects, 473
in all — as tick-exact events from the 24-tpqn MIDIs (midi.cfg options,
mid2agb's velocity LUT).

It never pre-flattens `(program, key) → voice` — that flattening was the old
pipeline's missing-instrument bug (docs/audio-engine.md), gone here by construction.
