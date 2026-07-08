#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayEvent {
    pub offset_ms: u32,
    pub keysound: u16,
}

const CMD_NOTE: u8 = 1;
const CMD_ASSIGN: u8 = 2;
const CMD_BG_START: u8 = 3;
const CMD_AUTO: u8 = 7;

const BUTTONS: usize = 16;

fn detect_stride(data: &[u8]) -> Option<usize> {
    if data.get(16) == Some(&0x45) {
        Some(12)
    } else if data.get(12) == Some(&0x45) {
        Some(8)
    } else {
        None
    }
}

pub fn parse(data: &[u8]) -> Option<Vec<PlayEvent>> {
    let stride = detect_stride(data)?;
    let mut assigned = [0u16; BUTTONS];
    let mut events = Vec::new();

    let mut pos = 0;
    while pos + 8 <= data.len() {
        let offset_ms = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        let command = data[pos + 5];
        let param = u16::from_le_bytes(data[pos + 6..pos + 8].try_into().unwrap());
        pos += stride;

        match command {
            CMD_NOTE => {
                let button = (param & 0xF) as usize;
                let keysound = assigned[button];

                if keysound != 0 {
                    events.push(PlayEvent { offset_ms, keysound });
                }
            }
            CMD_ASSIGN => {
                let button = (param >> 12) as usize;
                assigned[button] = param & 0xFFF;
            }
            CMD_BG_START => events.push(PlayEvent { offset_ms, keysound: 0 }),
            CMD_AUTO => {
                let keysound = param & 0xFFF;
                if keysound != 0 {
                    events.push(PlayEvent { offset_ms, keysound });
                }
            }
            _ => {}
        }
    }
    Some(events)
}

pub const CHART_PREFERENCE: &[&str] = &["op", "hp", "np", "ep", "bp"];

#[cfg(test)]
mod tests {
    use super::*;

    fn ev8(offset: u32, cmd: u8, param: u16) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0..4].copy_from_slice(&offset.to_le_bytes());
        b[4] = 0x45;
        b[5] = cmd;
        b[6..8].copy_from_slice(&param.to_le_bytes());
        b
    }

    fn ev12(offset: u32, cmd: u8, param: u16) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[0..8].copy_from_slice(&ev8(offset, cmd, param));
        b
    }

    #[test]
    fn old_format_notes_play_assigned_samples() {
        let mut d = Vec::new();
        d.extend(ev8(0, CMD_ASSIGN, (2 << 12) | 7));
        d.extend(ev8(0, CMD_BG_START, 0x0d));
        d.extend(ev8(500, CMD_NOTE, 2));
        d.extend(ev8(600, CMD_NOTE, 3));
        d.extend(ev8(700, CMD_AUTO, 9));
        let events = parse(&d).expect("old chart");
        assert_eq!(
            events,
            vec![
                PlayEvent { offset_ms: 0, keysound: 0 },
                PlayEvent { offset_ms: 500, keysound: 7 },
                PlayEvent { offset_ms: 700, keysound: 9 },
            ]
        );
    }

    #[test]
    fn new_format_skips_long_note_words() {
        let mut d = Vec::new();
        d.extend(ev12(0, CMD_ASSIGN, (1 << 12) | 4));
        d.extend(ev12(250, CMD_NOTE, 1));
        let events = parse(&d).expect("new chart");
        assert_eq!(events, vec![PlayEvent { offset_ms: 250, keysound: 4 }]);
    }

    #[test]
    fn reassignment_changes_later_notes_only() {
        let mut d = Vec::new();
        d.extend(ev8(0, CMD_ASSIGN, 5));
        d.extend(ev8(100, CMD_NOTE, 0));
        d.extend(ev8(200, CMD_ASSIGN, 6));
        d.extend(ev8(300, CMD_NOTE, 0));
        let events = parse(&d).expect("chart");
        assert_eq!(events[0].keysound, 5);
        assert_eq!(events[1].keysound, 6);
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse(&[0u8; 32]).is_none());
        assert!(parse(b"").is_none());
    }
}
