use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SoundEvent {
    pub offset: i32,

    pub keysound_id: i32,

    pub key: i32,
    pub player: i32,
}

impl SoundEvent {
    fn new(offset: i32, keysound_id: i32, key: i32, player: i32) -> Self {
        SoundEvent {
            offset,
            keysound_id,
            key,
            player,
        }
    }
}

const CMD_KEY_P1: u8 = 0;
const CMD_KEY_P2: u8 = 1;
const CMD_LOAD_SAMPLE_P1: u8 = 2;
const CMD_LOAD_SAMPLE_P2: u8 = 3;
const CMD_END: u8 = 6;
const CMD_BGM_NOTE: u8 = 7;

fn retro_update_key(sounds: &mut [SoundEvent], player: i32, key: i32, offset: i32, keysound_id: i32) {
    for s in sounds.iter_mut() {
        if s.player == player && s.key == key && s.offset >= offset {
            s.keysound_id = keysound_id;
        }
    }
}

fn parse_chart_data(data: &[u8]) -> Vec<SoundEvent> {
    let mut sounds: Vec<SoundEvent> = Vec::new();
    let mut loaded_samples: [HashMap<i32, i32>; 2] = [HashMap::new(), HashMap::new()];

    let mut pos = 0usize;
    while pos + 8 <= data.len() {
        let offset = i32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        let command = data[pos + 4];
        let mut param = data[pos + 5] as i32;
        let value = i16::from_le_bytes(data[pos + 6..pos + 8].try_into().unwrap()) as i32;
        pos += 8;

        if offset == 0x7fffffff {
            break;
        }

        match command {
            CMD_KEY_P1 | CMD_KEY_P2 => {
                let player = (command - CMD_KEY_P1) as i32;

                if param == 0x6b {
                    param = 7;
                }

                if value != 0 && param == 7 {
                    if let Some(&ks) = loaded_samples[player as usize].get(&param) {
                        sounds.push(SoundEvent::new(offset + value, ks, param, player));
                    }
                }

                if let Some(&ks) = loaded_samples[player as usize].get(&param) {
                    sounds.push(SoundEvent::new(offset, ks, param, player));
                }
            }

            CMD_LOAD_SAMPLE_P1 | CMD_LOAD_SAMPLE_P2 => {
                let player = (command - CMD_LOAD_SAMPLE_P1) as i32;
                let keysound_id = value - 1;
                loaded_samples[player as usize].insert(param, keysound_id);
                retro_update_key(&mut sounds, player, param, offset, keysound_id);
            }

            CMD_BGM_NOTE => {
                sounds.push(SoundEvent::new(offset, value - 1, -1, 0));
            }

            CMD_END => {
                sounds.push(SoundEvent::new(offset, -1, -1, param));
            }

            _ => {}
        }
    }

    sounds
}

pub fn parse_chart(data: &[u8], preference: &[i32]) -> Option<(i32, Vec<SoundEvent>)> {
    for &id in preference {
        if !(0..=12).contains(&id) {
            continue;
        }
        let entry = id as usize * 8;
        if entry + 8 > data.len() {
            continue;
        }
        let offset = u32::from_le_bytes(data[entry..entry + 4].try_into().unwrap()) as usize;
        let filesize = i32::from_le_bytes(data[entry + 4..entry + 8].try_into().unwrap());
        if offset == 0 || offset >= data.len() || filesize <= 0 {
            continue;
        }

        let end = offset.saturating_add(filesize as usize).min(data.len());
        let events = parse_chart_data(&data[offset..end]);
        if !events.is_empty() {
            return Some((id, events));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(offset: i32, cmd: u8, param: u8, value: i16) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0..4].copy_from_slice(&offset.to_le_bytes());
        b[4] = cmd;
        b[5] = param;
        b[6..8].copy_from_slice(&value.to_le_bytes());
        b
    }

    #[test]
    fn parses_chart_zero() {
        let mut stream = Vec::new();
        stream.extend(ev(0, CMD_LOAD_SAMPLE_P1, 0, 5));
        stream.extend(ev(0, CMD_KEY_P1, 0, 0));
        stream.extend(ev(500, CMD_KEY_P1, 0, 0));
        stream.extend(ev(0, CMD_BGM_NOTE, 0, 3));
        stream.extend(ev(2000, CMD_END, 0, 0));
        stream.extend(ev(0x7fffffff, 0, 0, 0));

        let mut data = vec![0u8; 13 * 8];
        data[0..4].copy_from_slice(&(13u32 * 8).to_le_bytes());
        data[4..8].copy_from_slice(&(stream.len() as i32).to_le_bytes());
        data.extend(stream);

        let (id, events) = parse_chart(&data, &[0]).expect("chart 0");
        assert_eq!(id, 0);

        assert_eq!(events.len(), 4);
        assert_eq!(events[0].keysound_id, 4);
        assert_eq!(events[0].offset, 0);
        assert_eq!(events[1].offset, 500);

        assert!(events.iter().any(|e| e.keysound_id == 2 && e.key == -1));
    }

    #[test]
    fn filesize_bounds_the_chart_stream() {
        let mut stream = Vec::new();
        stream.extend(ev(0, CMD_LOAD_SAMPLE_P1, 0, 1));
        stream.extend(ev(0, CMD_KEY_P1, 0, 0));
        let stream_len = stream.len();

        stream.extend(ev(100, CMD_KEY_P1, 0, 0));

        let mut data = vec![0u8; 13 * 8];
        data[0..4].copy_from_slice(&(13u32 * 8).to_le_bytes());
        data[4..8].copy_from_slice(&(stream_len as i32).to_le_bytes());
        data.extend(stream);

        let (_, events) = parse_chart(&data, &[0]).expect("chart 0");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].offset, 0);
    }

    #[test]
    fn empty_slot_falls_through_preference() {
        let mut stream = Vec::new();
        stream.extend(ev(0, CMD_LOAD_SAMPLE_P1, 0, 1));
        stream.extend(ev(0, CMD_KEY_P1, 0, 0));
        stream.extend(ev(0x7fffffff, 0, 0, 0));

        let mut data = vec![0u8; 13 * 8];
        data[2 * 8..2 * 8 + 4].copy_from_slice(&(13u32 * 8).to_le_bytes());
        data[2 * 8 + 4..2 * 8 + 8].copy_from_slice(&(stream.len() as i32).to_le_bytes());
        data.extend(stream);

        let (id, _) = parse_chart(&data, &[0, 1, 2]).expect("chart 2");
        assert_eq!(id, 2);
    }
}
