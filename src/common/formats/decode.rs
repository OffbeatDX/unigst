use std::io::Cursor;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::errors::Error as SymError;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::probe::Hint;

use crate::common::audio::Pcm;
use crate::common::ffmpeg;

use super::archive::Blob;
use super::mf;

pub struct RawPcm {
    pub samples: Vec<f32>,
    pub rate: u32,
    pub channels: usize,
}

fn symphonia_decode_wav(bytes: &[u8]) -> Result<RawPcm, String> {
    let mut hint = Hint::new();
    hint.with_extension("wav");
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &Default::default(), &Default::default())
        .map_err(|e| format!("probe failed: {e}"))?;

    let mut format = probed.format;
    let track = format.default_track().ok_or_else(|| "no default track".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &Default::default())
        .map_err(|e| format!("no decoder: {e}"))?;

    let mut rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let mut channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2);
    let mut samples: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("read failed: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                rate = spec.rate;
                channels = spec.channels.count();
                let mut sbuf = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
                sbuf.copy_interleaved_ref(buf);
                samples.extend_from_slice(sbuf.samples());
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode failed: {e}")),
        }
    }

    Ok(RawPcm { samples, rate, channels })
}

impl RawPcm {
    fn into_pcm(self) -> Pcm {
        Pcm::from_raw(&self.samples, self.channels, self.rate)
    }
}

pub fn decode_blob(blob: &Blob, ffmpeg: &Path) -> Result<Pcm, String> {
    let (bytes, primary) = match blob {
        Blob::Wav(bytes) => (bytes.as_slice(), symphonia_decode_wav(bytes)),
        Blob::Wma(bytes) => (bytes.as_slice(), mf::decode_bytes(bytes)),
    };
    match primary {
        Ok(raw) if !raw.samples.is_empty() => Ok(raw.into_pcm()),

        _ if bytes.is_empty() => Ok(Pcm::default()),
        _ => ffmpeg::decode_bytes(ffmpeg, bytes).map_err(|e| e.to_string()),
    }
}
