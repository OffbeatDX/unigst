use anyhow::{bail, Context, Result};

use crate::common::bytes::u16_le;
use crate::common::bytes::u32_le as read_u32;

pub enum Blob {
    Wav(Vec<u8>),

    Wma(Vec<u8>),
}

pub struct Entry2dx<'a> {
    pub payload: &'a [u8],
    pub track_id: u16,
}

fn slice(data: &[u8], pos: usize, len: usize) -> Option<&[u8]> {
    data.get(pos..pos.checked_add(len)?)
}

pub fn read_2dx_entries(data: &[u8]) -> Result<Vec<Entry2dx<'_>>> {
    let file_count = read_u32(data, 0x14).context("2dx: truncated header")? as usize;
    let mut entries = Vec::with_capacity(file_count);
    for i in 0..file_count {
        let offset = read_u32(data, 0x48 + i * 4).context("2dx: truncated index")? as usize;
        if slice(data, offset, 4).context("2dx: bad offset")? != b"2DX9" {
            bail!("2dx: invalid 2DX9 entry at {offset:#x}");
        }
        let data_offset = read_u32(data, offset + 4).context("2dx: bad entry")? as usize;
        let data_size = read_u32(data, offset + 8).context("2dx: bad entry")? as usize;

        let track_id = u16_le(data, offset + 14).unwrap_or(0);
        let payload = slice(data, offset + data_offset, data_size)
            .context("2dx: payload out of range")?;
        entries.push(Entry2dx { payload, track_id });
    }
    Ok(entries)
}

fn read_s3p(data: &[u8]) -> Result<Vec<Blob>> {
    let file_count = read_u32(data, 4).context("s3p: truncated header")? as usize;
    let mut samples = Vec::with_capacity(file_count);
    for i in 0..file_count {
        let entry = i * 8 + 8;
        let offset = read_u32(data, entry).context("s3p: truncated index")? as usize;
        if slice(data, offset, 4).context("s3p: bad offset")? != b"S3V0" {
            bail!("s3p: invalid S3V0 entry at {offset:#x}");
        }
        let data_offset = read_u32(data, offset + 4).context("s3p: bad entry")? as usize;
        let data_size = read_u32(data, offset + 8).context("s3p: bad entry")? as usize;
        let payload = slice(data, offset + data_offset, data_size)
            .context("s3p: payload out of range")?;
        samples.push(Blob::Wma(payload.to_vec()));
    }
    Ok(samples)
}

fn read_2dx(data: &[u8]) -> Result<Vec<Blob>> {
    Ok(read_2dx_entries(data)?
        .into_iter()
        .map(|e| Blob::Wav(e.payload.to_vec()))
        .collect())
}

pub fn read_bytes(data: &[u8]) -> Result<Vec<Blob>> {
    if data.len() >= 4 && &data[0..4] == b"S3P0" {
        read_s3p(data)
    } else {
        read_2dx(data)
    }
}

pub fn sample_count(data: &[u8]) -> Option<usize> {
    if data.len() >= 4 && &data[0..4] == b"S3P0" {
        read_u32(data, 4).map(|n| n as usize)
    } else {
        read_u32(data, 0x14).map(|n| n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_2dx(payload: &[u8]) -> Vec<u8> {
        let mut d = vec![0u8; 0x48 + 4];
        d[0x14..0x18].copy_from_slice(&1u32.to_le_bytes());
        let entry_off = d.len() as u32;
        d[0x48..0x4C].copy_from_slice(&entry_off.to_le_bytes());

        d.extend_from_slice(b"2DX9");
        d.extend_from_slice(&12u32.to_le_bytes());
        d.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        d.extend_from_slice(payload);
        d
    }

    fn tiny_s3p(payload: &[u8]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"S3P0");
        d.extend_from_slice(&1u32.to_le_bytes());
        let entry_off = 8 + 8;
        d.extend_from_slice(&(entry_off as u32).to_le_bytes());
        d.extend_from_slice(&0u32.to_le_bytes());
        d.extend_from_slice(b"S3V0");
        d.extend_from_slice(&12u32.to_le_bytes());
        d.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        d.extend_from_slice(payload);
        d
    }

    #[test]
    fn reads_2dx_payload() {
        let data = tiny_2dx(b"RIFFdata");
        let blobs = read_bytes(&data).expect("parse 2dx");
        assert_eq!(blobs.len(), 1);
        match &blobs[0] {
            Blob::Wav(b) => assert_eq!(b.as_slice(), b"RIFFdata"),
            _ => panic!("expected Wav blob"),
        }
        assert_eq!(sample_count(&data), Some(1));
    }

    #[test]
    fn reads_s3p_payload() {
        let data = tiny_s3p(b"wma-bytes");
        let blobs = read_bytes(&data).expect("parse s3p");
        assert_eq!(blobs.len(), 1);
        match &blobs[0] {
            Blob::Wma(b) => assert_eq!(b.as_slice(), b"wma-bytes"),
            _ => panic!("expected Wma blob"),
        }
        assert_eq!(sample_count(&data), Some(1));
    }

    #[test]
    fn truncated_archives_error_not_panic() {
        assert!(read_bytes(b"").is_err());
        assert!(read_bytes(b"S3P0").is_err());
        let mut bad = tiny_2dx(b"x");
        bad.truncate(0x4A);
        assert!(read_bytes(&bad).is_err());
    }
}
