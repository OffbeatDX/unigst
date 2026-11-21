use anyhow::{bail, Result};

use crate::common::audio::Pcm;

use super::chart::SoundEvent;

const RATE: i64 = 44_100;

fn pcm_duration_ms(pcm: &Pcm) -> f64 {
    (pcm.frames() as f64) / RATE as f64 * 1000.0
}

fn mix_placement(pcm: &Pcm, sound: &SoundEvent, player_end: &[i32; 2]) -> (i64, usize) {
    let start_frame = sound.offset as i64 * RATE / 1000;
    let pcm_frames = pcm.frames();
    if pcm_frames == 0 {
        return (start_frame, 0);
    }
    let mut use_frames = pcm_frames;
    if (0..=1).contains(&sound.player) {
        let end = player_end[sound.player as usize];
        let duration_ms = pcm_duration_ms(pcm);
        if end != -1 && sound.offset as f64 + duration_ms > end as f64 {
            let take_ms = (end - sound.offset) as f64;
            let want_frames = (take_ms * RATE as f64 / 1000.0).ceil() as i64;
            use_frames = pcm_frames.min(want_frames.max(0) as usize);
        }
    }
    (start_frame, use_frames * 2)
}

pub fn build_mix(events: &[SoundEvent], pcm_cache: &[Pcm], volume: f32) -> Result<Vec<f32>> {
    let mut player_end: [i32; 2] = [-1, -1];
    for e in events {
        if e.keysound_id == -1 && e.key == -1 && (0..=1).contains(&e.player) {
            player_end[e.player as usize] = e.offset;
        }
    }

    let mut max_end_frame: i64 = 0;
    for e in events {
        if e.keysound_id < 0 || e.keysound_id as usize >= pcm_cache.len() {
            continue;
        }
        let pcm = &pcm_cache[e.keysound_id as usize];
        let (start_frame, use_stereo) = mix_placement(pcm, e, &player_end);
        if use_stereo < 2 {
            continue;
        }
        let end = start_frame + (use_stereo / 2) as i64;
        if end > max_end_frame {
            max_end_frame = end;
        }
    }
    if max_end_frame <= 0 {
        max_end_frame = 1;
    }
    if max_end_frame > (i32::MAX / 2 - 1) as i64 {
        bail!("rendered mix exceeds maximum buffer size");
    }

    let mut mix = vec![0.0f32; max_end_frame as usize * 2];
    for e in events {
        if e.keysound_id < 0 || e.keysound_id as usize >= pcm_cache.len() {
            continue;
        }
        let pcm = &pcm_cache[e.keysound_id as usize];
        let (start_frame, use_stereo) = mix_placement(pcm, e, &player_end);
        if use_stereo < 2 {
            continue;
        }
        let use_frames = use_stereo / 2;
        let start_f = (-start_frame).max(0) as usize;
        for f in start_f..use_frames {
            let di = ((start_frame + f as i64) * 2) as usize;
            if di + 1 >= mix.len() {
                break;
            }
            mix[di] += pcm.samples[f * 2] * volume;
            mix[di + 1] += pcm.samples[f * 2 + 1] * volume;
        }
    }

    Ok(mix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_offset_clips_cleanly_without_panic() {
        let pcm = vec![Pcm::new(vec![0.25, -0.25, 0.5, -0.5])];
        let events = vec![SoundEvent {
            offset: -10,
            keysound_id: 0,
            key: 0,
            player: 0,
        }];
        let mix = build_mix(&events, &pcm, 1.0).expect("mix");
        assert!(!mix.is_empty());
    }
}
