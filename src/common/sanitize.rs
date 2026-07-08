use std::path::{Path, PathBuf};

pub fn full_width(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            '\\' => out.push('＼'),
            '/' => out.push('⁄'),
            ':' => out.push('։'),
            '*' => out.push('⁎'),
            '?' => out.push('？'),
            '"' => {}
            '<' => out.push('‹'),
            '>' => out.push('›'),
            '|' => out.push('ǀ'),
            other => out.push(other),
        }
    }

    out.trim().trim_end_matches('.').trim_end().to_string()
}

pub fn fancy_quotes(s: &str) -> String {
    let mut count = 0;
    s.chars()
        .map(|c| {
            if c == '"' {
                let r = if count % 2 == 0 { '\u{201C}' } else { '\u{201D}' };
                count += 1;
                r
            } else {
                c
            }
        })
        .collect()
}

pub fn clean(name: &str) -> String {
    full_width(&fancy_quotes(name))
}

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn guard_component(name: &str) -> String {
    let stem = name.split('.').next().unwrap_or(name);
    if RESERVED.iter().any(|r| stem.eq_ignore_ascii_case(r)) {
        format!("_{name}")
    } else {
        name.to_string()
    }
}

pub fn fit_stem(dir: &Path, stem: &str, ext: &str) -> String {
    const MAX_PATH: usize = 259;
    const PART_SUFFIX: usize = 5;
    const MIN_STEM: usize = 20;

    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let dir_len = abs.to_string_lossy().encode_utf16().count();
    let overhead = dir_len + 1   + 1   + ext.encode_utf16().count() + PART_SUFFIX;
    let budget = MAX_PATH.saturating_sub(overhead).max(MIN_STEM);

    if stem.encode_utf16().count() <= budget {
        return stem.to_string();
    }
    let mut used = 1;
    let mut out = String::new();
    for c in stem.chars() {
        let w = c.len_utf16();
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }

    let trimmed = out.trim_end().trim_end_matches('.').len();
    out.truncate(trimmed);
    out.push('…');
    out
}

pub fn output_path(out_root: &Path, album: &str, stem: &str, ext: &str) -> PathBuf {
    let dir = full_width(album);
    let stem = guard_component(stem);
    let stem = fit_stem(&out_root.join(&dir), &stem, ext);
    out_root.join(dir).join(format!("{stem}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_width_replaces_illegal() {
        assert_eq!(full_width("A/B:C*D?"), "A⁄B։C⁎D？");
        assert_eq!(full_width("no|pipe"), "no\u{01c0}pipe");
        assert_eq!(full_width("say \"hi\""), "say hi");
    }

    #[test]
    fn full_width_trims_trailing_dot_space() {
        assert_eq!(full_width("name.  "), "name");
    }

    #[test]
    fn fancy_quotes_alternate() {
        assert_eq!(fancy_quotes("\"a\" \"b\""), "\u{201c}a\u{201d} \u{201c}b\u{201d}");
    }

    #[test]
    fn clean_combines() {
        assert_eq!(clean("hi \"there\"/x"), "hi \u{201c}there\u{201d}⁄x");
    }

    #[test]
    fn reserved_components_guarded() {
        assert_eq!(guard_component("CON"), "_CON");
        assert_eq!(guard_component("aux.flac"), "_aux.flac");
        assert_eq!(guard_component("Console"), "Console");

        assert_eq!(guard_component("con - song"), "con - song");
    }

    #[test]
    fn fit_stem_truncates_long_names() {
        let dir = Path::new("out");
        assert_eq!(fit_stem(dir, "short", "flac"), "short");
        let long = "x".repeat(400);
        let fitted = fit_stem(dir, &long, "flac");
        assert!(fitted.ends_with('…'));
        assert!(fitted.encode_utf16().count() < 260);
        assert!(fitted.encode_utf16().count() >= 20);
    }
}
