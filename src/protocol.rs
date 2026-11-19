//! AULA F75 protocol, recovered from AULA's OemDrv.exe with Ghidra (docs/PROTOCOL.md).
//!
//! Two transports share the same payloads:
//! * 2.4 GHz receiver (3554:fa09) and Bluetooth (3554:fa08): `CDev3632`, 20-byte report 0x13
//!   `[13 cmd total index (sub<<4)|len data*14 checksum]`, checksum = sum(bytes 0..19).
//! * USB cable (258a:010c): `CDevG5KB`, 520-byte feature report 0x06
//!   `[06 cmd sub 00 total index size_lo size_hi data*512]`.

pub const LED_COUNT: usize = 126; // 21 columns x 6 rows; led = column * 6 + row
pub const MATRIX_LEN: usize = 128; // KB.ini MatrixLen
pub const LED_BLOCK_LEN: usize = 128;
pub const KEYMAP_LEN: usize = MATRIX_LEN * 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    Receiver,
    Bluetooth,
    Wired,
}

impl Link {
    pub fn name(self) -> &'static str {
        match self {
            Link::Receiver => "2.4G",
            Link::Bluetooth => "Bluetooth",
            Link::Wired => "Wired",
        }
    }
    pub fn wireless(self) -> bool {
        self != Link::Wired
    }
}

/// Command numbers differ per transport (CDev3632 vs CDevG5KB vtables).
#[derive(Clone, Copy)]
pub struct Cmds {
    pub keymap: u8,
    pub led_matrix: u8,
    pub macros: u8,
    pub led: u8,
    pub realtime: u8,
    pub read_flag: u8,
}

pub const WIRELESS: Cmds =
    Cmds { keymap: 0x01, led_matrix: 0x02, macros: 0x03, led: 0x04, realtime: 0x88, read_flag: 0x40 };
pub const WIRED: Cmds =
    Cmds { keymap: 0x03, led_matrix: 0x06, macros: 0x05, led: 0x04, realtime: 0x08, read_flag: 0x80 };
pub const WIRELESS_ONLINE: u8 = 0x07; // returns m_bOnline (1 = keyboard linked to receiver), not battery

pub fn cmds(link: Link) -> Cmds {
    if link.wireless() { WIRELESS } else { WIRED }
}

// ---------------------------------------------------------------- 20-byte wireless framing

pub const REPORT_ID: u8 = 0x13;
pub const PACKET_LEN: usize = 20;
pub const CHUNK: usize = 14;

