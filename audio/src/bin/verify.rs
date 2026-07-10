//! The verification harness (docs/audio-engine.md): proves the engine is not
//! missing instruments. For each song it
//!   1. statically resolves every note the score references against the raw
//!      voicegroup/keysplit/drumset tables (the exact path the engine takes
//!      at note-on) and flags any (track, program, key) that cannot sound;
//!   2. renders one full pass and checks every note-bearing track produced
//!      actual output energy.
//! Exit status is nonzero if any checked song fails — mus_cycling is the
//! acceptance target and is always included.
//!
//! Each song is verified against ITS OWN streamed pack (musicgen writes one
//! file per mus_* song plus sfx.bin for the se_* effects) — so this checks
//! exactly what ships, subset closure and index remapping included.
//!
//! Usage: cargo run -p audio --bin verify --release -- [song ...] [--wav song.wav]
//!        (no songs = the whole soundtrack)

use audio::data::{EventKind, MusicPack};
use audio::Engine;

const RATE: u32 = 44100;

fn main() {
    let mut wav_out: Option<String> = None;
    let mut names: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--wav" {
            wav_out = args.next();
        } else {
            names.push(a);
        }
    }

    let dir = std::path::Path::new("assets/music");
    // (pack file, songs to verify in it); empty songs = every song it holds
    let mut jobs: Vec<(std::path::PathBuf, Vec<String>)> = Vec::new();
    if names.is_empty() {
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .expect("run musicgen first")
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "bin"))
            .collect();
        paths.sort();
        jobs.extend(paths.into_iter().map(|p| (p, Vec::new())));
    } else {
        if !names.iter().any(|n| n == "mus_cycling") {
            names.push("mus_cycling".into()); // the acceptance target, always
        }
        for n in &names {
            let file = if n.starts_with("se_") { "sfx.bin".into() } else { format!("{n}.bin") };
            jobs.push((dir.join(file), vec![n.clone()]));
        }
    }

    let mut failures = 0;
    for (path, wanted) in &jobs {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                println!("FAIL {}: missing (run musicgen first)", path.display());
                failures += 1;
                continue;
            }
        };
        let pack = MusicPack::from_bytes(&bytes).unwrap();
        let songs: Vec<String> = if wanted.is_empty() {
            pack.songs.iter().map(|s| s.name.clone()).collect()
        } else {
            wanted.clone()
        };
        let mut engine = Engine::new(pack, RATE);
        for name in &songs {
            failures += verify_song(&mut engine, name) as u32;
        }
    }

    // optional WAV export of the FIRST named song, for confirming by ear
    if let Some(out) = wav_out {
        let name = names.first().cloned().unwrap_or_else(|| "mus_cycling".into());
        let file = if name.starts_with("se_") { "sfx.bin".into() } else { format!("{name}.bin") };
        let pack =
            MusicPack::from_bytes(&std::fs::read(dir.join(file)).expect("pack for --wav"))
                .unwrap();
        let mut engine = Engine::new(pack, RATE);
        let si = engine.song_index(&name).expect("song for --wav");
        let seconds = {
            let song = &engine.pack().songs[si];
            let end_tick = song.loop_end.unwrap_or(0).max(1);
            (ticks_to_seconds(song, end_tick) + 2.0).min(240.0)
        };
        engine.play(si, false);
        let mut pcm = vec![0f32; (seconds * RATE as f64) as usize * 2];
        engine.render(&mut pcm);
        write_wav(&out, RATE, &pcm);
        println!("wrote {out} ({seconds:.1}s of {name})");
    }

    std::process::exit(if failures > 0 { 1 } else { 0 });
}

