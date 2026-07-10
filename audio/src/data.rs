//! The music pack: the one data format between the extractor (`musicgen`) and
//! the m4a engine. Defined once, here, in Rust — the extractor writes it, the
//! engine reads it, there is no separate format spec to drift
//! (docs/audio-engine.md "Re-extraction, done beautifully").
//!
//! The pack carries the RAW structures the runtime needs — voicegroups with
//! their keysplit/drumset entries intact, keysplit tables, the sample bank —
//! never a pre-flattened (program, key) → voice map. Runtime resolution
//! against these tables is what makes the old extraction-time
//! missing-instrument bug impossible by construction.
//!
//! Format: `EMMUSIC1` magic, little-endian, length-prefixed.
//! Regenerate with: `cargo run -p musicgen -- <path-to-pret-clone>`.

pub const MAGIC: &[u8; 8] = b"EMMUSIC1";

#[derive(Debug, PartialEq)]
pub struct MusicPack {
    /// DirectSound samples: 8-bit signed PCM straight from pret's AIFFs.
    pub samples: Vec<SampleData>,
    /// PSG programmable wave patterns: 32 x 4-bit samples.
    pub waves: Vec<[u8; 32]>,
    /// Keysplit tables: MIDI key → voice index into the sub-voicegroup.
    pub keysplits: Vec<KeysplitTable>,
    pub voicegroups: Vec<VoiceGroup>,
    pub songs: Vec<SongData>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SampleData {
    pub name: String,
    pub rate: f32,
    /// Loop start frame; None = one-shot (pret: a real loop needs both the
    /// INST sustain loop and its MARK markers).
    pub loop_start: Option<u32>,
    pub pcm: Vec<i8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeysplitTable {
    pub name: String,
    pub map: [u8; 128],
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoiceGroup {
    pub name: String,
    /// First program number the entries cover (`voice_group name, offset`).
    pub offset: u16,
    pub entries: Vec<Voice>,
}

impl VoiceGroup {
    pub fn voice(&self, program: u16) -> Option<&Voice> {
        self.entries.get(program.checked_sub(self.offset)? as usize)
    }
}

/// One voicegroup entry, raw. ADSR bytes keep pret's semantics (DirectSound:
/// 0-255 envelope math; PSG: GB 0-15 envelope registers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voice {
    DirectSound {
        base_key: u8,
        pan: u8,
        sample: u16,
        adsr: [u8; 4],
        /// voice_directsound_no_resample: plays at the sample's own rate.
        fixed: bool,
    },
    Square {
        base_key: u8,
        pan: u8,
        /// GB duty 0-3: 12.5 / 25 / 50 / 75 %.
        duty: u8,
        /// Square 1 sweep register (unused by the hi-fi stage, kept raw).
        sweep: u8,
        adsr: [u8; 4],
    },
    Wave {
        base_key: u8,
        pan: u8,
        wave: u16,
        adsr: [u8; 4],
    },
    Noise {
        base_key: u8,
        pan: u8,
        /// GB NR43 counter width bit: 0 = 15-bit LFSR, 1 = 7-bit.
        period: u8,
        adsr: [u8; 4],
    },
    /// Melodic keysplit: look the note's key up in `table`, then use that
    /// voice index within `group`. Resolved at NOTE-ON, at runtime.
    Keysplit { group: u16, table: u16 },
    /// Drumset: the note's key IS the voice index within `group`; the note
    /// sounds at the sub-voice's own base key.
    KeysplitAll { group: u16 },
    /// Anything the engine doesn't play (cries, malformed lines) — kept so
    /// indices stay aligned with pret's tables.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SongData {
    pub name: String,
    pub voicegroup: u16,
    /// midi.cfg -V master volume (0-127).
    pub master_volume: u8,
    /// midi.cfg -R reverb (0-127), GBA-style frame-buffer echo.
    pub reverb: u8,
    /// MIDI ticks per beat (24 for pret's machine-derived MIDIs — the
    /// hardware's own resolution).
    pub division: u16,
    /// (tick, microseconds per beat), sorted by tick.
    pub tempos: Vec<(u32, u32)>,
    /// Loop start/end in ticks (from the `[` / `]` markers).
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
    pub tracks: Vec<Vec<Event>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub tick: u32,
    pub kind: EventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// dur is in ticks; velocity already through mid2agb's LUT quantisation.
    Note { key: u8, vel: u8, dur: u32 },
    Program(u8),
    /// Pitch bend, MP2K range -64..63.
    Bend(i8),
    /// Track volume 0-127 (master volume already folded in by the extractor,
    /// exactly like mid2agb's -V).
    Volume(u8),
    /// Pan -64..63, 0 = centre.
    Pan(i8),
    /// Modulation (vibrato) depth.
    Mod(u8),
    BendRange(u8),
    LfoSpeed(u8),
}

#[derive(Debug)]
pub struct PackError(pub &'static str);

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "music pack: {}", self.0)
    }
}
impl std::error::Error for PackError {}

