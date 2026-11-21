use std::io::Cursor;
use std::path::Path;

use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::TrackType;
use symphonia::core::io::MediaSourceStream;

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
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, Default::default(), Default::default())
        .map_err(|e| format!("probe failed: {e}"))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| "no default track".to_string())?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| "default track is not audio".to_string())?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .map_err(|e| format!("no decoder: {e}"))?;

    let mut rate = params.sample_rate.unwrap_or(44_100);
    let mut channels = params.channels.as_ref().map(|c| c.count()).unwrap_or(2);
    let mut samples: Vec<f32> = Vec::new();
    let mut chunk: Vec<f32> = Vec::new();

    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(format!("read failed: {e}")),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = buf.spec();
                rate = spec.rate();
                channels = spec.channels().count();
                // copy_to_vec_interleaved resizes `chunk` to exactly this packet's samples.
                buf.copy_to_vec_interleaved(&mut chunk);
                samples.extend_from_slice(&chunk);
            }
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode failed: {e}")),
        }
    }

    Ok(RawPcm {
        samples,
        rate,
        channels,
    })
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