pub fn checksum(p: &[u8; PACKET_LEN]) -> u8 {
    p[..PACKET_LEN - 1].iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

/// CDev3632::AccessData (0x004862f0).
pub fn write_packets(cmd: u8, sub: u8, data: &[u8]) -> Vec<[u8; PACKET_LEN]> {
    let total = data.len().div_ceil(CHUNK);
    data.chunks(CHUNK)
        .enumerate()
        .map(|(i, chunk)| {
            let mut p = [0u8; PACKET_LEN];
            p[0] = REPORT_ID;
            p[1] = cmd;
            p[2] = total as u8;
            p[3] = i as u8;
            p[4] = (sub << 4) | chunk.len() as u8;
            p[5..5 + chunk.len()].copy_from_slice(chunk);
            p[19] = checksum(&p);
            p
        })
        .collect()
}

/// 0x004865d0 / SendCMD_3632 (0x00487070).
pub fn read_request(cmd: u8, sub: u8) -> [u8; PACKET_LEN] {
    let mut p = [0u8; PACKET_LEN];
    p[0] = REPORT_ID;
    p[1] = cmd;
    p[2] = 1;
    p[4] = sub << 4;
    p[19] = checksum(&p);
    p
}

#[derive(Debug, Clone, Copy)]
pub struct Reply {
    pub cmd: u8,
    pub crc_error: bool,
    pub total: u8,
    pub index: u8,
    pub len: usize,
}

pub fn parse_reply(p: &[u8]) -> Option<Reply> {
    if p.len() < PACKET_LEN || p[0] != REPORT_ID {
        return None;
    }
    Some(Reply {
        cmd: p[1] & 0x7f,
        crc_error: p[1] & 0x80 != 0 || p[2] & 0x80 != 0,
        total: p[2] & 0x7f,
        index: p[3],
        len: ((p[4] & 0x0f) as usize).min(CHUNK),
    })
}

// ---------------------------------------------------------------- 520-byte wired framing

pub const WIRED_REPORT: u8 = 0x06;
pub const WIRED_LEN: usize = 520;
pub const WIRED_CHUNK: usize = 512;

/// CDevG5KB::AccessData (0x0048b4d0) and the inlined Get* readers (e.g. 0x0048bc00).
pub fn wired_packets(cmd: u8, sub: u8, len: usize, data: Option<&[u8]>) -> Vec<[u8; WIRED_LEN]> {
    let total = len.div_ceil(WIRED_CHUNK);
    (0..total)
        .map(|i| {
            let off = i * WIRED_CHUNK;
            let size = (len - off).min(WIRED_CHUNK);
            let mut p = [0u8; WIRED_LEN];
            p[0] = WIRED_REPORT;
            p[1] = cmd;
            p[2] = sub;
            p[4] = total as u8;
            p[5] = i as u8;
            p[6] = size as u8;
            p[7] = (size >> 8) as u8;
            if let Some(d) = data {
                p[8..8 + size].copy_from_slice(&d[off..off + size]);
            }
            p
        })
        .collect()
}

// ---------------------------------------------------------------- lighting

/// KB.ini `LedOpt*` + Text/en tc_kb_led*: (hardware id, name, has_speed, has_brightness, has_colour)
pub const EFFECTS: &[(u8, &str, bool, bool, bool)] = &[
    (1, "Fixed On", false, true, true),
    (2, "Respire", true, true, true),
    (3, "Rainbow", true, true, false),
    (4, "Flash Away", true, true, true),
    (5, "Raindrops", true, true, true),
    (6, "Rainbow Wheel", true, true, true),
    (7, "Ripples", true, true, true),
    (8, "Stars Twinkle", true, true, true),
    (9, "Shadow Fade", true, true, true),
    (10, "Retro Snake", true, true, true),
    (11, "Neon Stream", true, true, true),
    (12, "Reaction", true, true, true),
    (13, "Sine Wave", true, true, true),
    (14, "Scanner", true, true, true),
    (15, "Windmill", true, true, false),
    (16, "Waterfall", true, true, false),
    (17, "Blossoming", true, true, false),
    (18, "Storm", true, true, true),
    (21, "Per-key", false, true, false),
    (0, "Off", false, false, false),
];
pub const CUSTOM_MODE: u8 = 21;

pub const PALETTE: [[u8; 3]; 7] =
    [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0], [255, 0, 255], [0, 255, 255], [255, 255, 255]];
pub const RANDOM_COLOUR: u8 = 7;

/// KB.ini SleepUI / SleepHW (seconds -> hardware value); 0 disables sleep.
pub const SLEEP: &[(u16, u8)] =
    &[(0, 0), (60, 2), (90, 3), (120, 4), (180, 6), (240, 8), (300, 10), (600, 20), (900, 30), (1200, 40)];
pub const SIDE_LIGHT_OFFSET: usize = 0x12;
pub const SLEEP_OFFSET: usize = 0x18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SideLightMode {
    Off = 0,
    Fill = 1,
    Breathing = 2,
    Static = 3,
}

impl SideLightMode {
    pub const ALL: [Self; 4] = [Self::Off, Self::Fill, Self::Breathing, Self::Static];

    pub fn from_byte(value: u8) -> Option<Self> {
        Self::ALL.get(usize::from(value)).copied()
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Fill => "Fill",
            Self::Breathing => "Breathing",
            Self::Static => "Static",
        }
    }
}