impl MusicPack {
    /// A pack holding only `song_indices` and what they can reach: the
    /// voicegroup closure (through Keysplit/KeysplitAll sub-groups) plus the
    /// samples, waves and keysplit tables those groups reference — indices
    /// remapped. This is how the web build streams one song at a time
    /// instead of shipping the whole bank up front.
    pub fn subset(&self, song_indices: &[usize]) -> MusicPack {
        let mut vg_used = vec![false; self.voicegroups.len()];
        let mut stack: Vec<usize> =
            song_indices.iter().map(|&s| self.songs[s].voicegroup as usize).collect();
        while let Some(g) = stack.pop() {
            if std::mem::replace(&mut vg_used[g], true) {
                continue;
            }
            for v in &self.voicegroups[g].entries {
                match *v {
                    Voice::Keysplit { group, .. } | Voice::KeysplitAll { group } => {
                        stack.push(group as usize)
                    }
                    _ => {}
                }
            }
        }
        let mut sm_used = vec![false; self.samples.len()];
        let mut wv_used = vec![false; self.waves.len()];
        let mut ks_used = vec![false; self.keysplits.len()];
        for (g, used) in self.voicegroups.iter().zip(&vg_used) {
            if !used {
                continue;
            }
            for v in &g.entries {
                match *v {
                    Voice::DirectSound { sample, .. } => sm_used[sample as usize] = true,
                    Voice::Wave { wave, .. } => wv_used[wave as usize] = true,
                    Voice::Keysplit { table, .. } => ks_used[table as usize] = true,
                    _ => {}
                }
            }
        }
        // old index → new index, preserving order
        let remap = |used: &[bool]| -> Vec<u16> {
            let mut n = 0u16;
            used.iter().map(|&u| if u { n += 1; n - 1 } else { 0 }).collect()
        };
        let (sm, wv, ks, vg) =
            (remap(&sm_used), remap(&wv_used), remap(&ks_used), remap(&vg_used));
        MusicPack {
            samples: self
                .samples
                .iter()
                .enumerate()
                .filter(|(i, _)| sm_used[*i])
                .map(|(_, s)| s.clone())
                .collect(),
            waves: self
                .waves
                .iter()
                .enumerate()
                .filter(|(i, _)| wv_used[*i])
                .map(|(_, w)| *w)
                .collect(),
            keysplits: self
                .keysplits
                .iter()
                .enumerate()
                .filter(|(i, _)| ks_used[*i])
                .map(|(_, k)| k.clone())
                .collect(),
            voicegroups: self
                .voicegroups
                .iter()
                .enumerate()
                .filter(|(i, _)| vg_used[*i])
                .map(|(_, g)| VoiceGroup {
                    name: g.name.clone(),
                    offset: g.offset,
                    entries: g
                        .entries
                        .iter()
                        .map(|v| match *v {
                            Voice::DirectSound { base_key, pan, sample, adsr, fixed } => {
                                Voice::DirectSound {
                                    base_key,
                                    pan,
                                    sample: sm[sample as usize],
                                    adsr,
                                    fixed,
                                }
                            }
                            Voice::Wave { base_key, pan, wave, adsr } => {
                                Voice::Wave { base_key, pan, wave: wv[wave as usize], adsr }
                            }
                            Voice::Keysplit { group, table } => Voice::Keysplit {
                                group: vg[group as usize],
                                table: ks[table as usize],
                            },
                            Voice::KeysplitAll { group } => {
                                Voice::KeysplitAll { group: vg[group as usize] }
                            }
                            other => other,
                        })
                        .collect(),
                })
                .collect(),
            songs: song_indices
                .iter()
                .map(|&s| {
                    let mut song = self.songs[s].clone();
                    song.voicegroup = vg[song.voicegroup as usize];
                    song
                })
                .collect(),
        }
    }
}

