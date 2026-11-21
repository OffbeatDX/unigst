use std::path::{Path, PathBuf};

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string()
}

/// The game's main DLL in `modules/` or the folder root, if it is a real PE
/// image (starts with `MZ`) — an empty or renamed placeholder doesn't count.
pub fn find_game_dll(input: &Path, name: &str) -> Option<PathBuf> {
    let name = name.to_ascii_lowercase();
    find_game_dll_by(input, |n| n == name)
}

/// Like [`find_game_dll`], for DLLs whose name varies. `matches` gets the
/// lowercased file name. `modules/` is searched before the root; within a
/// folder the alphabetically first match wins (so `popn.dll` < `popn22.dll`).
pub fn find_game_dll_by(input: &Path, matches: impl Fn(&str) -> bool) -> Option<PathBuf> {
    for dir in [input.join("modules"), input.to_path_buf()] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut found: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_ascii_lowercase();
                let path = e.path();
                (matches(&name) && path.is_file()).then_some((name, path))
            })
            .collect();
        found.sort();
        if let Some((_, path)) = found.into_iter().find(|(_, p)| is_pe(p)) {
            return Some(path);
        }
    }
    None
}

fn is_pe(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && magic == *b"MZ"
}

pub fn first_existing_dir(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|p| p.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("unigst-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn game_dll_must_be_a_pe_image() {
        let d = tmp("dll");
        std::fs::create_dir_all(d.join("modules")).unwrap();

        // Empty placeholder: rejected.
        std::fs::write(d.join("modules").join("game.dll"), b"").unwrap();
        assert!(find_game_dll(&d, "game.dll").is_none());

        // Wrong magic: rejected.
        std::fs::write(d.join("modules").join("game.dll"), b"not a dll").unwrap();
        assert!(find_game_dll(&d, "game.dll").is_none());

        // PE image in modules/: found.
        std::fs::write(d.join("modules").join("game.dll"), b"MZ\x90\x00").unwrap();
        assert_eq!(
            find_game_dll(&d, "game.dll"),
            Some(d.join("modules").join("game.dll"))
        );

        // Case-insensitive.
        std::fs::rename(
            d.join("modules").join("game.dll"),
            d.join("modules").join("GAME.DLL"),
        )
        .unwrap();
        assert!(find_game_dll(&d, "game.dll").is_some());
        std::fs::rename(
            d.join("modules").join("GAME.DLL"),
            d.join("modules").join("game.dll"),
        )
        .unwrap();

        // PE image in the root is accepted too.
        std::fs::remove_file(d.join("modules").join("game.dll")).unwrap();
        std::fs::write(d.join("game.dll"), b"MZ").unwrap();
        assert_eq!(find_game_dll(&d, "game.dll"), Some(d.join("game.dll")));

        let _ = std::fs::remove_dir_all(&d);
    }
}
