//! musicgen — from-scratch pret → music.bin extraction.
//!
//! Reads a pret/pokeemerald source clone (never the ROM, never
//! pokeemerald_SDL3's music.pak — that pak is the *output* of the old buggy
//! flattening) and writes the MusicPack: raw voicegroups with keysplit and
//! drumset entries intact, keysplit tables, the 8-bit sample bank from the
//! AIFFs, PSG wave patterns, and every song as a tick-exact event stream from
//! pret's machine-derived 24-tpqn MIDIs (options from midi.cfg, velocities
//! through mid2agb's LUT, master volume folded like mid2agb's -V).
//!
//! Output is split for streaming: one small always-resident pack of every
//! se_* sound effect, plus one self-contained pack per mus_* song (its event
//! stream and the voicegroup/sample closure it reaches) — the browser fetches
//! a song only when the map asks for it, instead of the whole bank up front.
//!
//! Usage: cargo run -p musicgen -- [path-to-pret-clone] [output-dir]
//! Defaults: ~/pokeemerald  assets/music

use audio::data::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn main() {
    let mut args = std::env::args().skip(1);
    let pret = PathBuf::from(
        args.next().unwrap_or_else(|| format!("{}/pokeemerald", std::env::var("HOME").unwrap())),
    );
    let out = PathBuf::from(args.next().unwrap_or_else(|| "assets/music".into()));

    let pack = extract(&pret);
    std::fs::create_dir_all(&out).unwrap();
    // wipe stale packs so renamed/removed songs don't leave orphans
    for e in std::fs::read_dir(&out).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "bin") {
            std::fs::remove_file(p).unwrap();
        }
    }

    let sfx: Vec<usize> = (0..pack.songs.len())
        .filter(|&i| pack.songs[i].name.starts_with("se_"))
        .collect();
    let sfx_bytes = pack.subset(&sfx).to_bytes();
    std::fs::write(out.join("sfx.bin"), &sfx_bytes).unwrap();

    let (mut n, mut total, mut largest) = (0usize, sfx_bytes.len(), 0usize);
    for i in 0..pack.songs.len() {
        let name = &pack.songs[i].name;
        if !name.starts_with("mus_") {
            continue;
        }
        let bytes = pack.subset(&[i]).to_bytes();
        std::fs::write(out.join(format!("{name}.bin")), &bytes).unwrap();
        n += 1;
        total += bytes.len();
        largest = largest.max(bytes.len());
    }
    println!(
        "wrote {}: sfx.bin ({} KB, {} effects) + {n} song packs \
         ({:.1} MB total, largest {} KB)",
        out.display(),
        sfx_bytes.len() / 1024,
        sfx.len(),
        total as f64 / 1e6,
        largest / 1024,
    );
}

