use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::common::bytes;

#[derive(Debug, Clone)]
pub struct PopnSong {
    pub id: u32,

    pub title: String,
    pub artist: String,

    pub genre: String,

    pub version: u32,

    pub ifs: Vec<(String, String)>,
}

const MUSIC_ENTRY_SIZE: usize = 312;

const MUSIC_STRING_PTRS: usize = 7;

const OFF_VERSION: usize = 0x40;
const OFF_FILE_IDX: usize = 0x54;

const SLOT_PREFERENCE: [usize; 6] = [3, 2, 1, 0, 4, 5];

const FILE_ENTRY_SIZE: usize = 40;

const ACCENT_MAP: &[(char, &str)] = &[
    ('鵝', "7"),
    ('圄', "à"),
    ('圉', "ä"),
    ('鵤', "Ä"),
    ('鵑', "👁"),
    ('鶤', "©"),
    ('圈', "é"),
    ('鵐', "ê"),
    ('鵙', "Ə"),
    ('鵲', "ë"),
    ('囿', "♥"),
    ('鶚', "㊙"),
    ('鶉', "ó"),
    ('鶇', "ö"),
    ('鶲', "Ⓟ"),
    ('鶫', "²"),
    ('圍', "@"),
    ('圖', "ţ"),
    ('鵺', "Ü"),
    ('囎', ":"),
    ('囂', "♡"),
    ('釁', "🐾"),
    ('罕', "έ"),
    ('罔', "ς"),
    ('彑', "Ø"),
    ('冫', "ꓘ"),
    ('炙', "焱"),
];

struct PeImage {
    data: Vec<u8>,
    image_base: u64,

    sections: Vec<(u64, u64, u64, u64)>,
}