// ── writing ──────────────────────────────────────────────────────────────

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn i8(&mut self, v: i8) {
        self.0.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn opt_u32(&mut self, v: Option<u32>) {
        match v {
            None => self.u32(u32::MAX),
            Some(x) => self.u32(x),
        }
    }
    fn str(&mut self, v: &str) {
        self.u32(v.len() as u32);
        self.0.extend_from_slice(v.as_bytes());
    }
}

impl MusicPack {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = W(Vec::new());
        w.0.extend_from_slice(MAGIC);

        w.u32(self.samples.len() as u32);
        for s in &self.samples {
            w.str(&s.name);
            w.f32(s.rate);
            w.opt_u32(s.loop_start);
            w.u32(s.pcm.len() as u32);
            w.0.extend(s.pcm.iter().map(|&b| b as u8));
        }

        w.u32(self.waves.len() as u32);
        for wave in &self.waves {
            w.0.extend_from_slice(wave);
        }

        w.u32(self.keysplits.len() as u32);
        for k in &self.keysplits {
            w.str(&k.name);
            w.0.extend_from_slice(&k.map);
        }

        w.u32(self.voicegroups.len() as u32);
        for g in &self.voicegroups {
            w.str(&g.name);
            w.u16(g.offset);
            w.u32(g.entries.len() as u32);
            for v in &g.entries {
                match *v {
                    Voice::DirectSound { base_key, pan, sample, adsr, fixed } => {
                        w.u8(0);
                        w.u8(base_key);
                        w.u8(pan);
                        w.u16(sample);
                        w.0.extend_from_slice(&adsr);
                        w.u8(fixed as u8);
                    }
                    Voice::Square { base_key, pan, duty, sweep, adsr } => {
                        w.u8(1);
                        w.u8(base_key);
                        w.u8(pan);
                        w.u8(duty);
                        w.u8(sweep);
                        w.0.extend_from_slice(&adsr);
                    }
                    Voice::Wave { base_key, pan, wave, adsr } => {
                        w.u8(2);
                        w.u8(base_key);
                        w.u8(pan);
                        w.u16(wave);
                        w.0.extend_from_slice(&adsr);
                    }
                    Voice::Noise { base_key, pan, period, adsr } => {
                        w.u8(3);
                        w.u8(base_key);
                        w.u8(pan);
                        w.u8(period);
                        w.0.extend_from_slice(&adsr);
                    }
                    Voice::Keysplit { group, table } => {
                        w.u8(4);
                        w.u16(group);
                        w.u16(table);
                    }
                    Voice::KeysplitAll { group } => {
                        w.u8(5);
                        w.u16(group);
                    }
                    Voice::Unsupported => w.u8(6),
                }
            }
        }