fn extract(pret: &Path) -> MusicPack {
    // ── keysplit tables ───────────────────────────────────────────────────
    // The tables are contiguous bytes in ROM and each label is offset
    // backwards by its base key (see the comment atop keysplit_tables.inc),
    // so a lookup with a key outside a table's range reads the NEIGHBORING
    // table's bytes — deterministic, and some songs rely on it. Reproduce it
    // exactly: build the flat blob, then fill each table's full 0..128 map
    // through the same address math the GBA uses.
    let mut keysplits: Vec<KeysplitTable> = Vec::new();
    let mut ks_index: HashMap<String, u16> = HashMap::new();
    {
        let text = read(pret, "sound/keysplit_tables.inc");
        let mut blob: Vec<u8> = Vec::new();
        let mut tables: Vec<(String, usize, usize)> = Vec::new(); // (name, base, start)
        for line in text.lines() {
            let line = line.split('@').next().unwrap().trim();
            if let Some(rest) = line.strip_prefix("keysplit ") {
                let mut it = rest.split(',').map(str::trim);
                let name = it.next().unwrap().to_string();
                let base = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
                tables.push((name, base, blob.len()));
            } else if let Some(rest) = line.strip_prefix("split ") {
                let (_, base, start) = tables.last().expect("split outside keysplit");
                let mut it = rest.split(',').map(str::trim);
                let idx: u8 = it.next().unwrap().parse().unwrap();
                let end: usize = it.next().unwrap().parse().unwrap();
                let next_key = base + (blob.len() - start);
                blob.extend(std::iter::repeat_n(idx, end + 1 - next_key));
            }
        }
        for (name, base, start) in tables {
            let mut map = [0xFFu8; 128];
            for (k, m) in map.iter_mut().enumerate() {
                if let Some(pos) = (start + k).checked_sub(base) {
                    if let Some(&v) = blob.get(pos) {
                        *m = v;
                    }
                }
            }
            ks_index.insert(name.clone(), keysplits.len() as u16);
            keysplits.push(KeysplitTable { name, map });
        }
    }

    // ── sample + wave label → file maps ──────────────────────────────────
    let ds_files = parse_incbin_labels(&read(pret, "sound/direct_sound_data.inc"));
    let wave_files = parse_incbin_labels(&read(pret, "sound/programmable_wave_data.inc"));

    let mut samples: Vec<SampleData> = Vec::new();
    let mut sample_index: HashMap<String, u16> = HashMap::new();
    let mut waves: Vec<[u8; 32]> = Vec::new();
    let mut wave_index: HashMap<String, u16> = HashMap::new();

    // ── voicegroups: two passes (names first, then entries) ──────────────
    struct RawGroup {
        name: String,
        offset: u16,
        entries: Vec<(String, Vec<String>)>,
    }
    let mut raw_groups: Vec<RawGroup> = Vec::new();
    let mut vg_index: HashMap<String, u16> = HashMap::new();
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_inc_files(&pret.join("sound/voicegroups"), &mut paths);
    paths.sort();
    for p in &paths {
        let text = std::fs::read_to_string(p).unwrap();
        let mut name = None;
        let mut offset = 0u16;
        let mut entries = Vec::new();
        for line in text.lines() {
            let line = line.split('@').next().unwrap().trim();
            if let Some(rest) = line.strip_prefix("voice_group ") {
                let mut it = rest.split(',').map(str::trim);
                name = Some(it.next().unwrap().to_string());
                offset = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            } else if line.starts_with("voice_") || line.starts_with("cry") {
                let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
                let args = rest.split(',').map(|a| a.trim().to_string()).collect();
                entries.push((kind.to_string(), args));
            }
        }
        if let Some(name) = name {
            vg_index.insert(name.clone(), raw_groups.len() as u16);
            raw_groups.push(RawGroup { name, offset, entries });
        }
    }

    let voicegroups: Vec<VoiceGroup> = raw_groups
        .iter()
        .map(|g| VoiceGroup {
            name: g.name.clone(),
            offset: g.offset,
            entries: g
                .entries
                .iter()
                .map(|(kind, args)| {
                    parse_voice(
                        pret,
                        kind,
                        args,
                        &vg_index,
                        &ks_index,
                        &ds_files,
                        &wave_files,
                        &mut samples,
                        &mut sample_index,
                        &mut waves,
                        &mut wave_index,
                    )
                })
                .collect(),
        })
        .collect();

    // ── songs from midi.cfg + the 24-tpqn MIDIs ───────────────────────────
    let mut songs = Vec::new();
    let mut skipped = 0;
    for (name, group, mvl, reverb) in parse_midi_cfg(&read(pret, "sound/songs/midi/midi.cfg")) {
        // mus_* = BGM; se_* = sound effects (door, exit, ...): same engine
        if !name.starts_with("mus_") && !name.starts_with("se_") {
            continue;
        }
        let Some(&voicegroup) = vg_index.get(&group) else {
            skipped += 1;
            continue;
        };
        let midi = pret.join(format!("sound/songs/midi/{name}.mid"));
        let Ok(bytes) = std::fs::read(&midi) else {
            skipped += 1;
            continue;
        };
        match parse_midi(&bytes, mvl) {
            Some((division, tempos, loop_start, loop_end, tracks)) if !tracks.is_empty() => {
                songs.push(SongData {
                    name,
                    voicegroup,
                    master_volume: mvl,
                    reverb,
                    division,
                    tempos,
                    loop_start,
                    loop_end,
                    tracks,
                });
            }
            _ => skipped += 1,
        }
    }
    println!("songs extracted: {}, skipped: {skipped}", songs.len());

    MusicPack { samples, waves, keysplits, voicegroups, songs }
}

