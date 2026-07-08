use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use roxmltree::Node;

pub const EXCLUDED_IDS: &[&str] = &["1259", "1438"];

pub const TOTAL_VERSIONS: usize = 7;

const ACCENT_MAP: &[(char, &str)] = &[
    ('驩', "Ø"),
    ('齲', "♥"),
    ('齶', "♡"),
    ('趁', "Ǣ"),
    ('騫', "á"),
    ('曦', "à"),
    ('驫', "ā"),
    ('齷', "é"),
    ('曩', "è"),
    ('䧺', "ê"),
    ('罇', "ê"),
    ('骭', "ü"),
    ('隍', "Ü"),
    ('雋', "Ǜ"),
    ('鬻', "♃"),
    ('鬥', "Ã"),
    ('鬆', "Ý"),
    ('鬮', "¡"),
    ('龕', "€"),
    ('蹙', "ℱ"),
    ('頽', "ä"),
    ('黻', "*"),
    ('疉', "Ö"),
    ('鑒', "₩"),
    ('盥', "⚙︎"),
    ('瀑', "À"),
    ('蔕', "ῦ"),
    ('壥', "Є"),
    ('鷸', "♫"),
];

const GENRES: &[(u32, &str)] = &[
    (1, "EXIT_TUNES"),
    (2, "FLOOR"),
    (4, "TOUHOU"),
    (8, "VOCALOID"),
    (16, "BEMANI"),
    (32, "ORIGINAL"),
    (64, "POP_ANIME"),
    (128, "HINABITA"),
];

pub fn version_name(version: &str) -> &str {
    match version {
        "1" => "BOOTH",
        "2" => "INFINITE INFECTION",
        "3" => "GRAVITY WARS",
        "4" => "HEAVENLY HAVEN",
        "5" => "VIVID WAVE",
        "6" => "EXCEED GEAR",
        "7" => "∇",
        other => other,
    }
}

pub fn diff_name(diff_char: char) -> Option<&'static str> {
    match diff_char {
        'm' => Some("MXM"),
        'n' => Some("NOV"),
        'a' => Some("ADV"),
        'e' => Some("EXH"),
        'u' => Some("ULT"),
        _ => None,
    }
}

pub fn inf_name(inf_ver: &str) -> Option<&'static str> {
    match inf_ver {
        "2" => Some("INF"),
        "3" => Some("GRV"),
        "4" => Some("HVN"),
        "5" => Some("VVD"),
        "6" => Some("XCD"),
        _ => None,
    }
}

fn genre_names(mask: u32) -> Vec<&'static str> {
    let names: Vec<&'static str> = GENRES
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if names.is_empty() {
        vec!["OTHER"]
    } else {
        names
    }
}

fn fix_accents(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match ACCENT_MAP.iter().find(|(wrong, _)| *wrong == c) {
            Some((_, right)) => out.push_str(right),
            None => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genre_bitmask_decodes() {
        assert_eq!(genre_names(16), vec!["BEMANI"]);
        assert_eq!(genre_names(48), vec!["BEMANI", "ORIGINAL"]);
        assert_eq!(genre_names(0), vec!["OTHER"]);
    }

    #[test]
    fn difficulty_labels() {
        assert_eq!(diff_name('m'), Some("MXM"));
        assert_eq!(diff_name('n'), Some("NOV"));
        assert_eq!(diff_name('x'), None);
        assert_eq!(inf_name("3"), Some("GRV"));
        assert_eq!(inf_name("9"), None);
    }

    #[test]
    fn version_names() {
        assert_eq!(version_name("6"), "EXCEED GEAR");
        assert_eq!(version_name("7"), "∇");
        assert_eq!(version_name("99"), "99");
    }

    #[test]
    fn id_matching_ignores_leading_zeros() {
        assert!(id_matches("0001", "1"));
        assert!(id_matches("1", "1"));
        assert!(id_matches("0", "000"));
        assert!(!id_matches("10", "1"));
    }

    #[test]
    fn accent_remap() {
        assert_eq!(fix_accents("caf\u{9f77}"), "café");
        assert_eq!(fix_accents("plain"), "plain");
    }
}

#[derive(Debug, Clone)]
pub struct Song {
    pub song_id: String,
    pub title: String,
    pub artist: String,

    pub filename: String,
    pub version: String,
    pub inf_ver: String,
    pub bpm: u32,
    pub release_date: u32,
    pub genres: Vec<&'static str>,
}

impl Song {
    pub fn padded_id(&self) -> String {
        format!("{:0>4}", self.song_id)
    }
}

pub fn id_matches(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> &str {
        let t = s.trim_start_matches('0');
        if t.is_empty() {
            "0"
        } else {
            t
        }
    }
    norm(a) == norm(b)
}

#[derive(Default)]
pub struct Filters<'a> {
    pub version: Option<&'a str>,

    pub ids: &'a [String],
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Result<&'a str> {
    node.children()
        .find(|n| n.has_tag_name(name))
        .and_then(|n| n.text())
        .with_context(|| format!("missing <{name}> element"))
}

pub fn parse_music_db(db_path: &Path, filters: &Filters) -> Result<Vec<Song>> {
    let bytes =
        fs::read(db_path).with_context(|| format!("cannot read {}", db_path.display()))?;

    let (text, _, _) = encoding_rs::SHIFT_JIS.decode(&bytes);

    let text = match text.strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map(|(_, t)| t).unwrap_or(rest),
        None => &text,
    };

    let doc = roxmltree::Document::parse(text).context("music_db.xml is not valid XML")?;
    let mut songs = Vec::new();

    for music in doc
        .root_element()
        .children()
        .filter(|n| n.has_tag_name("music"))
    {
        let song_id = music
            .attribute("id")
            .context("<music> element without id attribute")?;
        if EXCLUDED_IDS.contains(&song_id) {
            continue;
        }
        if !filters.ids.is_empty() && !filters.ids.iter().any(|i| id_matches(i, song_id)) {
            continue;
        }

        let info = music
            .children()
            .find(|n| n.has_tag_name("info"))
            .with_context(|| format!("song {song_id}: missing <info>"))?;

        let parse = |name: &str| -> Result<&str> {
            child_text(info, name).with_context(|| format!("song {song_id}"))
        };

        let version = parse("version")?;
        if let Some(target) = filters.version {
            if version != target {
                continue;
            }
        }

        let release_date: u32 = parse("distribution_date")?
            .parse()
            .with_context(|| format!("song {song_id}: bad distribution_date"))?;

        let bpm_max: u32 = parse("bpm_max")?
            .parse()
            .with_context(|| format!("song {song_id}: bad bpm_max"))?;
        let genre_mask: u32 = parse("genre")?
            .parse()
            .with_context(|| format!("song {song_id}: bad genre"))?;

        songs.push(Song {
            song_id: song_id.to_string(),
            title: fix_accents(parse("title_name")?),
            artist: fix_accents(parse("artist_name")?),
            filename: parse("ascii")?.to_string(),
            version: version.to_string(),
            inf_ver: parse("inf_ver")?.to_string(),
            bpm: bpm_max / 100,
            release_date,
            genres: genre_names(genre_mask),
        });
    }

    Ok(songs)
}