impl PeImage {
    fn load(path: &Path) -> Result<PeImage> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let u16_at = |o: usize| -> Result<u16> {
            Ok(u16::from_le_bytes(
                data.get(o..o + 2).context("truncated PE")?.try_into().unwrap(),
            ))
        };
        let u32_at = |o: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(
                data.get(o..o + 4).context("truncated PE")?.try_into().unwrap(),
            ))
        };

        let e_lfanew = u32_at(0x3C)? as usize;
        if data.get(e_lfanew..e_lfanew + 4) != Some(b"PE\0\0") {
            bail!("not a PE file");
        }
        let nsec = u16_at(e_lfanew + 6)? as usize;
        let opt_size = u16_at(e_lfanew + 20)? as usize;
        let opt = e_lfanew + 24;
        let magic = u16_at(opt)?;
        if magic != 0x20B {
            bail!("expected a 64-bit popn.dll (PE32+), got magic {magic:#x}");
        }
        let image_base = u64::from_le_bytes(
            data.get(opt + 24..opt + 32).context("truncated PE")?.try_into().unwrap(),
        );

        let mut sections = Vec::with_capacity(nsec);
        for i in 0..nsec {
            let s = opt + opt_size + i * 40;
            let vsize = u32_at(s + 8)? as u64;
            let va = u32_at(s + 12)? as u64;
            let rsize = u32_at(s + 16)? as u64;
            let raw = u32_at(s + 20)? as u64;
            sections.push((va, vsize, raw, rsize));
        }
        Ok(PeImage {
            data,
            image_base,
            sections,
        })
    }

    fn va_to_offset(&self, va: u64) -> Option<usize> {
        let rva = va.checked_sub(self.image_base)?;
        for &(sva, vsize, raw, rsize) in &self.sections {
            if rva >= sva && rva < sva + vsize.max(rsize) {
                return usize::try_from(raw + (rva - sva)).ok();
            }
        }
        None
    }

    fn offset_to_va(&self, off: usize) -> Option<u64> {
        let off = off as u64;
        for &(sva, _, raw, rsize) in &self.sections {
            if off >= raw && off < raw + rsize {
                return Some(self.image_base + sva + (off - raw));
            }
        }
        None
    }

    fn cstr(&self, va: u64) -> Option<String> {
        let start = self.va_to_offset(va)?;
        let hay = self.data.get(start..start + 512.min(self.data.len() - start))?;
        let end = hay.iter().position(|&b| b == 0)?;
        let (s, _, _) = encoding_rs::SHIFT_JIS.decode(&hay[..end]);
        Some(s.into_owned())
    }

    fn valid_string_ptr(&self, va: u64) -> bool {
        va >= self.image_base && self.cstr(va).is_some()
    }

    fn u16_le(&self, off: usize) -> Option<u16> {
        bytes::u16_le(&self.data, off)
    }

    fn u32_le(&self, off: usize) -> Option<u32> {
        bytes::u32_le(&self.data, off)
    }

    fn u64_le(&self, off: usize) -> Option<u64> {
        bytes::u64_le(&self.data, off)
    }

    fn find_all(&self, needle: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut pos = 0;
        while let Some(i) = find(&self.data[pos..], needle) {
            out.push(pos + i);
            pos += i + 1;
        }
        out
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn music_entry_valid(pe: &PeImage, off: usize) -> bool {
    if off + MUSIC_ENTRY_SIZE > pe.data.len() {
        return false;
    }
    for k in 0..MUSIC_STRING_PTRS {
        match pe.u64_le(off + 8 * k) {
            Some(p) if pe.valid_string_ptr(p) => {}
            _ => return false,
        }
    }
    matches!(pe.u32_le(off + OFF_VERSION), Some(v) if v <= 64)
}

fn find_music_table(pe: &PeImage) -> Result<(usize, usize)> {
    const POPS_CP932: &[u8] = b"\x00\x83\x7C\x83\x62\x83\x76\x83\x58\x00";
    let hit = find(&pe.data, POPS_CP932).context("genre string 'pops' not found in DLL")?;
    let pops_va = pe.offset_to_va(hit + 1).context("pops string outside any section")?;

    for xref in pe.find_all(&pops_va.to_le_bytes()) {
        if !music_entry_valid(pe, xref) {
            continue;
        }

        let mut base = xref;
        while base >= MUSIC_ENTRY_SIZE && music_entry_valid(pe, base - MUSIC_ENTRY_SIZE) {
            base -= MUSIC_ENTRY_SIZE;
        }
        let mut count = 0;
        while music_entry_valid(pe, base + count * MUSIC_ENTRY_SIZE) {
            count += 1;
        }
        if count >= 100 {
            return Ok((base, count));
        }
    }
    bail!("music table not found (no pops xref parses as a table)");
}

fn find_file_table(pe: &PeImage) -> Result<usize> {
    for hit in pe.find_all(b"\x00pops\x00") {
        let Some(name_va) = pe.offset_to_va(hit + 1) else {
            continue;
        };
        for xref in pe.find_all(&name_va.to_le_bytes()) {
            if xref < 8 {
                continue;
            }
            let folder = pe.u64_le(xref - 8).unwrap_or(0);
            if pe.cstr(folder).as_deref() == Some("popn1") {
                return Ok(xref - 8);
            }
        }
    }
    bail!("file table not found (no (popn1, pops) entry)");
}

fn file_entry(pe: &PeImage, table: usize, idx: usize) -> Option<(String, String)> {
    let off = table + idx * FILE_ENTRY_SIZE;
    let folder = pe.cstr(pe.u64_le(off)?)?;
    let name = pe.cstr(pe.u64_le(off + 8)?)?;
    if folder.is_empty() || name.is_empty() || folder.len() > 32 || name.len() > 48 {
        return None;
    }
    Some((folder, name))
}

fn clean_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_color = false;
    while let Some(c) = chars.next() {
        if c == '[' {
            let tag: String = chars.by_ref().take_while(|&c| c != ']').collect();
            if tag.starts_with("c:") {
                in_color = true;
            } else if tag == "/c" {
                in_color = false;
            }

            continue;
        }
        if !in_color {
            out.push(c);
        }
    }
    let mut out = out.trim().to_string();
    for (from, to) in ACCENT_MAP {
        if out.contains(*from) {
            out = out.replace(*from, to);
        }
    }
    out
}