#[allow(clippy::too_many_arguments)]
fn parse_voice(
    pret: &Path,
    kind: &str,
    args: &[String],
    vg_index: &HashMap<String, u16>,
    ks_index: &HashMap<String, u16>,
    ds_files: &HashMap<String, String>,
    wave_files: &HashMap<String, String>,
    samples: &mut Vec<SampleData>,
    sample_index: &mut HashMap<String, u16>,
    waves: &mut Vec<[u8; 32]>,
    wave_index: &mut HashMap<String, u16>,
) -> Voice {
    let int = |i: usize| -> Option<u8> { args.get(i)?.parse().ok() };
    let adsr = |from: usize| -> Option<[u8; 4]> {
        Some([int(from)?, int(from + 1)?, int(from + 2)?, int(from + 3)?])
    };
    let voice = (|| -> Option<Voice> {
        if kind.starts_with("voice_keysplit_all") {
            let group = *vg_index.get(args.first()?.trim_start_matches("voicegroup_"))?;
            return Some(Voice::KeysplitAll { group });
        }
        if kind.starts_with("voice_keysplit") {
            let group = *vg_index.get(args.first()?.trim_start_matches("voicegroup_"))?;
            let table = *ks_index.get(args.get(1)?.trim_start_matches("keysplit_"))?;
            return Some(Voice::Keysplit { group, table });
        }
        if kind.starts_with("voice_directsound") {
            let label = args.get(2)?;
            let sample = match sample_index.get(label) {
                Some(&i) => i,
                None => {
                    let rel = ds_files.get(label)?;
                    let wav = pret.join(rel.replace(".bin", ".wav"));
                    let s = parse_wav(&wav, label)?;
                    samples.push(s);
                    let i = samples.len() as u16 - 1;
                    sample_index.insert(label.clone(), i);
                    i
                }
            };
            return Some(Voice::DirectSound {
                base_key: int(0)?,
                pan: int(1)?,
                sample,
                adsr: adsr(3)?,
                fixed: kind.contains("no_resample"),
            });
        }
        if kind.starts_with("voice_square_1") {
            return Some(Voice::Square {
                base_key: int(0)?,
                pan: int(1)?,
                sweep: int(2)?,
                duty: int(3)? & 3,
                adsr: adsr(4)?,
            });
        }
        if kind.starts_with("voice_square_2") {
            return Some(Voice::Square {
                base_key: int(0)?,
                pan: int(1)?,
                sweep: 0,
                duty: int(2)? & 3,
                adsr: adsr(3)?,
            });
        }
        if kind.starts_with("voice_programmable_wave") {
            let label = args.get(2)?;
            let wave = match wave_index.get(label) {
                Some(&i) => i,
                None => {
                    let raw = std::fs::read(pret.join(wave_files.get(label)?)).ok()?;
                    let mut pat = [0u8; 32];
                    for (i, &b) in raw.iter().take(16).enumerate() {
                        pat[i * 2] = b >> 4; // high nibble first
                        pat[i * 2 + 1] = b & 0xF;
                    }
                    waves.push(pat);
                    let i = waves.len() as u16 - 1;
                    wave_index.insert(label.clone(), i);
                    i
                }
            };
            return Some(Voice::Wave { base_key: int(0)?, pan: int(1)?, wave, adsr: adsr(3)? });
        }
        if kind.starts_with("voice_noise") {
            return Some(Voice::Noise {
                base_key: int(0)?,
                pan: int(1)?,
                period: int(2)? & 1,
                adsr: adsr(3)?,
            });
        }
        None // cries etc.
    })();
    voice.unwrap_or(Voice::Unsupported)
}

// ── file format helpers ──────────────────────────────────────────────────

fn read(pret: &Path, rel: &str) -> String {
    std::fs::read_to_string(pret.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn collect_inc_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            collect_inc_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "inc") {
            out.push(p);
        }
    }
}

/// `Label::` + `.incbin "path"` pairs (either may carry a trailing @comment).
fn parse_incbin_labels(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut label: Option<String> = None;
    for line in text.lines() {
        let line = line.split('@').next().unwrap().trim();
        if let Some(l) = line.strip_suffix("::") {
            label = Some(l.to_string());
        } else if let Some(rest) = line.strip_prefix(".incbin ") {
            if let (Some(l), Some(path)) = (label.take(), rest.split('"').nth(1)) {
                out.insert(l, path.to_string());
            }
        }
    }
    out
}

