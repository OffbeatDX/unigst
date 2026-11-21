use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use super::kbin::{self, KbinNode};

const IFS_SIGNATURE: u32 = 0x6CAD_8F89;

pub struct IfsIndex {
    path: PathBuf,

    base: u64,

    files: HashMap<String, (u32, u32)>,
}

impl IfsIndex {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn read_prefix(&self, name: &str, max: usize) -> Option<Vec<u8>> {
        let &(off, size) = self.files.get(name)?;
        self.read_range(off, (size as usize).min(max)).ok()
    }

    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        let &(off, size) = self.files.get(name)?;
        self.read_range(off, size as usize).ok()
    }

    fn read_range(&self, off: u32, len: usize) -> Result<Vec<u8>> {
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(self.base + off as u64))?;
        let mut buf = vec![0u8; len];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }
}

fn be_u32(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        d.get(o..o + 4)
            .context("IFS header truncated")?
            .try_into()
            .unwrap(),
    ))
}

fn be_u16(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(
        d.get(o..o + 2)
            .context("IFS header truncated")?
            .try_into()
            .unwrap(),
    ))
}

pub fn is_real_ifs(path: &Path) -> bool {
    let mut buf = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut buf))
        .map(|_| u32::from_be_bytes(buf) == IFS_SIGNATURE)
        .unwrap_or(false)
}

pub fn index(path: &Path) -> Result<IfsIndex> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;

    let mut head = [0u8; 0x24];
    f.read_exact(&mut head)
        .with_context(|| format!("reading {} header", path.display()))?;

    if be_u32(&head, 0)? != IFS_SIGNATURE {
        bail!("not an IFS file: {}", path.display());
    }
    let version = be_u16(&head, 4)?;

    let manifest_end = be_u32(&head, 0x10)? as usize;
    let manifest_start = if version > 1 { 0x24 } else { 0x14 };
    let file_len = f.metadata()?.len();
    if manifest_end < manifest_start || manifest_end as u64 > file_len {
        bail!("IFS manifest range invalid in {}", path.display());
    }

    let mut manifest = vec![0u8; manifest_end - manifest_start];
    f.seek(SeekFrom::Start(manifest_start as u64))?;
    f.read_exact(&mut manifest)
        .with_context(|| format!("reading {} manifest", path.display()))?;

    let root = kbin::decode(&manifest).context("decoding IFS manifest")?;
    let mut files = HashMap::new();
    collect(&root, &mut files);

    Ok(IfsIndex {
        path: path.to_path_buf(),
        base: manifest_end as u64,
        files,
    })
}

fn collect(node: &KbinNode, out: &mut HashMap<String, (u32, u32)>) {
    if node.children.is_empty() {
        if let Some(text) = &node.text {
            let ints: Vec<i64> = text
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();

            if ints.len() >= 2 && ints[0] >= 0 && ints[1] >= 0 {
                out.insert(node.name.clone(), (ints[0] as u32, ints[1] as u32));
            }
        }
        return;
    }
    for child in &node.children {
        collect(child, out);
    }
}
