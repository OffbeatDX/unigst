// Detection requires the game DLL *and* the song data.
use std::path::{Path, PathBuf};

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("unigst-detect-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn touch(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
}

#[test]
fn sdvx_needs_dll_and_music_db() {
    let d = tmp("sdvx");
    touch(&d.join("data/others/music_db.xml"), b"<mdb/>");
    assert!(!unigst::sdvx::looks_like(&d), "data without DLL");
    touch(&d.join("modules/soundvoltex.dll"), b"MZ");
    assert!(unigst::sdvx::looks_like(&d));
    assert!(!unigst::iidx::looks_like(&d) && !unigst::popn::looks_like(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn iidx_needs_dll_and_music_data() {
    let d = tmp("iidx");
    touch(&d.join("modules/bm2dx.dll"), b"MZ");
    assert!(!unigst::iidx::looks_like(&d), "DLL without music_data.bin");
    touch(&d.join("data/info/0/music_data.bin"), b"IIDX");
    assert!(unigst::iidx::looks_like(&d));
    assert!(!unigst::sdvx::looks_like(&d) && !unigst::popn::looks_like(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn popn_needs_dll_and_sd() {
    let d = tmp("popn");
    std::fs::create_dir_all(d.join("data/sd")).unwrap();
    assert!(!unigst::popn::looks_like(&d), "sd/ without DLL");
    touch(&d.join("popn.dll"), b"");
    assert!(!unigst::popn::looks_like(&d), "placeholder DLL");
    touch(&d.join("popn.dll"), b"MZ");
    assert!(unigst::popn::looks_like(&d));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn popn_accepts_numbered_dll_but_not_mods() {
    let d = tmp("popn-numbered");
    std::fs::create_dir_all(d.join("data/sd")).unwrap();
    touch(&d.join("modules/popnhax.dll"), b"MZ");
    assert!(
        !unigst::popn::looks_like(&d),
        "a mod DLL alone is not the game"
    );
    touch(&d.join("modules/popn22.dll"), b"MZ");
    assert!(unigst::popn::looks_like(&d));
    let _ = std::fs::remove_dir_all(&d);
}