/// midi.cfg lines: `name.mid:  -E -R50 -G_cycling -V083` → (name, group, mvl, reverb).
fn parse_midi_cfg(text: &str) -> Vec<(String, String, u8, u8)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((file, opts)) = line.split_once(':') else { continue };
        let Some(name) = file.trim().strip_suffix(".mid") else { continue };
        let mut group = None;
        let mut mvl = 127u8;
        let mut reverb = 0u8;
        for opt in opts.split_whitespace() {
            if let Some(g) = opt.strip_prefix("-G") {
                group = Some(g.trim_start_matches('_').to_string());
            } else if let Some(v) = opt.strip_prefix("-V") {
                mvl = v.parse().unwrap_or(127);
            } else if let Some(r) = opt.strip_prefix("-R") {
                reverb = r.parse().unwrap_or(0);
            }
        }
        if let Some(group) = group {
            out.push((name.to_string(), group, mvl, reverb));
        }
    }
    out
}

/// WAV, exactly as pret's wav2agb reads it: 8-bit unsigned mono PCM; `smpl`
/// gives the loop (end stored inclusive, +1); the custom `agbp` chunk is the
/// exact GBA pitch (rate * 1024) and `agbl` the exact loop end — both beat
/// the imprecise header values. Data past the loop end never plays on the
/// GBA (wav2agb trims it), so we trim too.
fn parse_wav(path: &Path, name: &str) -> Option<SampleData> {
    let d = std::fs::read(path).ok()?;
    if d.len() < 12 || &d[..4] != b"RIFF" || &d[8..12] != b"WAVE" {
        return None;
    }
    let leu16 = |i: usize| u16::from_le_bytes(d[i..i + 2].try_into().unwrap());
    let leu32 = |i: usize| u32::from_le_bytes(d[i..i + 4].try_into().unwrap());

    let mut rate: Option<f32> = None;
    let mut pcm: Option<Vec<i8>> = None;
    let mut loop_start: Option<u32> = None;
    let mut loop_end: Option<u32> = None;

    let mut i = 12;
    while i + 8 <= d.len() {
        let cid = &d[i..i + 4];
        let sz = leu32(i + 4) as usize;
        let b = i + 8;
        if b + sz > d.len() {
            return None;
        }
        match cid {
            b"fmt " => {
                let (format, channels, bits) = (leu16(b), leu16(b + 2), leu16(b + 14));
                if format != 1 || channels != 1 || bits != 8 {
                    return None; // wav2agb only accepts this shape too
                }
                rate.get_or_insert(leu32(b + 4) as f32);
            }
            b"smpl" => {
                let num_loops = leu32(b + 28);
                if num_loops > 0 && sz >= 36 + 24 && leu32(b + 36 + 4) == 0 {
                    loop_start = Some(leu32(b + 36 + 8));
                    loop_end = Some(leu32(b + 36 + 12) + 1);
                }
            }
            b"agbp" if sz >= 4 => rate = Some(leu32(b) as f32 / 1024.0),
            b"agbl" if sz >= 4 => loop_end = Some(leu32(b)),
            b"data" => {
                // 8-bit WAV is unsigned; the GBA (and this pack) is signed
                pcm = Some(d[b..b + sz].iter().map(|&x| (x as i16 - 128) as i8).collect());
            }
            _ => {}
        }
        i = b + sz + sz % 2;
    }

    let rate = rate?;
    let mut pcm = pcm?;
    if let Some(end) = loop_end {
        pcm.truncate(end as usize);
    }
    Some(SampleData { name: name.to_string(), rate, loop_start, pcm })
}

/// mid2agb's g_noteVelocityLUT quantisation.
fn vel_lut(v: u8) -> u8 {
    if v == 0 {
        0
    } else {
        ((v as u16).div_ceil(4) * 4).min(127) as u8
    }
}

/// Parse an SMF; returns (division, tempos, loop start/end ticks, tracks).
/// Master volume `mvl` is folded into volume events exactly like mid2agb -V.
type ParsedMidi = (u16, Vec<(u32, u32)>, Option<u32>, Option<u32>, Vec<Vec<Event>>);

