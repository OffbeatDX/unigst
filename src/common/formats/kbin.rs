use anyhow::{bail, Result};

const SIGNATURE: u8 = 0xA0;
const SIG_COMPRESSED: u8 = 0x42;
const SIG_UNCOMPRESSED: u8 = 0x45;

const NODE_END: u8 = 190;
const END_SECTION: u8 = 191;
const NODE_START: u8 = 1;
const NODE_ATTR: u8 = 46;

#[derive(Debug, Default)]
pub struct KbinNode {
    pub name: String,
    pub text: Option<String>,
    pub children: Vec<KbinNode>,
}

fn type_info(id: u8) -> Option<(i32, usize)> {
    let t = match id {
        1 => return None,
        2 | 3 | 52 => (1, 1),
        4 | 5 => (1, 2),
        6 | 7 | 12 | 13 | 14 => (1, 4),
        8 | 9 | 15 => (1, 8),
        10 | 11 => (-1, 1),
        16 | 17 | 53 => (2, 1),
        18 | 19 => (2, 2),
        20 | 21 | 24 => (2, 4),
        22 | 23 | 25 => (2, 8),
        26 | 27 | 54 => (3, 1),
        28 | 29 => (3, 2),
        30 | 31 | 34 => (3, 4),
        32 | 33 | 35 => (3, 8),
        36 | 37 | 55 => (4, 1),
        38 | 39 => (4, 2),
        40 | 41 | 44 => (4, 4),
        42 | 43 | 45 => (4, 8),
        48 | 49 | 56 => (16, 1),
        50 | 51 => (8, 2),
        _ => return None,
    };
    Some(t)
}

const SIXBIT: &[u8] = b"0123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz";

struct Decoder<'a> {
    data: &'a [u8],
    node_off: usize,
    node_end: usize,

    d_off: usize,

    b_off: usize,
    w_off: usize,
    compressed: bool,
}

impl<'a> Decoder<'a> {
    fn node_u8(&mut self) -> Result<u8> {
        let b = *self
            .data
            .get(self.node_off)
            .ok_or_else(|| anyhow::anyhow!("node EOF"))?;
        self.node_off += 1;
        Ok(b)
    }

    fn node_peek(&self) -> Result<u8> {
        self.data
            .get(self.node_off)
            .copied()
            .ok_or_else(|| anyhow::anyhow!("node EOF"))
    }

    fn be_u32(&self, off: usize) -> Result<u32> {
        let s = self
            .data
            .get(off..off + 4)
            .ok_or_else(|| anyhow::anyhow!("data EOF"))?;
        Ok(u32::from_be_bytes(s.try_into().unwrap()))
    }

    fn read_name(&mut self) -> Result<String> {
        if self.compressed {
            let len = self.node_u8()? as usize;
            let bits_len = len * 6;
            let bytes_len = bits_len.div_ceil(8);
            let raw = self
                .data
                .get(self.node_off..self.node_off + bytes_len)
                .ok_or_else(|| anyhow::anyhow!("name EOF"))?;
            self.node_off += bytes_len;

            let mut chars = vec![0u8; len];
            for (i, c) in chars.iter_mut().enumerate() {
                let bit = i * 6;
                let hi = raw[bit / 8] as u16;
                let lo = *raw.get(bit / 8 + 1).unwrap_or(&0) as u16;
                let idx = ((hi << 8 | lo) >> (10 - bit % 8)) & 0x3F;
                *c = SIXBIT[idx as usize];
            }
            Ok(String::from_utf8_lossy(&chars).into_owned())
        } else {
            let len = (self.node_u8()? & !0x40) as usize + 1;
            let raw = self
                .data
                .get(self.node_off..self.node_off + len)
                .ok_or_else(|| anyhow::anyhow!("name EOF"))?;
            self.node_off += len;
            Ok(String::from_utf8_lossy(raw).into_owned())
        }
    }

    fn realign_d(&mut self) {
        while !self.d_off.is_multiple_of(4) {
            self.d_off += 1;
        }
    }

