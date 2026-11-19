//! Device-wide settings stored in the LED block, separate from local profiles.
use crate::protocol as p;
use anyhow::{Result, ensure};

pub const DEBOUNCE_OFFSET: usize = 0x01;
pub const WIN_LOCK_OFFSET: usize = 0x0f;
pub const OS_MODE_OFFSET: usize = 0x1b;
pub const MAX_DEBOUNCE_MS: u8 = 8;
pub const OS_MODES: [&str; 3] = ["Linux", "Android", "Mac"];

#[derive(Clone, Copy, Default)]
pub struct Settings {
    pub win_lock: Option<bool>,
    pub sleep: Option<u16>,
    pub debounce: Option<u8>,
    pub brightness: Option<u8>,
    pub os_mode: Option<u8>,
}

fn brightness_offset(block: &[u8]) -> Option<usize> {
    let mode = block[10];
    (mode != 0)
        .then(|| p::EFFECTS.iter().position(|effect| effect.0 == mode))
        .flatten()
        .map(|slot| 0x3a + 2 * slot)
}

impl Settings {
    pub fn read(block: &[u8]) -> Option<Self> {
        if !p::led_block_valid(block) {
            return None;
        }
        Some(Self {
            win_lock: match block[WIN_LOCK_OFFSET] {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            },
            sleep: p::SLEEP.iter().find_map(|&(seconds, encoded)| {
                (encoded == block[p::SLEEP_OFFSET]).then_some(seconds)
            }),
            debounce: Some(block[DEBOUNCE_OFFSET]).filter(|ms| *ms <= MAX_DEBOUNCE_MS),
            brightness: brightness_offset(block)
                .map(|offset| block[offset])
                .filter(|value| *value <= 9),
            os_mode: Some(block[OS_MODE_OFFSET]).filter(|mode| usize::from(*mode) < OS_MODES.len()),
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Change {
    WinLock(bool),
    Sleep(u16),
    Debounce(u8),
    Brightness(u8),
    OsMode(u8),
}

impl Change {
    pub fn apply(self, block: &mut [u8]) -> Result<()> {
        ensure!(p::led_block_valid(block), "Invalid LED settings block");
        let (offset, value) = match self {
            Self::WinLock(locked) => (WIN_LOCK_OFFSET, u8::from(locked)),
            Self::Sleep(seconds) => {
                let value = p::SLEEP
                    .iter()
                    .find_map(|&(s, value)| (s == seconds).then_some(value))
                    .ok_or_else(|| anyhow::anyhow!("Unsupported sleep timeout"))?;
                (p::SLEEP_OFFSET, value)
            }
            Self::Debounce(ms) => {
                ensure!(ms <= MAX_DEBOUNCE_MS, "Unsupported debounce interval");
                (DEBOUNCE_OFFSET, ms)
            }
            Self::Brightness(value) => {
                ensure!(value <= 9, "Unsupported brightness");
                let offset = brightness_offset(block).ok_or_else(|| {
                    anyhow::anyhow!("No active effect with adjustable brightness")
                })?;
                (offset, value)
            }
            Self::OsMode(mode) => {
                ensure!(usize::from(mode) < OS_MODES.len(), "Unknown OS mode");
                (OS_MODE_OFFSET, mode)
            }
        };
        block[offset] = value;
        Ok(())
    }
}

/// A reset is only valid for the same, still-online receiver connection.
pub fn reset_allowed(link: Option<p::Link>, online: bool) -> bool {
    link == Some(p::Link::Receiver) && online
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block() -> [u8; p::LED_BLOCK_LEN] {
        let mut block = [0x42; p::LED_BLOCK_LEN];
        block[10] = 2;
        block[126..].copy_from_slice(&[0x5a, 0xa5]);
        block
    }

    #[test]
    fn each_setting_changes_only_its_byte() -> Result<()> {
        use Change::*;
        let original = block();
        for (change, offset, value) in [
            (WinLock(false), 0x0f, 0),
            (WinLock(true), 0x0f, 1),
            (Debounce(0), 0x01, 0),
            (Debounce(8), 0x01, 8),
            (Brightness(0), 0x3c, 0),
            (Brightness(9), 0x3c, 9),
            (OsMode(0), 0x1b, 0),
            (OsMode(1), 0x1b, 1),
            (OsMode(2), 0x1b, 2),
        ] {
            let mut expected = original;
            expected[offset] = value;
            let mut updated = original;
            change.apply(&mut updated)?;
            assert_eq!(updated, expected);
        }
        for &(seconds, encoded) in p::SLEEP {
            let mut updated = original;
            Sleep(seconds).apply(&mut updated)?;
            let mut expected = original;
            expected[0x18] = encoded;
            assert_eq!(updated, expected);
            assert_eq!(Settings::read(&updated).unwrap().sleep, Some(seconds));
        }
        Ok(())
    }

    #[test]
    fn invalid_or_unknown_settings_are_not_replaced_by_defaults() {
        let mut block = block();
        let decoded = Settings::read(&block).unwrap();
        assert!(decoded.win_lock.is_none());
        assert!(decoded.os_mode.is_none());
        assert!(decoded.debounce.is_none());
        assert!(decoded.sleep.is_none());
        assert!(decoded.brightness.is_none());
        assert!(Settings::read(&block[..10]).is_none());
        for change in [
            Change::Sleep(61),
            Change::Brightness(10),
            Change::Debounce(9),
            Change::OsMode(3),
        ] {
            assert!(change.apply(&mut block).is_err());
        }
        block[10] = 0;
        assert!(Change::Brightness(0).apply(&mut block).is_err());
        assert!(Change::WinLock(true).apply(&mut block[..126]).is_err());
    }

    #[test]
    fn reset_requires_online_receiver() {
        use p::Link::*;
        assert!(reset_allowed(Some(Receiver), true));
        for link in [None, Some(Wired), Some(Bluetooth)] {
            assert!(!reset_allowed(link, true));
        }
        assert!(!reset_allowed(Some(Receiver), false));
    }
}
