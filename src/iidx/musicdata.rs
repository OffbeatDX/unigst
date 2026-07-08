use std::collections::HashMap;
use std::path::Path;

use anyhow::{bail, Result};

const SUPPORTED_VERSIONS: &[u32] = &[
    20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 80,
];

fn is_supported(version: u32) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}

#[derive(Debug, Clone)]
pub struct SongMeta {
    pub song_id: u32,
    pub title: String,
    pub title_ascii: String,
    pub genre: String,
    pub artist: String,
    pub game_version: u16,
    pub volume: u32,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or_else(|| anyhow::anyhow!("overflow"))?;
        let s = self
            .data
            .get(self.pos..end)
            .ok_or_else(|| anyhow::anyhow!("unexpected end of music_data.bin"))?;
        self.pos = end;
        Ok(s)
    }

    fn skip(&mut self, len: usize) -> Result<()> {
        self.bytes(len)?;
        Ok(())
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.bytes(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into().unwrap()))
    }

    fn string_cp932(&mut self, len: usize) -> Result<String> {
        let raw = self.bytes(len)?;
        let (decoded, _, _) = encoding_rs::SHIFT_JIS.decode(raw);
        Ok(decoded.trim_end_matches('\0').replace('\u{fffd}', ""))
    }

    fn string_utf16(&mut self, len: usize) -> Result<String> {
        let raw = self.bytes(len)?;
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        Ok(String::from_utf16_lossy(&units).trim_end_matches('\0').to_string())
    }
}

fn read_song_entry(version: u32, r: &mut Reader) -> Result<SongMeta> {
    let (title, title_ascii, genre, artist) = if version == 80 {
        let t = r.string_cp932(0x80)?;
        let ta = r.string_cp932(0x40)?;
        let g = r.string_cp932(0x80)?;
        let a = r.string_cp932(0x80)?;
        (t, ta, g, a)
    } else if version >= 32 {
        let t = r.string_utf16(0x100)?;
        let ta = r.string_cp932(0x40)?;
        let g = r.string_utf16(0x80)?;
        let a = r.string_utf16(0x100)?;
        let _subtitle = r.string_utf16(0x100)?;
        (t, ta, g, a)
    } else {
        let t = r.string_cp932(0x40)?;
        let ta = r.string_cp932(0x40)?;
        let g = r.string_cp932(0x40)?;
        let a = r.string_cp932(0x40)?;
        (t, ta, g, a)
    };

    r.skip(4 * 5)?;
    if version >= 32 && version != 80 {
        r.skip(4)?;
    }
    r.skip(4)?;
    let game_version = r.u16()?;

    if version >= 32 && version != 80 {
        r.skip(2 * 7)?;
    } else {
        r.skip(2 * 3)?;
    }

    if version >= 27 {
        r.skip(10)?;
    } else {
        r.skip(8)?;
    }

    if version == 80 {
        r.skip(0x2C6)?;
    } else if version >= 27 {
        r.skip(0x286)?;
    } else {
        r.skip(0xA0)?;
    }

    let song_id = r.u32()?;
    let volume = r.u32()?;

    if version >= 27 {
        r.skip(10)?;
    } else {
        r.skip(8)?;
    }

    let _bga_delay = r.i16()?;
    if version <= 26 || version == 80 {
        r.skip(2)?;
    }

    let _bga_filename = r.string_cp932(0x20)?;
    if version == 80 {
        r.skip(2)?;
    }

    let _afp_flag = r.u32()?;
    let afp_count = if version >= 22 { 10 } else { 9 };
    r.skip(afp_count * 0x20)?;

    if version >= 26 {
        r.skip(4)?;
    }

    Ok(SongMeta {
        song_id,
        title,
        title_ascii,
        genre,
        artist,
        game_version,
        volume,
    })
}

pub fn parse_file(path: &Path) -> Result<HashMap<u32, SongMeta>> {
    let data = std::fs::read(path)?;
    let mut r = Reader::new(&data);

    if r.bytes(4)? != b"IIDX" {
        bail!("not a valid music_data.bin (missing IIDX magic)");
    }
    let version = r.u32()?;

    let (available_entries, total_entries) = if version >= 32 {
        let available = r.u16()?;
        let _unk = r.u16()?;
        let total = r.u32()?;
        (available, total)
    } else {
        let available = r.u16()?;
        let total = r.u32()?;
        let _unk = r.u16()?;
        (available, total)
    };

    let index_entry_size = if version >= 32 && version != 80 { 4 } else { 2 };
    r.skip(total_entries as usize * index_entry_size)?;

    if !is_supported(version) {
        bail!("unsupported music_data.bin version {version}");
    }

    let mut map = HashMap::with_capacity(available_entries as usize);
    for _ in 0..available_entries {
        let song = read_song_entry(version, &mut r)?;
        map.insert(song.song_id, song);
    }
    Ok(map)
}

pub fn style(game_version: u16) -> Option<(&'static str, &'static str)> {
    Some(match game_version {
        0 => ("1st style", "1999"),
        1 => ("substream", "1999"),
        2 => ("2nd style", "1999"),
        3 => ("3rd style", "2000"),
        4 => ("4th style", "2000"),
        5 => ("5th style", "2001"),
        6 => ("6th style", "2001"),
        7 => ("7th style", "2002"),
        8 => ("8th style", "2002"),
        9 => ("9th style", "2003"),
        10 => ("10th style", "2004"),
        11 => ("IIDX RED", "2004"),
        12 => ("HAPPY SKY", "2005"),
        13 => ("DistorteD", "2006"),
        14 => ("GOLD", "2007"),
        15 => ("DJ TROOPERS", "2007"),
        16 => ("EMPRESS", "2008"),
        17 => ("SIRIUS", "2009"),
        18 => ("Resort Anthem", "2010"),
        19 => ("Lincle", "2011"),
        20 => ("tricoro", "2012"),
        21 => ("SPADA", "2013"),
        22 => ("PENDUAL", "2014"),
        23 => ("copula", "2015"),
        24 => ("SINOBUZ", "2016"),
        25 => ("CANNON BALLERS", "2017"),
        26 => ("Rootage", "2018"),
        27 => ("HEROIC VERSE", "2019"),
        28 => ("BISTROVER", "2020"),
        29 => ("CastHour", "2021"),
        30 => ("RESIDENT", "2022"),
        31 => ("EPOLIS", "2023"),
        32 => ("Pinky Crush", "2024"),
        33 => ("Sparkle Shower", "2025"),
        80 => ("INFINITAS", "2020"),
        _ => return None,
    })
}