fn is_removed(title: &str, artist: &str) -> bool {
    let dash = |s: &str| matches!(s, "-" | "‐" | "−" | "－");
    (dash(title) && dash(artist)) || (title == "ＤＵＭＭＹ" && artist == "ＤＵＭＭＹ")
}

pub fn parse_dll(path: &Path) -> Result<Vec<PopnSong>> {
    let pe = PeImage::load(path)?;
    let (music, count) = find_music_table(&pe)?;
    let files = find_file_table(&pe)?;

    let mut songs = Vec::new();
    for id in 0..count {
        let off = music + id * MUSIC_ENTRY_SIZE;
        let s = |k: usize| {
            pe.u64_le(off + 8 * k)
                .and_then(|p| pe.cstr(p))
                .map(|v| clean_string(&v))
                .unwrap_or_default()
        };

        let (sort_title, sort_artist) = (s(1), s(2));
        if is_removed(&sort_title, &sort_artist) {
            continue;
        }
        let display = |primary: String, fallback: String| {
            if primary.is_empty() {
                fallback
            } else {
                primary
            }
        };
        let mut title = display(s(5), sort_title);
        let artist = display(s(6), sort_artist);
        let genre = s(0);

        if genre.ends_with("（アッパー）") {
            title.push_str(" (UPPER)");
        }

        let mut ifs = Vec::new();
        for slot in SLOT_PREFERENCE {
            let Some(idx) = pe.u16_le(off + OFF_FILE_IDX + 2 * slot) else {
                continue;
            };
            if let Some(entry) = file_entry(&pe, files, idx as usize) {
                if !ifs.contains(&entry) {
                    ifs.push(entry);
                }
            }
        }
        if ifs.is_empty() {
            continue;
        }

        songs.push(PopnSong {
            id: id as u32,
            title,
            artist,
            genre,
            version: pe.u32_le(off + OFF_VERSION).unwrap_or(0),
            ifs,
        });
    }
    if songs.is_empty() {
        bail!("music table parsed but produced no songs");
    }
    Ok(songs)
}

pub fn style(version: u32) -> Option<(&'static str, u32)> {
    Some(match version {
        1 => ("1", 1998),
        2 => ("2", 1999),
        3 => ("3", 1999),
        4 => ("4", 2000),
        5 => ("5", 2000),
        6 => ("6", 2001),
        7 => ("7", 2001),
        8 => ("8", 2002),
        9 => ("9", 2003),
        10 => ("10", 2003),
        11 => ("11", 2004),
        12 => ("12 Iroha", 2005),
        13 => ("13 Carnival", 2005),
        14 => ("14 FEVER!", 2006),
        15 => ("15 ADVENTURE", 2007),
        16 => ("16 PARTY♪", 2008),
        17 => ("17 THE MOVIE", 2009),
        18 => ("18 Sengoku Retsuden", 2010),
        19 => ("19 TUNE STREET", 2010),
        20 => ("20 fantasia", 2011),
        21 => ("21 Sunny Park", 2012),
        22 => ("22 Lapistoria", 2014),
        23 => ("23 éclale", 2015),
        24 => ("24 Usagi to Neko to Shounen no Yume", 2016),
        25 => ("25 peace", 2018),
        26 => ("26 Kaimei Riddles", 2020),
        27 => ("27 UniLab", 2022),
        28 => ("28 Jam&Fizz", 2024),
        29 => ("29 High☆Cheers!!", 2025),
        _ => {
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_and_gaiji_are_cleaned() {
        assert_eq!(clean_string("モヘア[/c][c:FF6d6d]鉑[/c]"), "モヘア");
        assert_eq!(clean_string("plain title"), "plain title");
        assert_eq!(clean_string("囂 heart"), "♡ heart");
    }

    #[test]
    fn removed_markers_detected() {
        assert!(is_removed("‐", "‐"));
        assert!(is_removed("-", "-"));
        assert!(is_removed("ＤＵＭＭＹ", "ＤＵＭＭＹ"));
        assert!(!is_removed("Quick Master", "act deft"));
    }
}
