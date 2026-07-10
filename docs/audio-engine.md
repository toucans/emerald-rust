# Plan: the hi-fi Rust m4a engine

> **Status: implemented** (except the site switch, the last section's item 5). The
> engine lives in [`audio/`](../audio/), the extractor in [`musicgen/`](../musicgen/),
> the sinks in [`backends/`](../backends/). The verification harness
> (`cargo run -p audio --bin verify --release`) passes the whole soundtrack, and the
> acceptance target renders complete: `mus_cycling`, 1378/1378 notes, keysplit piano
> and drumset resolving at runtime. This doc remains the design record.

Read [`../README.md`](../README.md) first — the audio section and the ports-and-adapters
architecture frame this. This doc is the **design and the decisions**; the hard,
intelligence-heavy implementation is deliberately left to Fable (see the last section).

## Goal

The most beautiful, hi-fi Rust version of Emerald's **GBA (m4a/MP2K) music**. Pure logic:
score + instrument samples in → **44.1 kHz float PCM** out, no I/O, no device. Fed to the
`AudioSink` port (cpal native / WebAudio web) like everything else in the repo.

**SC-88 is out of scope.** Only the GBA soundtrack is kept. So the leaked
`pm_eme_ose` MIDIs are **not needed** here — see "Provenance" below for why.

## The core decision: separate sequencing from output

Our current C engine (`pokeemerald_SDL3/src/m4a.c`) is a from-scratch reinterpretation
that conflates two concerns. The Rust engine should split them, because they have opposite
requirements:

| Concern | Requirement | Source of truth |
|---|---|---|
| **Sequencing + instrument dispatch** — event processing, voicegroups, keysplit, drumkits, envelopes, vibrato/LFO, pitch bend, loop points, the 59.7275 Hz frame tick | **Byte-accurate fidelity** — this is where "missing instrument" bugs live | **pret/pokeemerald's engine** (`src/m4a.c` + `src/m4a_1.s` + `src/m4a_tables.c`) is ground truth |
| **Output / rendering** — PSG synthesis, sample resampling, mixing, sample rate | **Hi-fi, on purpose** — we *want* to diverge from the GBA here | Our own hi-fi stage (the float render path in the C engine is a proven starting point) |

Port the first faithfully; keep/rebuild the second as hi-fi. That gets authentic notes
**and** clean sound — neither of which our current engine fully delivers alone.

## Sequencing + dispatch — port pret as ground truth

- Port pret's MP2K engine to Rust as pure logic: `m4a.c` (the sequencer/command
  processing) plus the mixer logic that lives in the hand-written ARM assembly
  `m4a_1.s` (translate the *algorithm*, not the assembly — it's a fixed-point mixing loop
  with envelope ramping).
- **Resolve keysplits and drumkits at runtime, like pret does** — `voice_keysplit`
  (melodic key splits) and `voice_keysplit_all` (drumsets). Our C project resolves these
  at *extraction* time (`pokeemerald-music/extract.py` flattens every `(program, key)` to a
  concrete voice); that flattening is a likely source of the missing-instrument bug and
  should go away. Runtime resolution against the real voicegroup tables is both more
  faithful and simpler to reason about.
- This means the **data the engine loads must be the raw structures** — voicegroups,
  keysplit tables, drumset sub-groups, and the sample bank — *not* pre-flattened
  `(program,key)→voice`. Design the pak/data format accordingly (a data-format change from
  what `music.pak` currently carries).

## Output stage — hi-fi, 44.1 kHz

Honest fidelity ceiling, so nobody expects magic:

- **PSG channels** (square / programmable wave / noise) are *synthesized* → 44.1 kHz is a
  clean win: no aliasing, no GBA DAC harshness. Consider band-limited synthesis
  (PolyBLEP/BLEP) for alias-free square edges — the cleanest possible chip voice. *(Fable
  decides BLEP vs naive after listening.)*
- **DirectSound PCM** are the GBA's **8-bit, low-rate samples**. Resample them up with good
  interpolation (sinc/cubic), skip the GBA's ~13.4 kHz output downsample entirely. They end
  up **clean and un-crushed, but bounded by the 8-bit source** — that's the ceiling.
  Exceeding it would require better samples (the SC-88 route, which is out of scope).
- Mix in **float, full stereo**, correct linear pan, correct envelopes and loop points. No
  forced downsample, no reverb unless a song asks for it.

Net: PSG sounds genuinely hi-fi; sampled instruments sound as clean as their sources allow.
That is the correct, beautiful ceiling for "the GBA's own instruments, at their best."

## Provenance — what feeds the engine (do not re-derive)

- **GBA sequence + voices + samples** come from **pret/pokeemerald**: pret's
  machine-derived 24-tpqn MIDIs (24 ticks/beat *is the hardware's own resolution* — not
  compression loss, so there is no finer "true" GBA timing to recover), the voicegroups,
  and the 8-bit AIFF samples. This is the exact GBA arrangement — the right and complete
  source.