    fn grab_aligned(&mut self, count: i32, elem: usize) -> Result<Vec<u8>> {
        let total = (count as usize)
            .checked_mul(elem)
            .ok_or_else(|| anyhow::anyhow!("data overflow"))?;

        if self.b_off.is_multiple_of(4) {
            self.b_off = self.d_off;
        }
        if self.w_off.is_multiple_of(4) {
            self.w_off = self.d_off;
        }
        let out;
        if total == 1 {
            let end = self
                .b_off
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("data EOF"))?;
            out = self
                .data
                .get(self.b_off..end)
                .ok_or_else(|| anyhow::anyhow!("data EOF"))?
                .to_vec();
            self.b_off = end;
        } else if total == 2 {
            let end = self
                .w_off
                .checked_add(2)
                .ok_or_else(|| anyhow::anyhow!("data EOF"))?;
            out = self
                .data
                .get(self.w_off..end)
                .ok_or_else(|| anyhow::anyhow!("data EOF"))?
                .to_vec();
            self.w_off = end;
        } else {
            let end = self
                .d_off
                .checked_add(total)
                .ok_or_else(|| anyhow::anyhow!("data EOF"))?;
            out = self
                .data
                .get(self.d_off..end)
                .ok_or_else(|| anyhow::anyhow!("data EOF"))?
                .to_vec();
            self.d_off = end;
            self.realign_d();
        }
        let trailing = self.b_off.max(self.w_off);
        if self.d_off < trailing {
            self.d_off = trailing;
            self.realign_d();
        }
        Ok(out)
    }

    fn grab_var(&mut self) -> Result<Vec<u8>> {
        let size = self.be_u32(self.d_off)? as usize;
        self.d_off += 4;
        let end = self
            .d_off
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("data EOF"))?;
        let out = self
            .data
            .get(self.d_off..end)
            .ok_or_else(|| anyhow::anyhow!("data EOF"))?
            .to_vec();
        self.d_off = end;
        self.realign_d();
        Ok(out)
    }

    fn format_value(&self, id: u8, bytes: &[u8]) -> String {
        match id {
            10 => bytes.iter().map(|b| format!("{b:02x}")).collect(),
            11 => {
                let (s, _, _) = encoding_rs::SHIFT_JIS.decode(bytes);
                s.trim_end_matches('\0').to_string()
            }
            _ => {
                let (_, elem) = type_info(id).unwrap_or((1, 1));
                let mut parts = Vec::new();
                match elem {
                    1 => {
                        for &b in bytes {
                            parts.push((b as i8 as i64).to_string());
                        }
                    }
                    2 => {
                        for &c in bytes.as_chunks::<2>().0 {
                            parts.push((i16::from_be_bytes(c) as i64).to_string());
                        }
                    }
                    4 => {
                        for &c in bytes.as_chunks::<4>().0 {
                            parts.push((i32::from_be_bytes(c) as i64).to_string());
                        }
                    }
                    8 => {
                        for &c in bytes.as_chunks::<8>().0 {
                            parts.push(i64::from_be_bytes(c).to_string());
                        }
                    }
                    _ => {}
                }
                parts.join(" ")
            }
        }
    }
}

pub fn decode(input: &[u8]) -> Result<KbinNode> {
    if input.len() < 8 || input[0] != SIGNATURE {
        bail!("not a KBIN document");
    }
    let compressed = match input[1] {
        SIG_COMPRESSED => true,
        SIG_UNCOMPRESSED => false,
        other => bail!("bad KBIN compression flag {other:#x}"),
    };

    let node_len = u32::from_be_bytes(input[4..8].try_into().unwrap()) as usize;
    let node_end = 8 + node_len;

    let mut dec = Decoder {
        data: input,
        node_off: 8,
        node_end,

        d_off: node_end + 4,
        b_off: node_end,
        w_off: node_end,
        compressed,
    };

    let mut root = KbinNode {
        name: "root".into(),
        ..Default::default()
    };
    let mut stack: Vec<KbinNode> = vec![std::mem::take(&mut root)];

    while dec.node_off < dec.node_end {
        while dec.node_off < dec.node_end && dec.node_peek()? == 0 {
            dec.node_off += 1;
        }
        if dec.node_off >= dec.node_end {
            break;
        }
        let raw = dec.node_u8()?;
        let is_array = raw & 0x40 != 0;
        let node_type = raw & !0x40;

        if node_type == END_SECTION {
            break;
        }
        if node_type == NODE_END {
            let done = stack
                .pop()
                .ok_or_else(|| anyhow::anyhow!("stack underflow"))?;
            stack
                .last_mut()
                .ok_or_else(|| anyhow::anyhow!("stack underflow"))?
                .children
                .push(done);
            continue;
        }

        let name = dec.read_name()?;

        if node_type == NODE_ATTR {
            let _ = dec.grab_var()?;
            continue;
        }

        stack.push(KbinNode {
            name: fix_name(&name),
            ..Default::default()
        });

        if node_type == NODE_START {
            continue;
        }

        let (count, elem) = match type_info(node_type) {
            Some(t) => t,
            None => continue,
        };

        let bytes = if count == -1 || is_array {
            dec.grab_var()?
        } else {
            dec.grab_aligned(count, elem)?
        };

        let text = dec.format_value(node_type, &bytes);
        if let Some(top) = stack.last_mut() {
            top.text = Some(text);
        }
    }

    let root = stack.into_iter().next().unwrap_or_default();
    root.children
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty KBIN document"))
}

pub fn fix_name(n: &str) -> String {
    let mut s = n.replace("_E", ".").replace("__", "_");
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'_' && bytes[1].is_ascii_digit() {
        s = s[1..].to_string();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_name_reverses_packing() {
        assert_eq!(fix_name("_01000_E1"), "01000.1");
        assert_eq!(fix_name("_01000a_E2dx"), "01000a.2dx");
        assert_eq!(fix_name("_01000__pre_E2dx"), "01000_pre.2dx");
        assert_eq!(fix_name("_010001_E2dx"), "010001.2dx");
    }

    fn pack_sixbit(name: &str) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        for c in name.bytes() {
            let v = SIXBIT.iter().position(|&s| s == c).expect("sixbit char") as u8;
            for k in (0..6).rev() {
                bits.push(v >> k & 1 == 1);
            }
        }
        while !bits.len().is_multiple_of(8) {
            bits.push(false);
        }
        let mut out = vec![name.len() as u8];
        for chunk in bits.chunks(8) {
            out.push(chunk.iter().fold(0u8, |acc, &b| acc << 1 | b as u8));
        }
        out
    }

    #[test]
    fn sixbit_names_of_any_length_decode() {
        for name in ["_01000_E1", "exx__asatsuru_E2dx", "exx__asatsuru__pre_E2dx"] {
            let packed = pack_sixbit(name);
            let mut dec = Decoder {
                data: &packed,
                node_off: 0,
                node_end: packed.len(),
                d_off: 0,
                b_off: 0,
                w_off: 0,
                compressed: true,
            };
            assert_eq!(dec.read_name().expect("decode"), name);
        }
    }
}