        w.u32(self.songs.len() as u32);
        for s in &self.songs {
            w.str(&s.name);
            w.u16(s.voicegroup);
            w.u8(s.master_volume);
            w.u8(s.reverb);
            w.u16(s.division);
            w.u32(s.tempos.len() as u32);
            for &(t, us) in &s.tempos {
                w.u32(t);
                w.u32(us);
            }
            w.opt_u32(s.loop_start);
            w.opt_u32(s.loop_end);
            w.u32(s.tracks.len() as u32);
            for track in &s.tracks {
                w.u32(track.len() as u32);
                for e in track {
                    w.u32(e.tick);
                    match e.kind {
                        EventKind::Note { key, vel, dur } => {
                            w.u8(0);
                            w.u8(key);
                            w.u8(vel);
                            w.u32(dur);
                        }
                        EventKind::Program(v) => {
                            w.u8(1);
                            w.u8(v);
                        }
                        EventKind::Bend(v) => {
                            w.u8(2);
                            w.i8(v);
                        }
                        EventKind::Volume(v) => {
                            w.u8(3);
                            w.u8(v);
                        }
                        EventKind::Pan(v) => {
                            w.u8(4);
                            w.i8(v);
                        }
                        EventKind::Mod(v) => {
                            w.u8(5);
                            w.u8(v);
                        }
                        EventKind::BendRange(v) => {
                            w.u8(6);
                            w.u8(v);
                        }
                        EventKind::LfoSpeed(v) => {
                            w.u8(7);
                            w.u8(v);
                        }
                    }
                }
            }
        }

        w.0
    }
}

// ── reading ──────────────────────────────────────────────────────────────

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], PackError> {
        let s = self.b.get(self.p..self.p + n).ok_or(PackError("truncated"))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, PackError> {
        Ok(self.take(1)?[0])
    }
    fn i8(&mut self) -> Result<i8, PackError> {
        Ok(self.take(1)?[0] as i8)
    }
    fn u16(&mut self) -> Result<u16, PackError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, PackError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, PackError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn opt_u32(&mut self) -> Result<Option<u32>, PackError> {
        Ok(match self.u32()? {
            u32::MAX => None,
            v => Some(v),
        })
    }
    fn len(&mut self) -> Result<usize, PackError> {
        let n = self.u32()? as usize;
        if n > self.b.len() - self.p {
            return Err(PackError("length exceeds data"));
        }
        Ok(n)
    }
    fn str(&mut self) -> Result<String, PackError> {
        let n = self.len()?;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| PackError("bad utf8"))
    }
}

