//! The m4a/MP2K engine: authentic sequencing + dispatch, hi-fi output.
//!
//! Sequencing follows pret's engine (the ground truth): the sequencer and
//! envelopes tick once per GBA frame (59.7275 Hz), ticks advance at
//! BPM/150 per frame (MP2K's tempo accumulator), TrkVolPitSet's exact
//! volume/pan/pitch math, and — the load-bearing part — voicegroup keysplits
//! and drumsets are resolved at NOTE-ON against the raw tables, never
//! pre-flattened. The output stage is hi-fi on purpose: float stereo at any
//! sample rate, synthesized PSG, interpolated PCM; its math is ported from
//! pokeemerald_SDL3's m4a.c float path, which was verified sample-exact
//! against official GBA recordings (docs/audio-engine.md blesses exactly
//! this split).

use crate::data::{MusicPack, Voice};

/// GBA VBlank rate: the sequencer/envelope tick.
pub const FRAME_HZ: f64 = 59.7275;
/// One full PSG channel vs full-scale PCM (GBA mixer proportions).
const PSG_FULL: f32 = 15.0 * 8.0 / 1024.0;
/// Headroom: the loudest song peaks just under full scale.
const MASTER: f32 = 0.34;
/// Emerald's DirectSound mix rate — sets the reverb tap spacing.
const DS_RATE: f64 = 13379.0;
/// Live channel cap (the GBA's is far lower; this is a hi-fi safety net).
const MAX_CHANNELS: usize = 256;

const DUTY: [f32; 4] = [0.125, 0.25, 0.5, 0.75];

/// GB square frequency table (gCgbFreqTable/gCgbScaleTable, m4a_tables.c).
const CGB_FREQ: [i32; 12] =
    [-2004, -1891, -1785, -1685, -1591, -1501, -1417, -1337, -1262, -1192, -1125, -1062];

fn square_freq(key: i32) -> f64 {
    let k = key.clamp(36, 166) - 36;
    let reg = 2048 + (CGB_FREQ[(k % 12) as usize] >> (k / 12));
    131072.0 / (2048 - reg) as f64
}

/// What a note resolved to: a concrete playable voice.
#[derive(Clone, Copy)]
struct Resolved {
    voice: Voice,
    /// The key that sounds (drumset notes play at the sub-voice's base key).
    play_key: u8,
    /// Drum pan override in the -128..126 range, 0 = neutral.
    rp: i32,
}

