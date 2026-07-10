//! Print a song's resolution chain from its pack — a quick eyeball that the
//! raw structures made it through extraction. Usage:
//!   cargo run -p audio --example inspect -- [song]

use audio::data::{EventKind, MusicPack, Voice};

fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "mus_cycling".into());
    let path = format!("assets/music/{name}.bin");
    let pack = MusicPack::from_bytes(&std::fs::read(&path).expect("run musicgen")).unwrap();
    let song = pack.songs.iter().find(|s| s.name == name).expect("song not in pack");
    let vg = &pack.voicegroups[song.voicegroup as usize];
    println!(
        "{}: voicegroup '{}' ({} entries), mvl={} reverb={} div={} tracks={} loop={:?}..{:?}",
        song.name,
        vg.name,
        vg.entries.len(),
        song.master_volume,
        song.reverb,
        song.division,
        song.tracks.len(),
        song.loop_start,
        song.loop_end
    );
    // programs actually used by the song
    let mut used: Vec<u8> = Vec::new();
    for t in &song.tracks {
        let mut prog = 0u8;
        for e in t {
            match e.kind {
                EventKind::Program(p) => prog = p,
                EventKind::Note { .. } if !used.contains(&prog) => used.push(prog),
                _ => {}
            }
        }
    }
    used.sort();
    for p in used {
        let v = vg.voice(p as u16);
        let desc = match v {
            Some(Voice::Keysplit { group, table }) => format!(
                "keysplit -> group '{}' via table '{}'",
                pack.voicegroups[*group as usize].name, pack.keysplits[*table as usize].name
            ),
            Some(Voice::KeysplitAll { group }) => {
                format!("drumset -> group '{}'", pack.voicegroups[*group as usize].name)
            }
            Some(Voice::DirectSound { sample, base_key, .. }) => format!(
                "directsound '{}' base={base_key}",
                pack.samples[*sample as usize].name
            ),
            other => format!("{other:?}"),
        };
        println!("  prog {p:3} = {desc}");
    }
    let notes: usize = song
        .tracks
        .iter()
        .map(|t| t.iter().filter(|e| matches!(e.kind, EventKind::Note { .. })).count())
        .sum();
    println!("  {} notes total", notes);
}