/// Keep an unknown firmware value distinct from a missing field.
pub fn side_light_from_block(b: &[u8]) -> Option<u8> {
    b.get(SIDE_LIGHT_OFFSET).copied()
}

/// Edit a complete LED settings block without touching other settings.
pub fn edit_side_light(b: &mut [u8], mode: SideLightMode) {
    b[SIDE_LIGHT_OFFSET] = mode as u8;
}

pub fn led_block_valid(b: &[u8]) -> bool {
    b.len() >= LED_BLOCK_LEN && b[126] == 0x5a && b[127] == 0xa5
}

/// Fields written by 0x0048d810.
pub fn edit_led_block(b: &mut [u8], mode: u8, brightness: u8, speed: u8, colour: u8) {
    b[9] = (mode == CUSTOM_MODE) as u8;
    b[10] = mode;
    if let Some(slot) = EFFECTS.iter().position(|e| e.0 == mode) {
        let o = 0x3a + slot * 2;
        b[o] = brightness.min(9);
        b[o + 1] = (speed.min(4) << 4) | (colour & 0x0f);
    }
}

pub fn effect_from_block(b: &[u8]) -> Option<(usize, u8, u8, u8)> {
    let i = EFFECTS.iter().position(|e| e.0 == b[10])?;
    let o = 0x3a + i * 2;
    Some((i, b[o].min(9), (b[o + 1] >> 4).min(4), b[o + 1] & 0x0f))
}

/// Per-key planes for led_matrix: R[126] G[126] B[126] + padding to 384 (0x0048dbc0).
pub fn matrix_payload(c: &[[u8; 3]; LED_COUNT]) -> Vec<u8> {
    let mut buf = vec![0u8; MATRIX_LEN * 3];
    for (led, px) in c.iter().enumerate() {
        for ch in 0..3 {
            buf[ch * LED_COUNT + led] = px[ch];
        }
    }
    buf
}

/// Interleaved RGB frame for realtime, sub 0 (0x0047f250 builds 0x17a bytes).
pub fn realtime_payload(c: &[[u8; 3]; LED_COUNT]) -> Vec<u8> {
    c.iter().flatten().copied().collect()
}

// ---------------------------------------------------------------- key codes (0x0048c820)

/// 4-byte hardware code per matrix index: `[type, a, b, c]`.
/// type 0 = keyboard `[0, modifiers, 0, usage]`, 2 = consumer `[2, 0, hi, lo]`,
/// 3 = macro `[3, mode, count, macro_index]` (macros: see docs/PROTOCOL.md).
pub fn key_code(usage: u8, mods: u8) -> [u8; 4] {
    [0, mods, 0, usage]
}
pub fn consumer_code(usage: u16) -> [u8; 4] {
    [2, 0, (usage >> 8) as u8, usage as u8]
}

