use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};

use super::audio::Pcm;
use super::tags::{Format, TrackTags};

pub fn locate() -> Result<PathBuf> {
    let names = exe_names("ffmpeg");

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in &names {
                let cand = dir.join(name);
                if cand.is_file() && runs(&cand) {
                    return Ok(cand);
                }
            }
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in &names {
                let cand = dir.join(name);
                if cand.is_file() && runs(&cand) {
                    return Ok(cand);
                }
            }
        }
    }

    bail!("working ffmpeg not found — install it and put it on PATH, or place ffmpeg next to unigst")
}

fn runs(path: &Path) -> bool {
    Command::new(path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn exe_names(base: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{base}.exe"), base.to_string()]
    } else {
        vec![base.to_string()]
    }
}

pub enum EncodeInput<'a> {
    File(&'a Path),
    WavBytes(&'a [u8]),
}

impl EncodeInput<'_> {
    fn describe(&self) -> String {
        match self {
            EncodeInput::File(p) => p.display().to_string(),
            EncodeInput::WavBytes(_) => "<in-memory wav>".to_string(),
        }
    }
}

pub struct EncodeJob<'a> {
    pub input: EncodeInput<'a>,
    pub output: &'a Path,
    pub format: Format,
    pub tags: &'a TrackTags,

    pub cover: Option<&'a Path>,
}

pub fn encode(ffmpeg: &Path, job: &EncodeJob) -> Result<()> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-y")
        .args(["-loglevel", "error"])
        .args(["-guess_layout_max", "0"]);

    match &job.input {
        EncodeInput::File(path) => {
            cmd.arg("-i").arg(path);
        }
        EncodeInput::WavBytes(_) => {
            cmd.args(["-f", "wav", "-i", "pipe:0"]);
        }
    }

    let stream_art = job.cover.is_some() && job.format.art_as_stream();
    let opus_meta = match job.cover {
        Some(cover) if !job.format.art_as_stream() => Some(write_cover_metadata(job.output, cover)?),
        _ => None,
    };

    if stream_art {
        cmd.arg("-i").arg(job.cover.unwrap());
    }
    if let Some(meta) = &opus_meta {
        cmd.arg("-i").arg(meta);
    }

    if opus_meta.is_some() {
        cmd.args(["-map_metadata", "1", "-map_metadata:s", "-1"]);
    } else {
        cmd.args(["-map_metadata", "-1", "-map_metadata:s", "-1"]);
    }

    cmd.args(job.format.codec_args());

    if stream_art {
        cmd.args(["-map", "0:a", "-map", "1:v", "-c:v", "copy"])
            .args(["-disposition:v:0", "attached_pic"]);
    } else if opus_meta.is_some() {
        cmd.args(["-map", "0:a"]);
    }

    for arg in job.tags.to_ffmpeg_args() {
        cmd.arg(arg);
    }

    let part = part_path(job.output);
    cmd.args(["-f", job.format.muxer()]).arg(&part);

    let output = match &job.input {
        EncodeInput::File(_) => {
            cmd.stdin(Stdio::null());
            cmd.output()
                .with_context(|| format!("failed to run ffmpeg for {}", job.input.describe()))?
        }
        EncodeInput::WavBytes(bytes) => {
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = cmd.spawn().context("failed to spawn ffmpeg for encode")?;
            let mut stdin = child.stdin.take().context("no ffmpeg stdin")?;

            std::thread::scope(|s| {
                s.spawn(move || {
                    let _ = stdin.write_all(bytes);
                });
                child.wait_with_output()
            })
            .context("waiting on ffmpeg encode")?
        }
    };
    if let Some(meta) = &opus_meta {
        let _ = std::fs::remove_file(meta);
    }
    if !output.status.success() {
        let _ = std::fs::remove_file(&part);
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ffmpeg failed ({}) for {}: {}",
            output.status,
            job.input.describe(),
            stderr.trim()
        );
    }
    std::fs::rename(&part, job.output)
        .with_context(|| format!("renaming {} into place", part.display()))?;
    Ok(())
}

fn part_path(output: &Path) -> PathBuf {
    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    output.with_file_name(name)
}

fn write_cover_metadata(output: &Path, cover: &Path) -> Result<PathBuf> {
    let img = std::fs::read(cover)
        .with_context(|| format!("reading cover art {}", cover.display()))?;
    let block = flac_picture_block(&img, cover);
    let value = base64_encode(&block);

    let mut name = output.file_name().unwrap_or_default().to_os_string();
    name.push(".covermeta");
    let path = output.with_file_name(name);

    let contents = format!(";FFMETADATA1\nMETADATA_BLOCK_PICTURE={value}\n");
    std::fs::write(&path, contents)
        .with_context(|| format!("writing cover metadata {}", path.display()))?;
    Ok(path)
}

fn flac_picture_block(img: &[u8], cover: &Path) -> Vec<u8> {
    let is_jpeg = matches!(
        cover.extension().and_then(|e| e.to_str()),
        Some("jpg") | Some("jpeg") | Some("JPG") | Some("JPEG")
    );
    let mime: &[u8] = if is_jpeg { b"image/jpeg" } else { b"image/png" };
    let (w, h) = png_dimensions(img);

    let mut b = Vec::with_capacity(img.len() + 64);
    b.extend_from_slice(&3u32.to_be_bytes());
    b.extend_from_slice(&(mime.len() as u32).to_be_bytes());
    b.extend_from_slice(mime);
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&24u32.to_be_bytes());
    b.extend_from_slice(&0u32.to_be_bytes());
    b.extend_from_slice(&(img.len() as u32).to_be_bytes());
    b.extend_from_slice(img);
    b
}

fn png_dimensions(data: &[u8]) -> (u32, u32) {
    const SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() >= 24 && data.starts_with(SIG) && &data[12..16] == b"IHDR" {
        let w = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
        let h = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
        (w, h)
    } else {
        (0, 0)
    }
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub fn decode_bytes(ffmpeg: &Path, bytes: &[u8]) -> Result<Pcm> {
    let mut child = Command::new(ffmpeg)
        .args(["-loglevel", "error"])
        .args(["-i", "pipe:0"])
        .args(["-f", "f32le", "-acodec", "pcm_f32le"])
        .args(["-ac", "2", "-ar", "44100"])
        .arg("pipe:1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg for byte decode")?;

    let mut stdin = child.stdin.take().context("no ffmpeg stdin")?;
    let mut stderr = child.stderr.take().context("no ffmpeg stderr")?;
    let mut stdout = child.stdout.take().context("no ffmpeg stdout")?;

    let (read_res, err_out) = std::thread::scope(|s| {
        s.spawn(move || {
            let _ = stdin.write_all(bytes);

        });
        let err_reader = s.spawn(move || {
            let mut buf = Vec::new();
            let _ = stderr.read_to_end(&mut buf);
            buf
        });
        let mut raw = Vec::new();
        let res = stdout.read_to_end(&mut raw).map(|_| raw);
        (res, err_reader.join().unwrap_or_default())
    });
    let raw = read_res.context("reading ffmpeg output")?;
    let status = child.wait().context("waiting on ffmpeg")?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&err_out);
        bail!("ffmpeg byte decode failed ({status}): {}", stderr.trim());
    }

    let mut samples = Vec::with_capacity(raw.len() / 4);
    for chunk in raw.chunks_exact(4) {
        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(Pcm::new(samples))
}
