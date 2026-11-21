use anyhow::{Context, Result};

pub const OUTPUT_RATE: u32 = 44_100;

#[derive(Debug, Default, Clone)]
pub struct Pcm {
    pub samples: Vec<f32>,
}

impl Pcm {
    pub fn new(samples: Vec<f32>) -> Self {
        Pcm { samples }
    }

    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }

    pub fn from_raw(interleaved: &[f32], channels: usize, rate: u32) -> Pcm {
        let stereo = to_stereo(interleaved, channels);
        let resampled = resample_stereo(&stereo, rate, OUTPUT_RATE);
        Pcm::new(resampled)
    }
}

fn to_stereo(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    if channels == 2 {
        return samples[..(samples.len() / 2) * 2].to_vec();
    }
    let frames = samples.len() / channels;
    let mut out = Vec::with_capacity(frames * 2);
    if channels == 1 {
        for &s in samples {
            out.push(s);
            out.push(s);
        }
    } else {
        for f in 0..frames {
            let base = f * channels;
            let mut sum = 0.0f32;
            for c in 0..channels {
                sum += samples[base + c];
            }
            let avg = sum / channels as f32;
            out.push(avg);
            out.push(avg);
        }
    }
    out
}

fn resample_stereo(samples: &[f32], src: u32, dst: u32) -> Vec<f32> {
    if src == 0 || dst == 0 || samples.is_empty() {
        return Vec::new();
    }
    if src == dst {
        return samples.to_vec();
    }
    let in_frames = samples.len() / 2;
    if in_frames == 0 {
        return Vec::new();
    }
    let ratio = dst as f64 / src as f64;
    let out_frames = ((in_frames as f64) * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let src_pos = i as f64 / ratio;
        let idx = (src_pos.floor() as usize).min(in_frames - 1);
        let frac = (src_pos - idx as f64).clamp(0.0, 1.0) as f32;
        let next = (idx + 1).min(in_frames - 1);
        for ch in 0..2 {
            let a = samples[idx * 2 + ch];
            let b = samples[next * 2 + ch];
            out.push(a + (b - a) * frac);
        }
    }
    out
}

pub fn normalize(mix: &mut [f32]) -> f32 {
    let peak = mix.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak > 1.0 {
        let scale = 1.0 / peak;
        for s in mix.iter_mut() {
            *s *= scale;
        }
    }
    peak
}

pub fn wav24_bytes(samples: &[f32]) -> Result<Vec<u8>> {
    const MAX_24: f32 = 8_388_607.0;
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: OUTPUT_RATE,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer =
            hound::WavWriter::new(&mut cursor, spec).context("cannot create in-memory WAV")?;
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * MAX_24) as i32;
            writer.write_sample(v)?;
        }
        writer.finalize().context("cannot finalize WAV")?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn loud_mix_scales_to_full_scale() {
        let mut mix = vec![0.5, -2.0, 1.0, 0.0];
        let peak = normalize(&mut mix);
        assert_eq!(peak, 2.0);
        assert_eq!(mix, vec![0.25, -1.0, 0.5, 0.0]);
    }

    #[test]
    fn quiet_mix_is_untouched() {
        let mut mix = vec![0.5, -0.9, 0.0];
        let peak = normalize(&mut mix);
        assert_eq!(peak, 0.9);
        assert_eq!(mix, vec![0.5, -0.9, 0.0]);
    }
}
