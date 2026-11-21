use super::decode::RawPcm;

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;

    use windows::Win32::Media::MediaFoundation::{
        IMFSample, IMFSourceReader, MFAudioFormat_Float, MFCreateMFByteStreamOnStream,
        MFCreateMediaType, MFCreateSourceReaderFromByteStream, MFMediaType_Audio, MFShutdown,
        MFStartup, MFSTARTUP_FULL, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
        MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    };
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
    use windows::Win32::UI::Shell::SHCreateMemStream;

    use super::RawPcm;

    const MF_VERSION: u32 = 0x0002_0070;
    const FIRST_AUDIO_STREAM: u32 = 0xFFFF_FFFD;
    const ALL_STREAMS: u32 = 0xFFFF_FFFE;
    const END_OF_STREAM: u32 = 0x2;

    struct MfState {
        com: bool,
        mf: bool,
    }

    thread_local! {
        static MF_STATE: RefCell<Option<MfState>> = const { RefCell::new(None) };
    }

    /// Lazily start COM + Media Foundation on the current thread.
    ///
    /// Teardown happens only in [`shutdown_thread`], which `report::run_jobs`
    /// registers as the rayon pool's exit handler. So `decode_bytes` must only
    /// be called from `run_jobs` pool threads; any other thread would leak its
    /// COM/MF initialization until process exit.
    fn ensure_init() {
        MF_STATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                let (com, mf) = unsafe {
                    let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
                    let mf = MFStartup(MF_VERSION, MFSTARTUP_FULL).is_ok();
                    (com, mf)
                };
                *slot = Some(MfState { com, mf });
            }
        });
    }

    /// Tear down COM/Media Foundation on the current worker thread before it
    /// exits, outside the Windows TLS/loader lock.
    pub fn shutdown_thread() {
        let state = MF_STATE.with(|slot| slot.borrow_mut().take());
        if let Some(state) = state {
            unsafe {
                if state.mf {
                    let _ = MFShutdown();
                }
                if state.com {
                    CoUninitialize();
                }
            }
        }
    }

    unsafe fn read_all(reader: &IMFSourceReader) -> Result<RawPcm, String> {
        let err = |e: windows::core::Error| format!("Media Foundation error: {e}");

        reader.SetStreamSelection(ALL_STREAMS, false).map_err(err)?;
        reader
            .SetStreamSelection(FIRST_AUDIO_STREAM, true)
            .map_err(err)?;

        let media_type = MFCreateMediaType().map_err(err)?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)
            .map_err(err)?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_Float)
            .map_err(err)?;
        reader
            .SetCurrentMediaType(FIRST_AUDIO_STREAM, None, &media_type)
            .map_err(err)?;

        let current = reader
            .GetCurrentMediaType(FIRST_AUDIO_STREAM)
            .map_err(err)?;
        let rate = current
            .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
            .map_err(err)?;
        let channels = current.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).map_err(err)? as usize;

        let mut samples: Vec<f32> = Vec::new();
        loop {
            let mut flags = 0u32;
            let mut sample: Option<IMFSample> = None;
            reader
                .ReadSample(
                    FIRST_AUDIO_STREAM,
                    0,
                    None,
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                )
                .map_err(err)?;

            if flags & END_OF_STREAM != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let buffer = sample.ConvertToContiguousBuffer().map_err(err)?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len)).map_err(err)?;
            if !ptr.is_null() && len >= 4 {
                let raw = std::slice::from_raw_parts(ptr, len as usize);
                samples.reserve(raw.len() / 4);
                for &chunk in raw.as_chunks::<4>().0 {
                    samples.push(f32::from_le_bytes(chunk));
                }
            }
            let _ = buffer.Unlock();
        }

        Ok(RawPcm {
            samples,
            rate,
            channels,
        })
    }

    pub fn decode_bytes(bytes: &[u8]) -> Result<RawPcm, String> {
        ensure_init();
        unsafe {
            let stream = SHCreateMemStream(Some(bytes))
                .ok_or_else(|| "SHCreateMemStream failed".to_string())?;
            let byte_stream = MFCreateMFByteStreamOnStream(&stream)
                .map_err(|e| format!("MFCreateMFByteStreamOnStream failed: {e}"))?;
            let reader = MFCreateSourceReaderFromByteStream(&byte_stream, None)
                .map_err(|e| format!("MFCreateSourceReaderFromByteStream failed: {e}"))?;
            read_all(&reader)
        }
    }
}

#[cfg(windows)]
pub use imp::{decode_bytes, shutdown_thread};

#[cfg(not(windows))]
pub fn decode_bytes(_bytes: &[u8]) -> Result<RawPcm, String> {
    Err("Media Foundation decoding is only available on Windows".to_string())
}

#[cfg(not(windows))]
pub fn shutdown_thread() {}