/// Remap targets: (label, code).
pub fn targets() -> Vec<(String, [u8; 4])> {
    let mut v: Vec<(String, [u8; 4])> = vec![("Disabled".into(), [0; 4])];
    for (i, c) in ('A'..='Z').enumerate() {
        v.push((c.to_string(), key_code(0x04 + i as u8, 0)));
    }
    for (i, c) in "1234567890".chars().enumerate() {
        v.push((c.to_string(), key_code(0x1e + i as u8, 0)));
    }
    for i in 0..12u8 {
        v.push((format!("F{}", i + 1), key_code(0x3a + i, 0)));
    }
    let named: &[(&str, u8)] = &[
        ("Enter", 0x28), ("Esc", 0x29), ("Backspace", 0x2a), ("Tab", 0x2b), ("Space", 0x2c), ("-", 0x2d),
        ("=", 0x2e), ("[", 0x2f), ("]", 0x30), ("\\", 0x31), (";", 0x33), ("'", 0x34), ("`", 0x35), (",", 0x36),
        (".", 0x37), ("/", 0x38), ("Caps Lock", 0x39), ("Print Screen", 0x46), ("Scroll Lock", 0x47),
        ("Pause", 0x48), ("Insert", 0x49), ("Home", 0x4a), ("Page Up", 0x4b), ("Delete", 0x4c), ("End", 0x4d),
        ("Page Down", 0x4e), ("Right", 0x4f), ("Left", 0x50), ("Down", 0x51), ("Up", 0x52), ("Menu", 0x65),
    ];
    for (n, u) in named {
        v.push((n.to_string(), key_code(*u, 0)));
    }
    // modifiers are bits in byte 1 (confirmed on hardware: Left Shift = 00 02 00 00, Left Ctrl = 00 01 00 00)
    for (i, n) in ["Left Ctrl", "Left Shift", "Left Alt", "Left Super", "Right Ctrl", "Right Shift", "Right Alt", "Right Super"]
        .iter()
        .enumerate()
    {
        v.push((n.to_string(), key_code(0, 1 << i)));
    }
    let media: &[(&str, u16)] = &[
        ("Play / Pause", 0xcd), ("Next Track", 0xb5), ("Previous Track", 0xb6), ("Stop", 0xb7), ("Mute", 0xe2),
        ("Volume Up", 0xe9), ("Volume Down", 0xea), ("Calculator", 0x192), ("Browser", 0x196), ("Mail", 0x18a),
    ];
    for (n, u) in media {
        v.push((n.to_string(), consumer_code(*u)));
    }
    v
}

pub fn describe_code(c: [u8; 4]) -> String {
    if c == [0; 4] {
        return "Disabled".into();
    }
    if let Some((n, _)) = targets().into_iter().find(|(_, t)| *t == c) {
        return n;
    }
    match c[0] {
        3 => format!("Macro {}", c[3] + 1),
        _ => format!("{:02x} {:02x} {:02x} {:02x}", c[0], c[1], c[2], c[3]),
    }
}

/// Factory code for a key, from its Windows VK in KB.ini.
pub fn default_code(vk: u8) -> Option<[u8; 4]> {
    let usage = match vk {
        0x41..=0x5a => 0x04 + (vk - 0x41),
        0x31..=0x39 => 0x1e + (vk - 0x31),
        0x30 => 0x27,
        0x70..=0x7b => 0x3a + (vk - 0x70),
        0x1b => 0x29,
        0x09 => 0x2b,
        0x14 => 0x39,
        0x0d => 0x28,
        0x08 => 0x2a,
        0x20 => 0x2c,
        0xc0 => 0x35,
        0xbd => 0x2d,
        0xbb => 0x2e,
        0xdb => 0x2f,
        0xdd => 0x30,
        0xdc => 0x31,
        0xba => 0x33,
        0xde => 0x34,
        0xbc => 0x36,
        0xbe => 0x37,
        0xbf => 0x38,
        0x2e => 0x4c,
        0x21 => 0x4b,
        0x22 => 0x4e,
        0x23 => 0x4d,
        0x27 => 0x4f,
        0x25 => 0x50,
        0x28 => 0x51,
        0x26 => 0x52,
        0xa2 => return Some(key_code(0, 0x01)),
        0xa0 => return Some(key_code(0, 0x02)),
        0xa4 => return Some(key_code(0, 0x04)),
        0x5b => return Some(key_code(0, 0x08)),
        0xa3 => return Some(key_code(0, 0x10)),
        0xa1 => return Some(key_code(0, 0x20)),
        _ => return None,
    };
    Some(key_code(usage, 0))
}

/// A base layer we are willing to edit: complete, and most letter/number keys at
/// their factory codes (catches shifted or truncated reads).
pub fn keymap_plausible(layer0: &[u8], keys: &[(u8, u8)]) -> bool {
    if layer0.len() < KEYMAP_LEN || layer0[..4] != key_code(0x29, 0) {
        return false;
    }
    let checks: Vec<bool> = keys
        .iter()
        .filter(|(vk, _)| matches!(vk, 0x30..=0x39 | 0x41..=0x5a))
        .map(|(vk, led)| default_code(*vk).is_some_and(|c| layer0[*led as usize * 4..*led as usize * 4 + 4] == c))
        .collect();
    checks.iter().filter(|c| **c).count() * 10 >= checks.len() * 8
}

