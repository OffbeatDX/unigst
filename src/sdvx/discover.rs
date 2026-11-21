use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use super::db::{diff_name, inf_name, Song};

pub struct AudioVariant {
    pub audio: PathBuf,

    pub jacket: Option<PathBuf>,

    pub diff_label: Option<String>,
}

fn is_pre_or_fx(stem: &str) -> bool {
    let core = match stem.as_bytes() {
        [.., b'_', d, c] if d.is_ascii_digit() && c.is_ascii_alphabetic() => {
            &stem[..stem.len() - 3]
        }
        _ => stem,
    };
    core.ends_with("_pre") || core.ends_with("_fx")
}

const JACKET_SIZES: [&str; 3] = ["_b", "", "_s"];

fn jacket_for_diff(folder: &Path, id: &str, digit: char) -> Option<PathBuf> {
    JACKET_SIZES.iter().find_map(|suffix| {
        let p = folder.join(format!("jk_{id}_{digit}{suffix}.png"));
        p.exists().then_some(p)
    })
}

fn best_jacket(folder: &Path, id: &str) -> Option<PathBuf> {
    (b'0'..=b'9')
        .rev()
        .find_map(|d| jacket_for_diff(folder, id, d as char))
}

fn split_diff_suffix(stem: &str) -> Option<(char, char)> {
    match stem.as_bytes() {
        [.., b'_', digit, diff] if digit.is_ascii_digit() && diff.is_ascii_alphabetic() => {
            Some((*diff as char, *digit as char))
        }
        _ => None,
    }
}

pub fn get_variants(
    song: &Song,
    music_dir: &Path,
    dummy_jacket: Option<&Path>,
) -> Result<Vec<AudioVariant>> {
    let id = song.padded_id();
    let folder = music_dir.join(format!("{id}_{}", song.filename));
    if !folder.is_dir() {
        bail!("folder not found: {}", folder.display());
    }

    let mut audio_files: Vec<PathBuf> = Vec::new();

    for entry in fs::read_dir(&folder)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(stem) = name.strip_suffix(".s3v") {
            if !is_pre_or_fx(stem) {
                audio_files.push(path);
            }
        }
    }
    audio_files.sort();

    if audio_files.is_empty() {
        bail!("no audio in {}", folder.display());
    }

    let fallback_jacket = best_jacket(&folder, &id).or_else(|| dummy_jacket.map(Path::to_path_buf));

    if audio_files.len() == 1 {
        return Ok(vec![AudioVariant {
            audio: audio_files.pop().unwrap(),
            jacket: fallback_jacket,
            diff_label: None,
        }]);
    }

    let mut variants = Vec::with_capacity(audio_files.len());
    for audio in audio_files {
        let stem = audio
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();

        let Some((diff_char, diff_digit)) = split_diff_suffix(stem) else {
            variants.push(AudioVariant {
                audio,
                jacket: fallback_jacket.clone(),
                diff_label: None,
            });
            continue;
        };

        let diff_label = diff_name(diff_char)
            .or_else(|| inf_name(&song.inf_ver))
            .map(String::from)
            .unwrap_or_else(|| diff_char.to_string());

        let jacket = jacket_for_diff(&folder, &id, diff_digit).or_else(|| fallback_jacket.clone());

        variants.push(AudioVariant {
            audio,
            jacket,
            diff_label: Some(diff_label),
        });
    }

    Ok(variants)
}

#[cfg(test)]
mod tests {
    use super::{is_pre_or_fx, split_diff_suffix};

    #[test]
    fn pre_and_fx_variants_are_skipped() {
        assert!(is_pre_or_fx("0026_gorilla_pinocchio_pre"));
        assert!(is_pre_or_fx("0026_gorilla_pinocchio_pre_4i"));
        assert!(is_pre_or_fx("0001_song_fx"));
        assert!(is_pre_or_fx("0001_song_fx_2a"));
    }

    #[test]
    fn real_audio_is_kept() {
        assert!(!is_pre_or_fx("0026_gorilla_pinocchio"));
        assert!(!is_pre_or_fx("0026_gorilla_pinocchio_4i"));
        assert!(!is_pre_or_fx("1758_helpme_erin_remix_marines_5m"));
        assert!(!is_pre_or_fx("0400_prelude_ens"));
    }

    #[test]
    fn diff_suffix_requires_underscore() {
        assert_eq!(split_diff_suffix("0026_song_4i"), Some(('i', '4')));
        assert_eq!(split_diff_suffix("1758_remix_5m"), Some(('m', '5')));
        assert_eq!(split_diff_suffix("0500_song2u"), None);
    }
}