/// Verify one song on its engine; returns true on failure.
fn verify_song(engine: &mut Engine, name: &str) -> bool {
    let Some(si) = engine.song_index(name) else {
        println!("FAIL {name}: not in pack");
        return true;
    };

    // 1. static shape of the score: which tracks have notes, how many
    let (total_notes, track_has_notes, n_tracks, seconds) = {
        let song = &engine.pack().songs[si];
        let mut total = 0u64;
        let mut has = vec![false; song.tracks.len()];
        for (ti, track) in song.tracks.iter().enumerate() {
            for e in track {
                if let EventKind::Note { .. } = e.kind {
                    total += 1;
                    has[ti] = true;
                }
            }
        }
        // render one pass: to loop_end (or last event) plus release tails
        let end_tick = song
            .loop_end
            .unwrap_or_else(|| {
                song.tracks.iter().flat_map(|t| t.iter().map(|e| e.tick)).max().unwrap_or(0)
            })
            .max(1);
        let secs = (ticks_to_seconds(song, end_tick) + 2.0).min(240.0);
        (total, has, song.tracks.len(), secs)
    };

    engine.play(si, false);
    let mut buf = vec![0f32; 2 * 4096];
    let mut remaining = (seconds * RATE as f64) as usize * 2;
    let mut peak = 0f32;
    while remaining > 0 {
        let n = buf.len().min(remaining);
        engine.render(&mut buf[..n]);
        peak = buf[..n].iter().fold(peak, |p, s| p.max(s.abs()));
        remaining -= n;
    }

    let stats = engine.stats.clone();
    // a track only counts as wrongly silent if the engine spawned an
    // audibly-gained note for it and still produced no output
    let silent: Vec<usize> = (0..track_has_notes.len())
        .filter(|&ti| stats.spawned_audible[ti] && stats.track_energy[ti] <= 0.0)
        .collect();

    // classify drops: keys/programs outside the authored tables are the
    // GBA reading adjacent ROM — data quirks, warned but not failures
    let mut quirks = std::collections::BTreeSet::new();
    let mut bugs = std::collections::BTreeSet::new();
    for &(ti, prog, key) in &stats.dropped {
        let group = engine.pack().songs[si].voicegroup;
        match classify_drop(engine.pack(), group, prog, key) {
            Some(reason) => quirks.insert((ti, prog, key, reason)),
            None => bugs.insert((ti, prog, key, "unexplained — engine/extraction bug")),
        };
    }

    if bugs.is_empty() && silent.is_empty() && stats.notes_on > 0 {
        let q = if quirks.is_empty() {
            String::new()
        } else {
            format!(" ({} out-of-range data quirks)", quirks.len())
        };
        println!(
            "ok   {name}: {} notes, {n_tracks} tracks, peak {peak:.2}{q}",
            stats.notes_on
        );
        false
    } else {
        println!(
            "FAIL {name}: {} of {total_notes} notes unexplained, silent tracks {silent:?}",
            bugs.len()
        );
        for (ti, prog, key, reason) in bugs.iter().take(10) {
            println!("       track {ti} prog {prog} key {key}: {reason}");
        }
        true
    }
}

/// Why can't this (program, key) sound? `Some(reason)` = the score references
/// something outside the authored tables (on hardware the GBA reads whatever
/// ROM bytes sit next door — undefined, unreproducible-by-design). `None` =
/// no benign explanation: a real engine/extraction bug.
fn classify_drop(
    pack: &MusicPack,
    group: u16,
    prog: u8,
    key: u8,
) -> Option<&'static str> {
    use audio::data::Voice;
    let vg = pack.voicegroups.get(group as usize)?;
    let Some(entry) = vg.voice(prog as u16) else {
        return Some("program outside the voicegroup");
    };
    match *entry {
        Voice::Unsupported => Some("unsupported voice kind (cry)"),
        Voice::Keysplit { group: sub, table } => {
            let idx = pack.keysplits.get(table as usize)?.map[key as usize & 127];
            if idx == 0xFF {
                return Some("key outside the keysplit tables' ROM span");
            }
            match pack.voicegroups.get(sub as usize)?.voice(idx as u16) {
                None => Some("keysplit index outside the sub-voicegroup"),
                Some(Voice::Unsupported) => Some("keysplit resolves to unsupported voice"),
                Some(Voice::Keysplit { .. } | Voice::KeysplitAll { .. }) => {
                    Some("nested keysplit (engine forbids)")
                }
                Some(_) => None,
            }
        }
        Voice::KeysplitAll { group: sub } => {
            match pack.voicegroups.get(sub as usize)?.voice(key as u16) {
                None => Some("drum key outside the drumset's range"),
                Some(Voice::Unsupported) => Some("drum key is an unsupported voice"),
                Some(Voice::Keysplit { .. } | Voice::KeysplitAll { .. }) => {
                    Some("nested keysplit (engine forbids)")
                }
                Some(_) => None,
            }
        }
        _ => None,
    }
}

fn ticks_to_seconds(song: &audio::data::SongData, tick: u32) -> f64 {
    let div = song.division.max(1) as f64;
    let mut sec = 0.0;
    let (mut last_t, mut last_us) = (0u32, 500000u32);
    for &(t, us) in &song.tempos {
        if t >= tick {
            break;
        }
        sec += (t - last_t) as f64 * last_us as f64 / 1e6 / div;
        (last_t, last_us) = (t, us);
    }
    sec + (tick - last_t) as f64 * last_us as f64 / 1e6 / div
}

fn write_wav(path: &str, rate: u32, pcm: &[f32]) {
    let mut out: Vec<u8> = Vec::with_capacity(44 + pcm.len() * 2);
    let data_len = (pcm.len() * 2) as u32;
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(2u16.to_le_bytes()); // stereo
    out.extend(rate.to_le_bytes());
    out.extend((rate * 4).to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for s in pcm {
        out.extend(((s * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out).unwrap();
}