/// Factory base layer from the KB.ini layout (matches the F75's 2.4G read key for key).
pub fn factory_layer0() -> Vec<u8> {
    let mut km = vec![0u8; KEYMAP_LEN];
    for k in crate::layout::KEYS {
        let code = if k.vk == 0xfa { Some([0x0d, 0, 0, 0]) } else { default_code(k.vk) }; // Fn = 0d 00 00 00 on hardware
        if let Some(c) = code {
            km[k.led as usize * 4..k.led as usize * 4 + 4].copy_from_slice(&c);
        }
    }
    km
}

pub fn keymap_encoding_ok(layer0: &[u8]) -> bool {
    keymap_plausible(layer0, &crate::layout::KEYS.iter().map(|k| (k.vk, k.led)).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_light_mode_decoding() {
        for (value, mode) in [
            (0, SideLightMode::Off),
            (1, SideLightMode::Fill),
            (2, SideLightMode::Breathing),
            (3, SideLightMode::Static),
        ] {
            assert_eq!(SideLightMode::from_byte(value), Some(mode));
            assert_eq!(mode as u8, value);
        }
        assert_eq!(SideLightMode::from_byte(4), None);
        assert_eq!(SideLightMode::from_byte(255), None);
        assert_eq!(side_light_from_block(&[0; SIDE_LIGHT_OFFSET]), None);

        let mut block = [0; LED_BLOCK_LEN];
        block[SIDE_LIGHT_OFFSET] = 255;
        assert_eq!(side_light_from_block(&block), Some(255));
    }

    #[test]
    fn side_light_edit_preserves_other_settings() {
        let mut original = [0x42; LED_BLOCK_LEN];
        original[10] = 1;
        original[SLEEP_OFFSET] = 10;
        original[126..].copy_from_slice(&[0x5a, 0xa5]);

        for mode in SideLightMode::ALL {
            let mut block = original;
            edit_side_light(&mut block, mode);
            assert_eq!(side_light_from_block(&block), Some(mode as u8));
            assert_eq!(block[..SIDE_LIGHT_OFFSET], original[..SIDE_LIGHT_OFFSET]);
            assert_eq!(
                block[SIDE_LIGHT_OFFSET + 1..],
                original[SIDE_LIGHT_OFFSET + 1..]
            );
            assert!(led_block_valid(&block));

            edit_led_block(&mut block, CUSTOM_MODE, 9, 1, 7);
            assert_eq!(side_light_from_block(&block), Some(mode as u8));
        }
    }

    #[test]
    fn checksum_is_byte_sum() {
        let p = read_request(WIRELESS_ONLINE, 0);
        assert_eq!(&p[..5], &[0x13, 0x07, 0x01, 0x00, 0x00]);
        assert_eq!(p[19], 0x1b);
    }

    #[test]
    fn matrix_write_is_28_packets() {
        let pk = write_packets(WIRELESS.led_matrix, 0, &[0u8; MATRIX_LEN * 3]);
        assert_eq!(pk.len(), 28);
        assert!(pk[..27].iter().all(|p| p[2] == 28 && p[4] == 14));
        assert_eq!((pk[27][3], pk[27][4]), (27, 6));
        assert!(pk.iter().all(|p| p[19] == checksum(p)));
    }

    #[test]
    fn wired_framing() {
        let d = vec![7u8; 600];
        let pk = wired_packets(WIRED.led_matrix, 2, d.len(), Some(&d));
        assert_eq!(pk.len(), 2);
        assert_eq!(&pk[0][..8], &[0x06, 0x06, 0x02, 0x00, 0x02, 0x00, 0x00, 0x02]);
        assert_eq!(&pk[1][..8], &[0x06, 0x06, 0x02, 0x00, 0x02, 0x01, 88, 0x00]);
        let r = wired_packets(WIRED.led | WIRED.read_flag, 0, 128, None);
        assert_eq!(&r[0][..8], &[0x06, 0x84, 0, 0, 1, 0, 128, 0]);
    }

    #[test]
    fn matrix_planes() {
        let mut c = [[0u8; 3]; LED_COUNT];
        c[0] = [10, 20, 30];
        c[125] = [1, 2, 3];
        let b = matrix_payload(&c);
        assert_eq!((b[0], b[126], b[252]), (10, 20, 30));
        assert_eq!((b[125], b[251], b[377]), (1, 2, 3));
    }

    #[test]
    fn effect_roundtrip_against_real_block() {
        // probe output from the user's F75 over 2.4G
        let mut b = vec![0u8; 128];
        b[9] = 1;
        b[10] = 0x15;
        b[0x3a] = 0x09;
        b[0x3b] = 0x42;
        b[126] = 0x5a;
        b[127] = 0xa5;
        assert!(led_block_valid(&b));
        assert_eq!(effect_from_block(&b).map(|e| e.0), Some(18));
        edit_led_block(&mut b, 1, 5, 4, 2);
        assert_eq!((b[9], b[10], b[0x3a], b[0x3b]), (0, 1, 5, 0x42));
    }

    #[test]
    fn factory_layout_matches_hardware() {
        assert!(keymap_encoding_ok(&factory_layer0()));
        // every key of the user's F75 base layer, read over 2.4G
        let hw: &[(usize, [u8; 4])] = &[(0, [0x00, 0x00, 0x00, 0x29]), (12, [0x00, 0x00, 0x00, 0x3a]), (18, [0x00, 0x00, 0x00, 0x3b]), (24, [0x00, 0x00, 0x00, 0x3c]), (30, [0x00, 0x00, 0x00, 0x3d]), (36, [0x00, 0x00, 0x00, 0x3e]), (42, [0x00, 0x00, 0x00, 0x3f]), (48, [0x00, 0x00, 0x00, 0x40]), (54, [0x00, 0x00, 0x00, 0x41]), (60, [0x00, 0x00, 0x00, 0x42]), (66, [0x00, 0x00, 0x00, 0x43]), (72, [0x00, 0x00, 0x00, 0x44]), (78, [0x00, 0x00, 0x00, 0x45]), (88, [0x00, 0x00, 0x00, 0x4d]), (1, [0x00, 0x00, 0x00, 0x35]), (7, [0x00, 0x00, 0x00, 0x1e]), (13, [0x00, 0x00, 0x00, 0x1f]), (19, [0x00, 0x00, 0x00, 0x20]), (25, [0x00, 0x00, 0x00, 0x21]), (31, [0x00, 0x00, 0x00, 0x22]), (37, [0x00, 0x00, 0x00, 0x23]), (43, [0x00, 0x00, 0x00, 0x24]), (49, [0x00, 0x00, 0x00, 0x25]), (55, [0x00, 0x00, 0x00, 0x26]), (61, [0x00, 0x00, 0x00, 0x27]), (67, [0x00, 0x00, 0x00, 0x2d]), (73, [0x00, 0x00, 0x00, 0x2e]), (79, [0x00, 0x00, 0x00, 0x2a]), (85, [0x00, 0x00, 0x00, 0x4c]), (2, [0x00, 0x00, 0x00, 0x2b]), (8, [0x00, 0x00, 0x00, 0x14]), (14, [0x00, 0x00, 0x00, 0x1a]), (20, [0x00, 0x00, 0x00, 0x08]), (26, [0x00, 0x00, 0x00, 0x15]), (32, [0x00, 0x00, 0x00, 0x17]), (38, [0x00, 0x00, 0x00, 0x1c]), (44, [0x00, 0x00, 0x00, 0x18]), (50, [0x00, 0x00, 0x00, 0x0c]), (56, [0x00, 0x00, 0x00, 0x12]), (62, [0x00, 0x00, 0x00, 0x13]), (68, [0x00, 0x00, 0x00, 0x2f]), (74, [0x00, 0x00, 0x00, 0x30]), (80, [0x00, 0x00, 0x00, 0x31]), (86, [0x00, 0x00, 0x00, 0x4b]), (3, [0x00, 0x00, 0x00, 0x39]), (9, [0x00, 0x00, 0x00, 0x04]), (15, [0x00, 0x00, 0x00, 0x16]), (21, [0x00, 0x00, 0x00, 0x07]), (27, [0x00, 0x00, 0x00, 0x09]), (33, [0x00, 0x00, 0x00, 0x0a]), (39, [0x00, 0x00, 0x00, 0x0b]), (45, [0x00, 0x00, 0x00, 0x0d]), (51, [0x00, 0x00, 0x00, 0x0e]), (57, [0x00, 0x00, 0x00, 0x0f]), (63, [0x00, 0x00, 0x00, 0x33]), (69, [0x00, 0x00, 0x00, 0x34]), (81, [0x00, 0x00, 0x00, 0x28]), (4, [0x00, 0x02, 0x00, 0x00]), (10, [0x00, 0x00, 0x00, 0x1d]), (16, [0x00, 0x00, 0x00, 0x1b]), (22, [0x00, 0x00, 0x00, 0x06]), (28, [0x00, 0x00, 0x00, 0x19]), (34, [0x00, 0x00, 0x00, 0x05]), (40, [0x00, 0x00, 0x00, 0x11]), (46, [0x00, 0x00, 0x00, 0x10]), (52, [0x00, 0x00, 0x00, 0x36]), (58, [0x00, 0x00, 0x00, 0x37]), (64, [0x00, 0x00, 0x00, 0x38]), (70, [0x00, 0x20, 0x00, 0x00]), (82, [0x00, 0x00, 0x00, 0x52]), (5, [0x00, 0x01, 0x00, 0x00]), (11, [0x00, 0x08, 0x00, 0x00]), (17, [0x00, 0x04, 0x00, 0x00]), (35, [0x00, 0x00, 0x00, 0x2c]), (59, [0x00, 0x10, 0x00, 0x00]), (53, [0x0d, 0x00, 0x00, 0x00]), (77, [0x00, 0x00, 0x00, 0x50]), (83, [0x00, 0x00, 0x00, 0x51]), (89, [0x00, 0x00, 0x00, 0x4f]), (87, [0x00, 0x00, 0x00, 0x4e])];
        let f = factory_layer0();
        assert_eq!(hw.len(), 80);
        for (led, code) in hw {
            assert_eq!(&f[led * 4..led * 4 + 4], code, "matrix {led}");
        }
    }

    #[test]
    fn keymap_sanity() {
        let mut km = vec![0u8; KEYMAP_LEN];
        for k in crate::layout::KEYS {
            if let Some(c) = default_code(k.vk) {
                km[k.led as usize * 4..k.led as usize * 4 + 4].copy_from_slice(&c);
            }
        }
        assert!(keymap_encoding_ok(&km));
        // the user's first 2.4G read lost packet 3 (bytes 42..56) and shifted the rest
        let mut shifted = km[..42].to_vec();
        shifted.extend_from_slice(&km[56..]);
        shifted.resize(KEYMAP_LEN, 0);
        assert!(!keymap_encoding_ok(&shifted));
        assert_eq!(&shifted[44..48], &[0, 0x1a, 0, 0]); // matches the hardware dump ("Win 00 1a 00 00")
    }

    #[test]
    fn reply_parsing() {
        let mut r = [0u8; 20];
        r[0] = 0x13;
        r[1] = 0x44;
        r[2] = 10;
        r[3] = 2;
        r[4] = 0x0e;
        let p = parse_reply(&r).unwrap();
        assert_eq!((p.cmd, p.total, p.index, p.len, p.crc_error), (0x44, 10, 2, 14, false));
        r[0] = 0x04;
        assert!(parse_reply(&r).is_none());
    }
}