fn parse_midi(d: &[u8], mvl: u8) -> Option<ParsedMidi> {
    if d.len() < 14 || &d[..4] != b"MThd" {
        return None;
    }
    let ntr = u16::from_be_bytes([d[10], d[11]]);
    let division = u16::from_be_bytes([d[12], d[13]]);
    let mut i = 14usize;
    let mut tempos: Vec<(u32, u32)> = Vec::new();
    let (mut loop_start, mut loop_end) = (None, None);
    let mut tracks: Vec<Vec<Event>> = Vec::new();

    for _ in 0..ntr {
        if d.get(i..i + 4)? != b"MTrk" {
            return None;
        }
        let len = u32::from_be_bytes(d[i + 4..i + 8].try_into().unwrap()) as usize;
        let mut j = i + 8;
        let end = j + len;
        let mut tick = 0u32;
        let mut running = 0u8;
        let mut events: Vec<Event> = Vec::new();
        // note-on awaiting its note-off: key → (slot in events)
        let mut open: HashMap<u8, Vec<usize>> = HashMap::new();
        let mut xcmd = 0xFFu8;

        while j < end {
            let (delta, nj) = read_varlen(d, j)?;
            j = nj;
            tick += delta;
            let mut st = *d.get(j)?;
            if st & 0x80 != 0 {
                j += 1;
                running = st;
            } else {
                st = running;
            }
            match st {
                0xFF => {
                    let mt = *d.get(j)?;
                    let l = *d.get(j + 1)? as usize;
                    let body = d.get(j + 2..j + 2 + l)?;
                    j += 2 + l;
                    if mt == 0x51 && l == 3 {
                        tempos.push((
                            tick,
                            ((body[0] as u32) << 16) | ((body[1] as u32) << 8) | body[2] as u32,
                        ));
                    } else if mt == 0x06 {
                        if body == b"[" {
                            loop_start = Some(tick);
                        } else if body == b"]" {
                            loop_end = Some(tick);
                        }
                    }
                }
                0xF0 | 0xF7 => {
                    let (l, nj) = read_varlen(d, j)?;
                    j = nj + l as usize;
                }
                _ => {
                    let hi = st & 0xF0;
                    if hi == 0xC0 || hi == 0xD0 {
                        let p1 = *d.get(j)?;
                        j += 1;
                        if hi == 0xC0 {
                            events.push(Event { tick, kind: EventKind::Program(p1) });
                        }
                    } else {
                        let p1 = *d.get(j)?;
                        let p2 = *d.get(j + 1)?;
                        j += 2;
                        match hi {
                            0x90 if p2 > 0 => {
                                open.entry(p1).or_default().push(events.len());
                                // dur patched at the matching note-off
                                events.push(Event {
                                    tick,
                                    kind: EventKind::Note { key: p1, vel: vel_lut(p2), dur: 0 },
                                });
                            }
                            0x80 | 0x90 => {
                                if let Some(slot) =
                                    open.get_mut(&p1).and_then(|v| (!v.is_empty()).then(|| v.remove(0)))
                                {
                                    let start = events[slot].tick;
                                    if let EventKind::Note { dur, .. } = &mut events[slot].kind {
                                        *dur = tick - start;
                                    }
                                }
                            }
                            0xB0 => {
                                let kind = match p1 {
                                    7 => Some(EventKind::Volume(
                                        (p2 as u16 * mvl as u16 / 127) as u8,
                                    )),
                                    10 => Some(EventKind::Pan(p2 as i8 - 64)),
                                    1 => Some(EventKind::Mod(p2)),
                                    20 => Some(EventKind::BendRange(p2)),
                                    21 => Some(EventKind::LfoSpeed(p2)),
                                    30 => {
                                        xcmd = p2;
                                        None
                                    }
                                    29 => {
                                        // pseudo-echo XCMDs — not played (yet)
                                        let _ = xcmd;
                                        None
                                    }
                                    _ => None,
                                };
                                if let Some(kind) = kind {
                                    events.push(Event { tick, kind });
                                }
                            }
                            0xE0 => {
                                let raw = ((p2 as i32) << 7 | p1 as i32) - 8192;
                                events.push(Event {
                                    tick,
                                    kind: EventKind::Bend((raw / 128) as i8),
                                });
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        // keep only tracks that make sound
        if events.iter().any(|e| matches!(e.kind, EventKind::Note { .. })) {
            events.retain(|e| !matches!(e.kind, EventKind::Note { dur: 0, .. }));
            tracks.push(events);
        }
        i = end;
    }

    if tempos.is_empty() {
        tempos.push((0, 500000));
    }
    tempos.sort();
    Some((division, tempos, loop_start, loop_end, tracks))
}

fn read_varlen(d: &[u8], mut j: usize) -> Option<(u32, usize)> {
    let mut v = 0u32;
    loop {
        let b = *d.get(j)?;
        j += 1;
        v = (v << 7) | (b & 0x7F) as u32;
        if b & 0x80 == 0 {
            return Some((v, j));
        }
    }
}
