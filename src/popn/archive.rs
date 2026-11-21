use anyhow::Result;

use crate::common::formats::archive::read_2dx_entries;

pub struct Keysounds {
    pub wavs: Vec<Vec<u8>>,

    pub bg: usize,
}

impl Keysounds {
    pub fn entry_for(&self, chart_keysound: u16) -> Option<usize> {
        if chart_keysound == 0 {
            return (self.bg < self.wavs.len()).then_some(self.bg);
        }
        let idx = chart_keysound as usize - 1;
        (idx < self.wavs.len()).then_some(idx)
    }
}

pub fn read(data: &[u8]) -> Result<Keysounds> {
    let entries = read_2dx_entries(data)?;
    let bg = entries.iter().position(|e| e.track_id == 0).unwrap_or(0);
    let wavs = entries.into_iter().map(|e| e.payload.to_vec()).collect();
    Ok(Keysounds { wavs, bg })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_2dx(payloads: &[&[u8]], bg_at: usize) -> Vec<u8> {
        let count = payloads.len();
        let mut d = vec![0u8; 0x48 + count * 4];
        d[0x14..0x18].copy_from_slice(&(count as u32).to_le_bytes());
        for (i, p) in payloads.iter().enumerate() {
            let entry = d.len() as u32;
            d[0x48 + i * 4..0x48 + i * 4 + 4].copy_from_slice(&entry.to_le_bytes());
            d.extend_from_slice(b"2DX9");
            d.extend_from_slice(&24u32.to_le_bytes());
            d.extend_from_slice(&(p.len() as u32).to_le_bytes());
            d.extend_from_slice(&[0u8; 2]);
            let track_id: u16 = if i == bg_at { 0 } else { 0xFFFF };
            d.extend_from_slice(&track_id.to_le_bytes());
            d.extend_from_slice(&[0u8; 8]);
            d.extend_from_slice(p);
        }
        d
    }

    #[test]
    fn bg_flag_is_found_wherever_it_sits() {
        let ks = read(&tiny_2dx(&[b"a", b"b", b"c"], 2)).expect("parse");
        assert_eq!(ks.wavs.len(), 3);
        assert_eq!(ks.bg, 2);
        assert_eq!(ks.entry_for(0), Some(2));
        assert_eq!(ks.entry_for(1), Some(0));
        assert_eq!(ks.entry_for(3), Some(2));
        assert_eq!(ks.entry_for(4), None);
    }

    #[test]
    fn truncated_container_errors() {
        assert!(read(b"").is_err());
        let mut bad = tiny_2dx(&[b"x"], 0);
        bad.truncate(0x4A);
        assert!(read(&bad).is_err());
    }
}