impl MusicPack {
    pub fn from_bytes(b: &[u8]) -> Result<MusicPack, PackError> {
        let mut r = R { b, p: 0 };
        if r.take(8)? != MAGIC {
            return Err(PackError("bad magic (regenerate with musicgen)"));
        }

        let mut samples = Vec::new();
        for _ in 0..r.u32()? {
            let name = r.str()?;
            let rate = r.f32()?;
            let loop_start = r.opt_u32()?;
            let n = r.len()?;
            let pcm = r.take(n)?.iter().map(|&b| b as i8).collect();
            samples.push(SampleData { name, rate, loop_start, pcm });
        }

        let mut waves = Vec::new();
        for _ in 0..r.u32()? {
            waves.push(r.take(32)?.try_into().unwrap());
        }

        let mut keysplits = Vec::new();
        for _ in 0..r.u32()? {
            let name = r.str()?;
            let map = r.take(128)?.try_into().unwrap();
            keysplits.push(KeysplitTable { name, map });
        }

        let mut voicegroups = Vec::new();
        for _ in 0..r.u32()? {
            let name = r.str()?;
            let offset = r.u16()?;
            let mut entries = Vec::new();
            for _ in 0..r.u32()? {
                entries.push(match r.u8()? {
                    0 => Voice::DirectSound {
                        base_key: r.u8()?,
                        pan: r.u8()?,
                        sample: r.u16()?,
                        adsr: r.take(4)?.try_into().unwrap(),
                        fixed: r.u8()? != 0,
                    },
                    1 => Voice::Square {
                        base_key: r.u8()?,
                        pan: r.u8()?,
                        duty: r.u8()?,
                        sweep: r.u8()?,
                        adsr: r.take(4)?.try_into().unwrap(),
                    },
                    2 => Voice::Wave {
                        base_key: r.u8()?,
                        pan: r.u8()?,
                        wave: r.u16()?,
                        adsr: r.take(4)?.try_into().unwrap(),
                    },
                    3 => Voice::Noise {
                        base_key: r.u8()?,
                        pan: r.u8()?,
                        period: r.u8()?,
                        adsr: r.take(4)?.try_into().unwrap(),
                    },
                    4 => Voice::Keysplit { group: r.u16()?, table: r.u16()? },
                    5 => Voice::KeysplitAll { group: r.u16()? },
                    6 => Voice::Unsupported,
                    _ => return Err(PackError("bad voice tag")),
                });
            }
            voicegroups.push(VoiceGroup { name, offset, entries });
        }

        let mut songs = Vec::new();
        for _ in 0..r.u32()? {
            let name = r.str()?;
            let voicegroup = r.u16()?;
            let master_volume = r.u8()?;
            let reverb = r.u8()?;
            let division = r.u16()?;
            let mut tempos = Vec::new();
            for _ in 0..r.u32()? {
                tempos.push((r.u32()?, r.u32()?));
            }
            let loop_start = r.opt_u32()?;
            let loop_end = r.opt_u32()?;
            let mut tracks = Vec::new();
            for _ in 0..r.u32()? {
                let mut track = Vec::new();
                for _ in 0..r.u32()? {
                    let tick = r.u32()?;
                    let kind = match r.u8()? {
                        0 => EventKind::Note { key: r.u8()?, vel: r.u8()?, dur: r.u32()? },
                        1 => EventKind::Program(r.u8()?),
                        2 => EventKind::Bend(r.i8()?),
                        3 => EventKind::Volume(r.u8()?),
                        4 => EventKind::Pan(r.i8()?),
                        5 => EventKind::Mod(r.u8()?),
                        6 => EventKind::BendRange(r.u8()?),
                        7 => EventKind::LfoSpeed(r.u8()?),
                        _ => return Err(PackError("bad event tag")),
                    };
                    track.push(Event { tick, kind });
                }
                tracks.push(track);
            }
            songs.push(SongData {
                name,
                voicegroup,
                master_volume,
                reverb,
                division,
                tempos,
                loop_start,
                loop_end,
                tracks,
            });
        }

        Ok(MusicPack { samples, waves, keysplits, voicegroups, songs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pack = MusicPack {
            samples: vec![SampleData {
                name: "glockenspiel".into(),
                rate: 13379.0,
                loop_start: Some(42),
                pcm: vec![-128, 0, 127],
            }],
            waves: vec![[7u8; 32]],
            keysplits: vec![KeysplitTable { name: "keysplit_piano".into(), map: [3; 128] }],
            voicegroups: vec![VoiceGroup {
                name: "cycling".into(),
                offset: 0,
                entries: vec![
                    Voice::KeysplitAll { group: 0 },
                    Voice::Keysplit { group: 0, table: 0 },
                    Voice::Square { base_key: 60, pan: 0, duty: 2, sweep: 0, adsr: [0, 0, 15, 0] },
                    Voice::DirectSound {
                        base_key: 60,
                        pan: 0,
                        sample: 0,
                        adsr: [255, 165, 51, 242],
                        fixed: false,
                    },
                    Voice::Wave { base_key: 60, pan: 0, wave: 0, adsr: [0, 0, 15, 0] },
                    Voice::Noise { base_key: 60, pan: 0, period: 1, adsr: [0, 0, 15, 0] },
                    Voice::Unsupported,
                ],
            }],
            songs: vec![SongData {
                name: "mus_cycling".into(),
                voicegroup: 0,
                master_volume: 83,
                reverb: 50,
                division: 24,
                tempos: vec![(0, 500000)],
                loop_start: Some(0),
                loop_end: Some(9600),
                tracks: vec![vec![
                    Event { tick: 0, kind: EventKind::Program(1) },
                    Event { tick: 0, kind: EventKind::Volume(90) },
                    Event { tick: 4, kind: EventKind::Note { key: 60, vel: 100, dur: 24 } },
                    Event { tick: 30, kind: EventKind::Bend(-32) },
                    Event { tick: 31, kind: EventKind::Pan(-64) },
                    Event { tick: 32, kind: EventKind::Mod(5) },
                    Event { tick: 33, kind: EventKind::BendRange(12) },
                    Event { tick: 34, kind: EventKind::LfoSpeed(44) },
                ]],
            }],
        };
        let bytes = pack.to_bytes();
        assert_eq!(MusicPack::from_bytes(&bytes).unwrap(), pack);
        assert!(MusicPack::from_bytes(&bytes[..bytes.len() - 2]).is_err());
    }

    #[test]
    fn subset_keeps_reachable_data_and_remaps_indices() {
        // two of everything; song 1 reaches only the SECOND of each via a
        // keysplit chain, so the subset must drop the firsts and remap to 0
        let sample = |name: &str| SampleData {
            name: name.into(),
            rate: 8000.0,
            loop_start: None,
            pcm: vec![1, 2, 3],
        };
        let song = |name: &str, vg: u16| SongData {
            name: name.into(),
            voicegroup: vg,
            master_volume: 127,
            reverb: 0,
            division: 24,
            tempos: vec![(0, 500000)],
            loop_start: None,
            loop_end: None,
            tracks: vec![vec![Event {
                tick: 0,
                kind: EventKind::Note { key: 60, vel: 100, dur: 24 },
            }]],
        };
        let pack = MusicPack {
            samples: vec![sample("unused"), sample("used")],
            waves: vec![[0u8; 32], [7u8; 32]],
            keysplits: vec![
                KeysplitTable { name: "unused".into(), map: [0; 128] },
                KeysplitTable { name: "used".into(), map: [1; 128] },
            ],
            voicegroups: vec![
                VoiceGroup {
                    name: "unused".into(),
                    offset: 0,
                    entries: vec![Voice::DirectSound {
                        base_key: 60,
                        pan: 0,
                        sample: 0,
                        adsr: [0; 4],
                        fixed: false,
                    }],
                },
                VoiceGroup {
                    name: "sub".into(),
                    offset: 0,
                    entries: vec![
                        Voice::DirectSound {
                            base_key: 60,
                            pan: 0,
                            sample: 1,
                            adsr: [0; 4],
                            fixed: false,
                        },
                        Voice::Wave { base_key: 60, pan: 0, wave: 1, adsr: [0; 4] },
                    ],
                },
                VoiceGroup {
                    name: "main".into(),
                    offset: 0,
                    entries: vec![Voice::Keysplit { group: 1, table: 1 }],
                },
            ],
            songs: vec![song("mus_unused", 0), song("mus_used", 2)],
        };
        let sub = pack.subset(&[1]);
        assert_eq!(sub.songs.len(), 1);
        assert_eq!(sub.songs[0].name, "mus_used");
        assert_eq!(sub.samples.len(), 1);
        assert_eq!(sub.samples[0].name, "used");
        assert_eq!(sub.waves, vec![[7u8; 32]]);
        assert_eq!(sub.keysplits.len(), 1);
        assert_eq!(sub.voicegroups.len(), 2, "closure keeps main + sub");
        // remapped: song → "main" (now 1), whose keysplit → "sub" (now 0)
        assert_eq!(sub.songs[0].voicegroup, 1);
        assert_eq!(sub.voicegroups[1].entries[0], Voice::Keysplit { group: 0, table: 0 });
        assert_eq!(
            sub.voicegroups[0].entries[0],
            Voice::DirectSound { base_key: 60, pan: 0, sample: 0, adsr: [0; 4], fixed: false }
        );
        // the subset is a valid pack in its own right
        assert_eq!(MusicPack::from_bytes(&sub.to_bytes()).unwrap(), sub);
    }
}