- **The leaked `pm_eme_ose` MIDIs were used ONLY for the SC-88 soundtrack** (the composers'
  480-tpqn arrangement), never for the m4a player. With SC-88 dropped, they are not used.
  (They're also a *different arrangement* — different instruments/drums, `mus_route111`
  missing — so they would not be a clean "more accurate GBA" source even if wanted.)
- Music data source-of-truth stays in
  [pokeemerald_SDL3](https://github.com/toucans/pokeemerald_SDL3); never extract or
  re-derive music from the ROM.

## Fit with the repo

Pure logic (samples → PCM) behind the `AudioSink` port; compiles to native + wasm from one
source.

**Decided: the pokeemerald-music site moves onto this engine when it's done.** The C
`m4a.c` currently serves both the game and the site (`m4a.wasm`); rather than fork, this
Rust engine compiles to wasm and **replaces `m4a.wasm`**, so one engine serves both again
and the missing-instrument fix reaches the site for free.

**SC-88 is dropped from the site too (decided).** The site today plays *both* soundtracks
through `m4a.wasm` (the GBA path *and* the SC-88 GM path in `m4a.c`). This Rust engine is
GBA-only, and SC-88 goes away entirely — so when the site moves to the
Rust engine, the SC-88 buttons and the old `m4a.wasm` are **removed**, not kept alongside.
The replacement is clean: one GBA-only Rust engine serves both game and site, no second
engine. (Do this *at the switch*, not now — SC-88 is a working feature on the current site
and there's no benefit to removing it before the Rust engine exists to replace `m4a.wasm`.)

## Verification (the acceptance test)

The port is "done" only when it's **provably not missing instruments**. Build a harness that
renders each song and compares against a reference (pret's own output and/or the current
renders), flagging any track/voice that drops out. This is the guardrail for the whole
effort — design it early.

## Do NOT debug the current pipeline — rebuild so the bug can't exist

This is a from-scratch rewrite. The known `mus_cycling` failure lives in the old
`pokeemerald-music/extract.py` flattening — **the exact code the new design deletes.**
Debugging it would be throwaway work whose fix doesn't transfer (different language,
different resolution model, different data format). So don't autopsy the legacy pipeline;
the ground-truth reference for *how* keysplits/drumkits must behave is **pret's engine**,
not our buggy Python. Build runtime resolution against the real voicegroup tables and the
whole "a voice got dropped at extraction time" class is gone by construction.

## Re-extraction, done beautifully (one format, no drift)

The current pipeline is three programs that must agree on a format (`extract.py` writes
JSON → `pack_music.py` packs → `m4a.c` parses) — drift between them is itself a bug source.
The beautiful replacement:

- Re-extract from pret **from scratch**, emitting the **raw** structures the runtime needs
  — voicegroups, keysplit tables, drumset sub-groups, the sample bank — *not* pre-flattened
  `(program,key)→voice`. (Runtime resolution needs the raw tables anyway.)
- **Define the data types once, in Rust, and share them between the extractor and the
  engine.** The extractor writes them; the engine reads them; there is no separate format
  spec to drift. Prefer writing the extractor in Rust too so the whole thing is one
  toolchain with one source of truth for the format — the single-source-of-truth principle
  applied to the data pipeline itself. (This is a real improvement over the C project, not
  just a port.)

## Acceptance target: `mus_cycling` (verify, don't diagnose)

**`mus_cycling` is audibly missing an instrument** in the *current* setup (found by ear).
Its role here is a **test case for the new engine**, not a legacy bug to fix. It's ideal
for that because it exercises the two hardest features and the result is ear-checkable:

- Voicegroup `sound/voicegroups/cycling.inc` (`-G_cycling` in `midi.cfg`) contains a
  **`voice_keysplit` (piano, `keysplit_piano`)** and a **`voice_keysplit_all`
  (`voicegroup_rs_drumset`)** — precisely the keysplit/drumkit resolution the new runtime
  path must get right. (The `_alt` PSG voices in that group are ordinary variants; not the
  concern.)
- **Definition of done:** the new engine renders `mus_cycling` complete — every voice the
  score references sounds — confirmed by ear and by the verification harness below.

## Left to Fable (needs high AI intelligence — do not attempt in this plan)

1. The faithful **pret → Rust port** of the sequencer + mixer (incl. reading `m4a_1.s`'s
   fixed-point mixing/envelope math and re-expressing it cleanly in Rust).
2. **From-scratch re-extraction** emitting raw voicegroups/keysplit/drumset structures,
   with the data types **shared (defined once in Rust) between extractor and engine**, plus
   the **runtime keysplit/drumkit resolution** that consumes them.
3. The **hi-fi output stage**: PSG synthesis (with/without BLEP), sample resampler choice,
   float mixer — the one place it's fine to reuse the C engine's existing float render math
   as a starting point (it's the part that isn't buggy).
4. The **verification harness** proving no instrument is missing, with **`mus_cycling` as
   the first acceptance target** (render it complete — every referenced voice sounds).
5. **Port the pokeemerald-music site onto this engine** (wasm build replacing `m4a.wasm`),
   once it's done — after settling the SC-88-on-the-site consequence above.