/// Resolve (voicegroup, program, key) at note-on — pret's runtime path.
/// `None` = the note cannot sound; the verification harness counts these.
fn resolve(pack: &MusicPack, group: u16, program: u8, key: u8) -> Option<Resolved> {
    let vg = pack.voicegroups.get(group as usize)?;
    let concrete = |v: &Voice, rhythm: bool| -> Option<Resolved> {
        match *v {
            Voice::Keysplit { .. } | Voice::KeysplitAll { .. } => None, // no nesting
            Voice::Unsupported => None,
            voice => {
                let (base, pan) = match voice {
                    Voice::DirectSound { base_key, pan, .. }
                    | Voice::Square { base_key, pan, .. }
                    | Voice::Wave { base_key, pan, .. }
                    | Voice::Noise { base_key, pan, .. } => (base_key, pan),
                    _ => unreachable!(),
                };
                let (play_key, rp) = if rhythm {
                    // engine: pan byte 0x80|pan -> ((0x80|pan)-0xC0)*2
                    let rp = if pan != 0 { ((0x80 | pan as i32) - 0xC0) * 2 } else { 0 };
                    (base, rp)
                } else {
                    (key, 0)
                };
                Some(Resolved { voice, play_key, rp })
            }
        }
    };
    match *vg.voice(program as u16)? {
        Voice::Keysplit { group: sub, table } => {
            let idx = pack.keysplits.get(table as usize)?.map[key as usize & 127];
            if idx == 0xFF {
                return None; // key outside the table's mapped range
            }
            concrete(pack.voicegroups.get(sub as usize)?.voice(idx as u16)?, false)
        }
        Voice::KeysplitAll { group: sub } => {
            concrete(pack.voicegroups.get(sub as usize)?.voice(key as u16)?, true)
        }
        ref v => concrete(v, false),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum EnvPhase {
    Attack,
    Decay,
    Sustain,
    Release,
    Dead,
}

struct Channel {
    track: usize,
    res: Resolved,
    vel: u8,
    /// Gate in sequencer ticks; release starts when it runs out.
    gate: u32,
    /// DirectSound envelope 0..255 (pret's per-frame math) or PSG level 0..15.
    env: f32,
    phase_env: EnvPhase,
    /// PSG note volume goal 0..15, quantized like the hardware.
    goal: i32,
    /// Held for the current frame.
    env_held: f32,
    gl: f32,
    gr: f32,
    incr: f64,
    phase: f64,
    born_frame: u64,
    dead: bool,
}

struct Track {
    events: usize, // index into song.tracks[n]
    next: usize,
    done: bool,
    prog: u8,
    vol: i32,
    pan: i32,
    bend: i32,
    bend_range: i32,
    modulation: i32,
    lfo_speed: i32,
}

impl Track {
    fn new(events: usize) -> Self {
        Track {
            events,
            next: 0,
            done: false,
            prog: 0,
            vol: 100,
            pan: 0,
            bend: 0,
            bend_range: 2,
            modulation: 0,
            lfo_speed: 22,
        }
    }
}

/// Per-play diagnostics for the verification harness: every note the score
/// asked for either sounded or is accounted for here.
#[derive(Debug, Default, Clone)]
pub struct PlayStats {
    pub notes_on: u64,
    /// (track, program, key) of notes that resolved to nothing.
    pub dropped: Vec<(usize, u8, u8)>,
    /// Accumulated |output| per track — proof each track actually sounds.
    pub track_energy: Vec<f64>,
    /// Per track: spawned at least one note with nonzero potential gain.
    /// (Quiet PSG notes can quantize to the hardware's silent volume steps —
    /// faithful silence, not a missing instrument.)
    pub spawned_audible: Vec<bool>,
}

pub struct Engine {
    pack: MusicPack,
    rate: f64,
    lfsr15: Vec<f32>,
    lfsr7: Vec<f32>,
    noise_nr43: [u8; 60],

    song: Option<usize>,
    looping: bool,
    tracks: Vec<Track>,
    channels: Vec<Channel>,
    tick: u32,
    tick_acc: f64,
    frame: u64,
    frame_acc: f64,
    pub stats: PlayStats,

    reverb_g: f32,
    rev_buf: Vec<f32>,
    rev_idx: usize,
    rev_a: usize,
    rev_b: usize,
}

impl Engine {
    pub fn new(pack: MusicPack, sample_rate: u32) -> Engine {
        let mut lfsr15 = Vec::with_capacity(32767);
        let mut lfsr7 = Vec::with_capacity(127);
        let mut lfsr = 0x7fffi32;
        for _ in 0..32767 {
            let bit = (lfsr ^ (lfsr >> 1)) & 1;
            lfsr = (lfsr >> 1) | (bit << 14);
            lfsr15.push(if lfsr & 1 != 0 { 1.0 } else { -1.0 });
        }
        lfsr = 0x7f;
        for _ in 0..127 {
            let bit = (lfsr ^ (lfsr >> 1)) & 1;
            lfsr = (lfsr >> 1) | (bit << 6);
            lfsr7.push(if lfsr & 1 != 0 { 1.0 } else { -1.0 });
        }
        // gNoiseTable: NR43 per key 21..80
        let mut noise_nr43 = [0u8; 60];
        let mut ni = 0;
        's: for s in 0..15u8 {
            for r in (4..=7u8).rev() {
                noise_nr43[ni] = (s << 4) | r;
                ni += 1;
                if ni == 56 {
                    break 's;
                }
            }
        }
        noise_nr43[56..].copy_from_slice(&[3, 2, 1, 0]);

        let rate = sample_rate as f64;
        let rev_a = (7.0 * 224.0 / DS_RATE * rate).round() as usize;
        let rev_b = (6.0 * 224.0 / DS_RATE * rate).round() as usize;
        Engine {
            pack,
            rate,
            lfsr15,
            lfsr7,
            noise_nr43,
            song: None,
            looping: false,
            tracks: Vec::new(),
            channels: Vec::new(),
            tick: 0,
            tick_acc: 0.0,
            frame: 0,
            frame_acc: 0.0,
            stats: PlayStats::default(),
            reverb_g: 0.0,
            rev_buf: vec![0.0; rev_a + 4],
            rev_idx: 0,
            rev_a,
            rev_b,
        }
    }

    pub fn pack(&self) -> &MusicPack {
        &self.pack
    }

    pub fn song_index(&self, name: &str) -> Option<usize> {
        self.pack.songs.iter().position(|s| s.name == name)
    }

    pub fn playing(&self) -> bool {
        self.song.is_some()
    }

    pub fn play(&mut self, song: usize, looping: bool) {
        let s = &self.pack.songs[song];
        self.tracks = (0..s.tracks.len()).map(Track::new).collect();
        self.channels.clear();
        self.song = Some(song);
        self.looping = looping && s.loop_start.is_some() && s.loop_end.is_some();
        self.tick = 0;
        self.tick_acc = 0.0;
        self.frame = 0;
        self.frame_acc = 0.0;
        self.stats = PlayStats::default();
        self.stats.track_energy = vec![0.0; s.tracks.len()];
        self.stats.spawned_audible = vec![false; s.tracks.len()];
        self.reverb_g = s.reverb as f32 / 256.0;
        self.rev_buf.fill(0.0);
        self.rev_idx = 0;
        self.frame_tick(); // fire tick-0 events before the first sample
    }

    pub fn stop(&mut self) {
        self.song = None;
        self.channels.clear();
    }

    /// Current BPM from the song's tempo map at the current tick.
    fn bpm(&self) -> f64 {
        let s = &self.pack.songs[self.song.unwrap()];
        let us = s
            .tempos
            .iter()
            .take_while(|&&(t, _)| t <= self.tick)
            .last()
            .map(|&(_, us)| us)
            .unwrap_or(500000);
        // scaled for the song's own division (pret's MIDIs are all 24)
        60e6 / us as f64 * 24.0 / s.division.max(1) as f64
    }

    /// One sequencer tick: fire due events, count down gates.
    fn seq_tick(&mut self) {
        let Some(si) = self.song else { return };

        // loop wrap: [ .. ) — events at loop_end belong to the next pass
        if self.looping {
            let s = &self.pack.songs[si];
            if let (Some(ls), Some(le)) = (s.loop_start, s.loop_end) {
                if self.tick >= le {
                    self.tick = ls;
                    for t in &mut self.tracks {
                        let evs = &self.pack.songs[si].tracks[t.events];
                        t.next = evs.partition_point(|e| e.tick < ls);
                        t.done = false;
                    }
                }
            }
        }

        // gates first: a note spawned THIS tick keeps its full gate (MP2K
        // counts down before reading new commands)
        for c in &mut self.channels {
            if c.gate > 0 {
                c.gate -= 1;
                if c.gate == 0 && c.phase_env != EnvPhase::Dead {
                    c.phase_env = EnvPhase::Release;
                }
            }
        }

        for ti in 0..self.tracks.len() {
            loop {
                let t = &self.tracks[ti];
                if t.done {
                    break;
                }
                let evs = &self.pack.songs[si].tracks[t.events];
                let Some(e) = evs.get(t.next) else {
                    self.tracks[ti].done = !self.looping;
                    break;
                };
                if e.tick > self.tick {
                    break;
                }
                let kind = e.kind;
                self.tracks[ti].next += 1;
                use crate::data::EventKind::*;
                match kind {
                    Note { key, vel, dur } => self.note_on(ti, key, vel, dur),
                    Program(p) => self.tracks[ti].prog = p,
                    Volume(v) => self.tracks[ti].vol = v as i32,
                    Pan(p) => self.tracks[ti].pan = p as i32,
                    Bend(b) => self.tracks[ti].bend = b as i32,
                    Mod(m) => self.tracks[ti].modulation = m as i32,
                    BendRange(r) => self.tracks[ti].bend_range = r as i32,
                    LfoSpeed(l) => self.tracks[ti].lfo_speed = l as i32,
                }
            }
        }

        self.tick += 1;
    }

    fn note_on(&mut self, ti: usize, key: u8, vel: u8, dur: u32) {
        self.stats.notes_on += 1;
        let group = self.pack.songs[self.song.unwrap()].voicegroup;
        let prog = self.tracks[ti].prog;
        let Some(res) = resolve(&self.pack, group, prog, key) else {
            self.stats.dropped.push((ti, prog, key));
            return;
        };
        if self.channels.len() >= MAX_CHANNELS {
            return;
        }
        let goal = match res.voice {
            Voice::DirectSound { .. } => 0,
            _ => {
                let t = &self.tracks[ti];
                let (l, r) = side_vols(vel as i32, t.vol, t.pan, res.rp);
                ((l + r) >> 4).min(15)
            }
        };
        let audible = match res.voice {
            Voice::DirectSound { .. } => vel > 0,
            Voice::Wave { .. } => wave_vol_code(goal) > 0.0,
            _ => goal > 0,
        };
        if audible {
            self.stats.spawned_audible[ti] = true;
        }
        self.channels.push(Channel {
            track: ti,
            res,
            vel,
            gate: dur.max(1),
            env: 0.0,
            phase_env: EnvPhase::Attack,
            goal,
            env_held: 0.0,
            gl: 0.0,
            gr: 0.0,
            incr: 0.0,
            phase: 0.0,
            born_frame: self.frame,
            dead: false,
        });
    }

    /// One GBA frame: sequencer ticks (MP2K tempo accumulator: BPM/150 ticks
    /// per frame), then envelope + gain + pitch updates held for the frame.
    fn frame_tick(&mut self) {
        if self.song.is_none() {
            return;
        }
        self.tick_acc += self.bpm() / 150.0;
        while self.tick_acc >= 1.0 {
            self.tick_acc -= 1.0;
            self.seq_tick();
        }

        for c in &mut self.channels {
            let t = &self.tracks[c.track];

            // vibrato: triangle LFO, ls*59.7275/256 Hz, peak mod*16/256 semi
            let mut vib = 0.0f64;
            if t.modulation > 0 && !matches!(c.res.voice, Voice::Noise { .. }) {
                let hz = t.lfo_speed as f64 * FRAME_HZ / 256.0;
                let ph = ((self.frame - c.born_frame) as f64 / FRAME_HZ * hz).fract();
                let tri = if ph < 0.25 {
                    ph * 4.0
                } else if ph < 0.75 {
                    2.0 - ph * 4.0
                } else {
                    ph * 4.0 - 4.0
                };
                vib = t.modulation as f64 * 16.0 / 256.0 * tri;
            }
            let bend_semi = (t.bend * t.bend_range) as f64 / 64.0;

            match c.res.voice {
                Voice::DirectSound { sample, adsr, fixed, .. } => {
                    step_ds_env(c, adsr);
                    if c.phase_env == EnvPhase::Dead {
                        c.dead = true;
                        continue;
                    }
                    c.env_held = c.env / 255.0 * (c.vel as f32 / 127.0);
                    // live volume/pan (ChnVolSetAsm, linear pan)
                    let y = (2 * t.pan).clamp(-128, 127);
                    c.gl = ((127 - y) as f32 / 128.0)
                        * ((127 - c.res.rp) as f32 / 128.0)
                        * (t.vol as f32 / 127.0);
                    c.gr = ((y + 128) as f32 / 128.0)
                        * ((c.res.rp + 128) as f32 / 128.0)
                        * (t.vol as f32 / 127.0);
                    let smp = &self.pack.samples[sample as usize];
                    let ratio = if fixed {
                        1.0
                    } else {
                        2f64.powf((c.res.play_key as f64 - 60.0 + bend_semi + vib) / 12.0)
                    };
                    c.incr = smp.rate as f64 * ratio / self.rate;
                }
                psg => {
                    let adsr = match psg {
                        Voice::Square { adsr, .. }
                        | Voice::Wave { adsr, .. }
                        | Voice::Noise { adsr, .. } => adsr,
                        _ => unreachable!(),
                    };
                    step_psg_env(c, adsr);
                    if c.phase_env == EnvPhase::Dead {
                        c.dead = true;
                        continue;
                    }
                    // PSG gain: goal quantized to 4 bits, hard 3-way pan
                    let (l, r) = side_vols(c.vel as i32, t.vol, t.pan, c.res.rp);
                    let amp = c.goal as f32 / 15.0 * PSG_FULL;
                    let (mut gl, mut gr) = (amp, amp);
                    if r >= l && (r >> 1) >= l {
                        gl = 0.0;
                    } else if l > r && (l >> 1) >= r {
                        gr = 0.0;
                    }
                    if let Voice::Wave { .. } = psg {
                        // the wave channel only has coarse volume fractions
                        let sc = wave_vol_code(c.goal) * 15.0 / c.goal.max(1) as f32;
                        gl *= sc;
                        gr *= sc;
                    }
                    c.gl = gl;
                    c.gr = gr;
                    c.env_held = c.env / 15.0;

                    let key = c.res.play_key as i32;
                    let fac = 2f64.powf((bend_semi + vib) / 12.0);
                    c.incr = match psg {
                        Voice::Square { .. } => square_freq(key) * fac / self.rate,
                        // wave: 32 steps at half the square rate
                        Voice::Wave { .. } => square_freq(key) / 2.0 * fac / self.rate,
                        Voice::Noise { .. } => {
                            let i = (key - 21).clamp(0, 59) as usize;
                            let nr43 = self.noise_nr43[i];
                            let (s, r) = (nr43 >> 4, nr43 & 7);
                            let div = if r == 0 { 0.5 } else { r as f64 };
                            524288.0 / div / 2f64.powi(s as i32 + 1) / self.rate
                        }
                        _ => unreachable!(),
                    };
                }
            }
        }
        self.channels.retain(|c| !c.dead);

        // song over? (non-looping, all tracks done, nothing sounding)
        if !self.looping && self.tracks.iter().all(|t| t.done) && self.channels.is_empty() {
            self.song = None;
        }
        self.frame += 1;
    }

    /// Render interleaved stereo i16 — the `AudioSink` port's wire format.
    pub fn render_i16(&mut self, scratch: &mut Vec<f32>, out: &mut [i16]) {
        scratch.clear();
        scratch.resize(out.len(), 0.0);
        self.render(scratch);
        for (o, s) in out.iter_mut().zip(scratch.iter()) {
            *o = (s * 32767.0) as i16;
        }
    }

    /// Render interleaved stereo f32. Pure: no I/O, no device.
    pub fn render(&mut self, out: &mut [f32]) {
        let frame_period = self.rate / FRAME_HZ;
        for o in out.chunks_exact_mut(2) {
            if self.frame_acc <= 0.0 {
                self.frame_tick();
                self.frame_acc += frame_period;
            }
            self.frame_acc -= 1.0;

            if self.song.is_none() {
                o[0] = 0.0;
                o[1] = 0.0;
                continue;
            }

            let (mut pcm_l, mut pcm_r, mut psg_l, mut psg_r) = (0f32, 0f32, 0f32, 0f32);
            for c in &mut self.channels {
                let s = render_sample(c, &self.pack, &self.lfsr15, &self.lfsr7) * c.env_held;
                let (l, r) = (s * c.gl, s * c.gr);
                if matches!(c.res.voice, Voice::DirectSound { .. }) {
                    pcm_l += l;
                    pcm_r += r;
                } else {
                    psg_l += l;
                    psg_r += r;
                }
                self.stats.track_energy[c.track] += (l.abs() + r.abs()) as f64;
            }

            // m4a reverb: mono two-tap feedback echo on the PCM sub-mix only
            let mut e = 0f32;
            if self.reverb_g > 0.0 {
                let n = self.rev_buf.len();
                let a = self.rev_buf[(self.rev_idx + n - self.rev_a) % n];
                let b = self.rev_buf[(self.rev_idx + n - self.rev_b) % n];
                e = self.reverb_g * (a + b);
                let mut w = 0.5 * (pcm_l + pcm_r) + e;
                if w.abs() < 1e-15 {
                    w = 0.0; // flush denormals
                }
                self.rev_buf[self.rev_idx] = w;
                self.rev_idx = (self.rev_idx + 1) % n;
            }

            o[0] = ((pcm_l + e + psg_l) * MASTER).clamp(-1.0, 1.0);
            o[1] = ((pcm_r + e + psg_r) * MASTER).clamp(-1.0, 1.0);
        }
    }
}

/// MP2K side volumes (m4a_1.s ChnVolSetAsm): 0..255 per side.
fn side_vols(vel: i32, vol: i32, pan: i32, rp: i32) -> (i32, i32) {
    let y = (2 * pan).clamp(-128, 127);
    let vol_mr = ((y + 128) * 2 * vol) >> 8;
    let vol_ml = ((127 - y) * 2 * vol) >> 8;
    let l = (vel * (127 - rp) * vol_ml) >> 14;
    let r = (vel * (rp + 128) * vol_mr) >> 14;
    (l.min(255), r.min(255))
}

/// gCgb3Vol: wave channel coarse volume fractions.
fn wave_vol_code(level: i32) -> f32 {
    match level {
        ..=1 => 0.0,
        2..=5 => 0.25,
        6..=9 => 0.5,
        10..=13 => 0.75,
        _ => 1.0,
    }
}

/// DirectSound envelope, pret's per-frame math on 0..255:
/// attack: env += A; decay: env = env*D/256 toward S; release: env = env*R/256.
fn step_ds_env(c: &mut Channel, adsr: [u8; 4]) {
    let [a, d, s, r] = adsr.map(|v| v as f32);
    match c.phase_env {
        EnvPhase::Attack => {
            c.env += a;
            if c.env >= 255.0 {
                c.env = 255.0;
                c.phase_env = EnvPhase::Decay;
            }
        }
        EnvPhase::Decay => {
            c.env = c.env * d / 256.0;
            if c.env <= s {
                c.env = s;
                c.phase_env = EnvPhase::Sustain;
            }
        }
        EnvPhase::Sustain => {}
        EnvPhase::Release => {
            if r <= 0.0 {
                c.phase_env = EnvPhase::Dead;
                return;
            }
            c.env = c.env * r / 256.0;
            if c.env < 0.5 {
                c.phase_env = EnvPhase::Dead;
            }
        }
        EnvPhase::Dead => {}
    }
}

/// PSG envelope on the GB's 0..15 scale: A/D/R are frames per level step
/// (0 = instant), sustain level = (goal*S + 15) >> 4.
fn step_psg_env(c: &mut Channel, adsr: [u8; 4]) {
    let [a, d, s, r] = adsr;
    let goal = c.goal as f32;
    let sus = if c.goal > 0 { ((c.goal * s as i32 + 15) >> 4) as f32 } else { 0.0 };
    match c.phase_env {
        EnvPhase::Attack => {
            if a == 0 {
                c.env = goal;
            } else {
                c.env += 1.0 / a as f32;
            }
            if c.env >= goal {
                c.env = goal;
                c.phase_env = EnvPhase::Decay;
            }
        }
        EnvPhase::Decay => {
            if d == 0 {
                c.env = sus;
            } else {
                c.env -= 1.0 / d as f32;
            }
            if c.env <= sus {
                c.env = sus;
                c.phase_env = EnvPhase::Sustain;
            }
        }
        EnvPhase::Sustain => {}
        EnvPhase::Release => {
            if r == 0 {
                c.phase_env = EnvPhase::Dead;
                return;
            }
            c.env -= 1.0 / r as f32;
            if c.env <= 0.0 {
                c.env = 0.0;
                c.phase_env = EnvPhase::Dead;
            }
        }
        EnvPhase::Dead => {}
    }
}

/// One output sample for a channel, pre-envelope/gain.
fn render_sample(c: &mut Channel, pack: &MusicPack, lfsr15: &[f32], lfsr7: &[f32]) -> f32 {
    match c.res.voice {
        Voice::DirectSound { sample, .. } => {
            let smp = &pack.samples[sample as usize];
            let data = &smp.pcm;
            let n = data.len();
            if n == 0 {
                return 0.0;
            }
            let mut p = c.phase;
            if let Some(ls) = smp.loop_start {
                let ls = (ls as usize).min(n - 1) as f64;
                if p >= n as f64 {
                    let span = n as f64 - ls;
                    p = ls + (p - ls) % span;
                }
            } else if p >= (n - 1) as f64 {
                c.dead = true; // one-shot finished
                return 0.0;
            }
            let i = p as usize;
            let frac = (p - i as f64) as f32;
            let j = if i + 1 < n { i + 1 } else { i };
            c.phase = p + c.incr;
            (data[i] as f32 * (1.0 - frac) + data[j] as f32 * frac) * (1.0 / 128.0)
        }
        Voice::Square { duty, .. } => {
            // 4x oversampled naive pulse
            let duty = DUTY[(duty & 3) as usize];
            let inc = c.incr / 4.0;
            let mut acc = 0f32;
            for _ in 0..4 {
                acc += if (c.phase.fract() as f32) < duty { 1.0 } else { -1.0 };
                c.phase += inc;
            }
            if c.phase >= 1024.0 {
                c.phase = c.phase.fract();
            }
            acc / 4.0
        }
        Voice::Wave { wave, .. } => {
            let pat = &pack.waves[wave as usize];
            let pos = c.phase.fract() as f32 * 32.0;
            let i = pos as usize;
            let frac = pos - i as f32;
            c.phase += c.incr;
            if c.phase >= 1024.0 {
                c.phase = c.phase.fract();
            }
            let a = pat[i & 31] as f32 - 7.5;
            let b = pat[(i + 1) & 31] as f32 - 7.5;
            (a * (1.0 - frac) + b * frac) / 7.5
        }
        Voice::Noise { period, .. } => {
            let seq = if period == 1 { lfsr7 } else { lfsr15 };
            let len = seq.len();
            let s = seq[c.phase as usize % len];
            c.phase += c.incr;
            if c.phase >= len as f64 * 64.0 {
                c.phase %= len as f64;
            }
            s
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::*;

    fn test_pack() -> MusicPack {
        // one looping sample, one drumset + keysplit chain, one song
        let sub = VoiceGroup {
            name: "sub".into(),
            offset: 0,
            entries: vec![
                Voice::DirectSound { base_key: 60, pan: 0, sample: 0, adsr: [255, 0, 255, 165], fixed: false },
                Voice::Square { base_key: 60, pan: 0, duty: 2, sweep: 0, adsr: [0, 0, 15, 0] },
                Voice::DirectSound { base_key: 48, pan: 0xC0u8.wrapping_add(16), sample: 0, adsr: [255, 0, 255, 165], fixed: false },
            ],
        };
        let main = VoiceGroup {
            name: "main".into(),
            offset: 0,
            entries: vec![
                Voice::KeysplitAll { group: 0 },
                Voice::Keysplit { group: 0, table: 0 },
                Voice::Wave { base_key: 60, pan: 0, wave: 0, adsr: [0, 0, 15, 3] },
                Voice::Noise { base_key: 60, pan: 0, period: 0, adsr: [0, 0, 15, 3] },
            ],
        };
        let mut map = [0xFFu8; 128];
        for k in 40..80 {
            map[k] = if k < 60 { 0 } else { 1 };
        }
        let mut wave = [0u8; 32];
        for (i, w) in wave.iter_mut().enumerate() {
            *w = (i / 2) as u8;
        }
        MusicPack {
            samples: vec![SampleData {
                name: "s".into(),
                rate: 13379.0,
                loop_start: Some(8),
                pcm: (0..64).map(|i| if i % 8 < 4 { 100 } else { -100 }).collect(),
            }],
            waves: vec![wave],
            keysplits: vec![KeysplitTable { name: "t".into(), map }],
            voicegroups: vec![sub, main],
            songs: vec![SongData {
                name: "test".into(),
                voicegroup: 1,
                master_volume: 127,
                reverb: 50,
                division: 24,
                tempos: vec![(0, 500000)], // 120 BPM
                loop_start: Some(0),
                loop_end: Some(96),
                tracks: vec![
                    vec![
                        Event { tick: 0, kind: EventKind::Program(1) },
                        Event { tick: 0, kind: EventKind::Volume(100) },
                        Event { tick: 0, kind: EventKind::Note { key: 50, vel: 100, dur: 24 } },
                        Event { tick: 24, kind: EventKind::Note { key: 70, vel: 100, dur: 24 } },
                    ],
                    vec![
                        Event { tick: 0, kind: EventKind::Program(0) },
                        Event { tick: 0, kind: EventKind::Volume(100) },
                        Event { tick: 0, kind: EventKind::Note { key: 2, vel: 127, dur: 4 } },
                        Event { tick: 48, kind: EventKind::Note { key: 127, vel: 127, dur: 4 } },
                    ],
                    vec![
                        Event { tick: 0, kind: EventKind::Program(2) },
                        Event { tick: 0, kind: EventKind::Volume(90) },
                        Event { tick: 12, kind: EventKind::Note { key: 60, vel: 90, dur: 48 } },
                    ],
                ],
            }],
        }
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|s| s * s).sum::<f32>() / buf.len() as f32).sqrt()
    }

    #[test]
    fn renders_sound_and_resolves_keysplits_and_drums() {
        let mut e = Engine::new(test_pack(), 44100);
        e.play(0, false);
        let mut buf = vec![0f32; 44100 * 2 * 2]; // 2 seconds
        e.render(&mut buf);
        assert!(rms(&buf) > 0.01, "the song must make sound");
        assert_eq!(e.stats.notes_on, 5);
        // drum key 127 has no entry (the sub group has 3); everything else,
        // including drum key 2 and both keysplit halves, must sound
        assert_eq!(e.stats.dropped, vec![(1, 0, 127)]);
    }

    #[test]
    fn keysplit_ranges_pick_sub_voices() {
        let pack = test_pack();
        let low = resolve(&pack, 1, 1, 50).unwrap();
        assert!(matches!(low.voice, Voice::DirectSound { .. }));
        assert_eq!(low.play_key, 50, "melodic keysplit keeps the played key");
        let high = resolve(&pack, 1, 1, 70).unwrap();
        assert!(matches!(high.voice, Voice::Square { .. }));
        assert!(resolve(&pack, 1, 1, 20).is_none(), "outside the table range");
    }

    #[test]
    fn drumset_notes_play_at_the_sub_voice_base_key() {
        let pack = test_pack();
        let drum = resolve(&pack, 1, 0, 2).unwrap();
        assert_eq!(drum.play_key, 48, "drum sounds at its own base key");
        assert_ne!(drum.rp, 0, "drum pan byte override applies");
        assert!(resolve(&pack, 1, 0, 100).is_none(), "no entry at that key");
    }

    #[test]
    fn looping_song_keeps_playing_and_non_looping_ends() {
        let mut e = Engine::new(test_pack(), 44100);
        e.play(0, true);
        let mut buf = vec![0f32; 44100 * 2 * 6];
        e.render(&mut buf);
        assert!(e.playing(), "looping song plays on");
        // the fixture's 2 s loop has notes in its first half: check second
        // 4..5 (the third pass), well after the first wrap at 2 s
        let pass3 = &buf[4 * 88200..5 * 88200];
        assert!(rms(pass3) > 0.001, "still audible after loop wraps");

        let mut e = Engine::new(test_pack(), 44100);
        e.play(0, false);
        let mut buf = vec![0f32; 44100 * 2 * 8];
        e.render(&mut buf);
        assert!(!e.playing(), "non-looping song must end");
        let tail = &buf[buf.len() - 8820..];
        assert!(rms(tail) == 0.0, "silence after the end");
    }
}
