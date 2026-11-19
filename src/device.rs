use crate::protocol::{self as p, Cmds, Link, PACKET_LEN, WIRED_LEN};
use anyhow::{anyhow, bail, Context, Result};
use hidapi::{HidApi, HidDevice};
use std::thread::sleep;
use std::time::{Duration, Instant};

const RECEIVER: (u16, u16) = (0x3554, 0xfa09);
const BLUETOOTH: (u16, u16) = (0x3554, 0xfa08);
const WIRED: (u16, u16) = (0x258a, 0x010c);

/// AULA's driver only configures the F75 over the cable (and the receiver); over Bluetooth the
/// keyboard returns one packet of each reply and config traffic breaks typing, so we never send.
pub const BT_UNSUPPORTED: &str = "Bluetooth can't be configured - switch the F75 to the 2.4G receiver or USB-C to change settings. \
They are stored on the keyboard and stay active over Bluetooth.";

pub struct F75 {
    dev: Option<HidDevice>, // None = dry run
    simulated_side_light: p::SideLightMode,
    pub link: Link,
    pub cmds: Cmds,
    pub log: Vec<String>,
    pub path: String,
    /// Raw packet trace for `f75-studio doctor` (None = off).
    pub trace: Option<Vec<String>>,
}

impl F75 {
    /// Connected F75 interfaces in preference order (cable, receiver, Bluetooth), without opening them.
    pub fn detect() -> Vec<(Link, String)> {
        let Ok(api) = HidApi::new() else { return vec![] };
        let list: Vec<_> = api.device_list().collect();
        [(WIRED, Link::Wired), (RECEIVER, Link::Receiver), (BLUETOOTH, Link::Bluetooth)]
            .into_iter()
            .filter_map(|((v, pid), link)| {
                list.iter()
                    .find(|d| {
                        d.vendor_id() == v
                            && d.product_id() == pid
                            && if link == Link::Wired { d.interface_number() == 1 } else { d.usage_page() == 0xff02 }
                    })
                    .map(|d| (link, d.path().to_string_lossy().into_owned()))
            })
            .collect()
    }

    /// Opens the preferred connection: cable, then 2.4G receiver, then Bluetooth (view only).
    pub fn open() -> Result<Self> {
        let (link, path) = Self::detect()
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("AULA F75 not found (cable, 2.4G receiver or Bluetooth)"))?;
        let api = HidApi::new().context("hidapi init")?;
        let dev = api
            .open_path(&std::ffi::CString::new(path.clone())?)
            .with_context(|| format!("cannot open {path} ({}) - click Fix permissions in the app or run `f75-studio fix-permissions`", link.name()))?;
        Ok(Self {
            dev: Some(dev),
            simulated_side_light: p::SideLightMode::Off,
            link,
            cmds: p::cmds(link),
            log: vec![format!("{} via {path}", link.name())],
            path,
            trace: None,
        })
    }

    pub fn dry_run(link: Link) -> Self {
        Self {
            dev: None,
            simulated_side_light: p::SideLightMode::Off,
            link,
            cmds: p::cmds(link),
            log: vec![format!("dry run ({})", link.name())],
            path: String::new(),
            trace: None,
        }
    }

    pub fn dry(&self) -> bool {
        self.dev.is_none()
    }

    fn tr(&mut self, line: String) {
        if let Some(t) = &mut self.trace {
            if t.len() < 600 {
                t.push(line);
            }
        }
    }

    // ------------------------------------------------------------ 2.4G receiver (CDev3632)

    fn send20(&mut self, pk: &[u8; PACKET_LEN]) -> Result<()> {
        self.tr(format!("> {}", hex(pk)));
        match &self.dev {
            Some(d) => {
                d.write(pk).context("hid write")?;
            }
            None => println!("> {}", hex(pk)),
        }
        Ok(())
    }

    /// Next report-0x13 packet; keyboard/mouse reports on the same interface are skipped.
    fn recv20(&mut self, timeout_ms: u64) -> Result<Option<([u8; PACKET_LEN], p::Reply)>> {
        if self.dev.is_none() {
            return Ok(None);
        }
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut buf = [0u8; 64];
        while Instant::now() < deadline {
            let left = deadline.saturating_duration_since(Instant::now()).as_millis().max(1) as i32;
            let n = self.dev.as_ref().unwrap().read_timeout(&mut buf, left).context("hid read")?;
            if n > 0 {
                self.tr(format!("< {}", hex(&buf[..n])));
            }
            if let Some(r) = p::parse_reply(&buf[..n]) {
                let mut pk = [0u8; PACKET_LEN];
                pk.copy_from_slice(&buf[..PACKET_LEN]);
                return Ok(Some((pk, r)));
            }
        }
        Ok(None)
    }

    /// AccessData (0x004862f0): per-packet delay, then wait for the receiver's ack (retry ×5).
    /// Commands with bit 7 set (live frames) are not acknowledged.
    fn wireless_write(&mut self, cmd: u8, sub: u8, data: &[u8], delay_ms: u64) -> Result<()> {
        let wants_ack = cmd & 0x80 == 0 && !self.dry();
        for (i, pk) in p::write_packets(cmd, sub, data).iter().enumerate() {
            let mut tries = 0;
            loop {
                sleep(Duration::from_millis(delay_ms));
                self.send20(pk)?;
                if !wants_ack {
                    break;
                }
                let mut ack = None;
                for _ in 0..20 {
                    match self.recv20(1000)? {
                        Some((raw, r)) if raw[1] == cmd => {
                            ack = Some(r);
                            break;
                        }
                        Some(_) => continue,
                        None => break,
                    }
                }
                match ack {
                    Some(r) if !r.crc_error && r.index as usize == i => break,
                    Some(r) if !r.crc_error => bail!("cmd 0x{cmd:02x}: ack index {} for packet {i}", r.index),
                    _ => {}
                }
                tries += 1;
                if tries > 4 {
                    bail!("no reply from keyboard (asleep or out of range?) - press a key and retry");
                }
            }
        }
        Ok(())
    }

    /// Multi-packet read. Chunks are placed by packet index and merged across retries (the radio
    /// occasionally drops one); a read with gaps is never returned.
    fn wireless_read(&mut self, cmd: u8, sub: u8, len: usize) -> Result<Vec<u8>> {
        if self.dry() {
            self.send20(&p::read_request(cmd, sub))?;
            return Ok(vec![0; len]);
        }
        let mut out = vec![0u8; len.max(p::CHUNK)];
        let mut got: Vec<bool> = vec![];
        let mut total = 0usize;
        for attempt in 0..4 {
            if attempt > 0 {
                sleep(Duration::from_millis(60));
            }
            self.send20(&p::read_request(cmd, sub))?;
            while let Some((pk, r)) = self.recv20(500)? {
                if r.cmd != cmd & 0x7f {
                    continue;
                }
                if r.crc_error {
                    break;
                }
                if total == 0 {
                    total = r.total.max(1) as usize;
                    got = vec![false; total];
                }
                let i = r.index as usize;
                if i >= total || r.total as usize != total {
                    continue;
                }
                let off = i * p::CHUNK;
                if out.len() < off + r.len {
                    out.resize(off + r.len, 0);
                }
                out[off..off + r.len].copy_from_slice(&pk[5..5 + r.len]);
                got[i] = true;
                if got.iter().all(|g| *g) {
                    out.resize(len, 0);
                    return Ok(out);
                }
            }
        }
        bail!("no complete reply to 0x{cmd:02x} - keyboard asleep or out of range? press a key and retry")
    }

    // ------------------------------------------------------------ USB-C cable (CDevG5KB)

    fn wired_send(&mut self, pk: &[u8; WIRED_LEN]) -> Result<()> {
        self.tr(format!("> {} ... ({WIRED_LEN} bytes)", hex(&pk[..24])));
        let Some(d) = &self.dev else {
            println!("> {} ... ({WIRED_LEN} bytes)", hex(&pk[..24]));
            return Ok(());
        };
        let mut err = None;
        for _ in 0..3 {
            match d.send_feature_report(pk) {
                Ok(()) => return Ok(()),
                Err(e) => err = Some(e),
            }
            sleep(Duration::from_millis(70));
        }
        Err(anyhow!("feature report failed: {}", err.unwrap()))
    }

    fn wired_write(&mut self, cmd: u8, sub: u8, data: &[u8], delay_ms: u64) -> Result<()> {
        for pk in p::wired_packets(cmd, sub, data.len(), Some(data)) {
            sleep(Duration::from_millis(delay_ms));
            self.wired_send(&pk)?;
        }
        Ok(())
    }

    /// Header via SetFeature, wait 20 ms, GetFeature returns the payload at byte 8 (0x0048bc00).
    fn wired_read(&mut self, cmd: u8, sub: u8, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![];
        for pk in p::wired_packets(cmd, sub, len, None) {
            let size = pk[6] as usize | (pk[7] as usize) << 8;
            sleep(Duration::from_millis(20));
            self.wired_send(&pk)?;
            sleep(Duration::from_millis(20));
            let Some(d) = &self.dev else {
                out.extend(vec![0; size]);
                continue;
            };
            let mut buf = [0u8; WIRED_LEN];
            buf[0] = p::WIRED_REPORT;
            let n = d.get_feature_report(&mut buf).context("get feature report")?;
            self.tr(format!("< {} ... ({n} bytes)", hex(&buf[..24.min(n)])));
            if n < 8 + size {
                bail!("short wired reply ({n} bytes)");
            }
            out.extend_from_slice(&buf[8..8 + size]);
        }
        Ok(out)
    }

    // ------------------------------------------------------------ shared

    pub fn write(&mut self, cmd: u8, sub: u8, data: &[u8], delay_ms: u64) -> Result<()> {
        if self.link == Link::Bluetooth && !self.dry() {
            bail!("{BT_UNSUPPORTED}");
        }
        let t0 = Instant::now();
        if self.link == Link::Wired {
            self.wired_write(cmd, sub, data, delay_ms.min(20))?;
        } else {
            self.wireless_write(cmd, sub, data, delay_ms)?;
        }
        self.log.push(format!("write 0x{cmd:02x}/{sub}: {} bytes, {} ms", data.len(), t0.elapsed().as_millis()));
        Ok(())
    }

    pub fn read(&mut self, cmd: u8, sub: u8, len: usize) -> Result<Vec<u8>> {
        if self.link == Link::Bluetooth && !self.dry() {
            bail!("{BT_UNSUPPORTED}");
        }
        let rc = cmd | self.cmds.read_flag;
        let out = if self.link == Link::Wired { self.wired_read(rc, sub, len)? } else { self.wireless_read(rc, sub, len)? };
        self.log.push(format!("read 0x{rc:02x}/{sub}: {} bytes", out.len()));
        Ok(out)
    }

    /// 2.4G: is the keyboard linked to the receiver (m_bOnline). Cable/Bluetooth: always true.
    pub fn online(&mut self) -> Result<bool> {
        if self.link != Link::Receiver {
            return Ok(true);
        }
        let reply = self.wireless_read(p::WIRELESS_ONLINE, 0, 1)?;
        Ok(self.dry() || reply[0] != 0)
    }

    pub fn get_led_block(&mut self) -> Result<Vec<u8>> {
        let led = self.cmds.led;
        let mut b = self.read(led, 0, p::LED_BLOCK_LEN)?;
        if self.dry() {
            p::edit_side_light(&mut b, self.simulated_side_light);
        } else if !p::led_block_valid(&b) {
            bail!("LED block signature 5A A5 missing - refusing to write");
        }
        Ok(b)
    }

    /// Read-modify-write of the 128-byte LED block, like OemDrv.
    fn edit_block(&mut self, f: impl FnOnce(&mut Vec<u8>)) -> Result<()> {
        let mut b = self.get_led_block()?;
        f(&mut b);
        let led = self.cmds.led;
        self.write(led, 0, &b, 15)
    }

    pub fn set_effect(&mut self, mode: u8, brightness: u8, speed: u8, colour: u8) -> Result<()> {
        self.edit_block(|b| p::edit_led_block(b, mode, brightness, speed, colour))
    }

    pub fn set_side_light(&mut self, mode: p::SideLightMode) -> Result<()> {
        self.edit_block(|b| p::edit_side_light(b, mode))?;
        if self.dry() {
            self.simulated_side_light = mode;
        }
        Ok(())
    }

    /// Seconds from protocol::SLEEP (0 = never).
    pub fn set_sleep(&mut self, seconds: u16) -> Result<()> {
        let hw = p::SLEEP.iter().find(|s| s.0 == seconds).map(|s| s.1).unwrap_or(2);
        self.edit_block(|b| b[p::SLEEP_OFFSET] = hw)
    }

    pub fn set_per_key(&mut self, colours: &[[u8; 3]; p::LED_COUNT], brightness: u8) -> Result<()> {
        let m = self.cmds.led_matrix;
        self.write(m, 0, &p::matrix_payload(colours), 15)?;
        self.set_effect(p::CUSTOM_MODE, brightness, 0, 0)
    }

    /// Live frame, not acknowledged. Vendor pacing: 10 ms per packet on 2.4G (0x00487330), 7 ms wired (0x0048c6e0).
    pub fn realtime(&mut self, colours: &[[u8; 3]; p::LED_COUNT]) -> Result<()> {
        let (rt, delay) = (self.cmds.realtime, if self.link == Link::Wired { 7 } else { 10 });
        self.write(rt, 0, &p::realtime_payload(colours), delay)
    }

    pub fn get_keymap(&mut self, layer: u8) -> Result<Vec<u8>> {
        let k = self.cmds.keymap;
        self.read(k, layer, p::KEYMAP_LEN)
    }

    pub fn set_keymap(&mut self, layer: u8, data: &[u8]) -> Result<()> {
        let k = self.cmds.keymap;
        self.write(k, layer, data, 15)
    }

    /// Macro area (see macros.rs). 2.4G: 512-byte pages, sub = page, 100 ms apart (0x004868a0);
    /// cable: one transfer (0x0048b960). OemDrv waits before it and writes the key matrix first.
    pub fn set_macros(&mut self, buf: &[u8]) -> Result<()> {
        let m = self.cmds.macros;
        sleep(Duration::from_millis(100));
        if self.link == Link::Wired {
            return self.write(m, 0, buf, 20);
        }
        for (page, chunk) in buf.chunks(0x200).enumerate() {
            self.write(m, page as u8, chunk, 15)?;
            sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    /// Reads back the start of the macro area to verify a write.
    pub fn get_macros(&mut self, len: usize) -> Result<Vec<u8>> {
        let m = self.cmds.macros;
        self.read(m, 0, len.clamp(1, 0x200))
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_side_light_round_trips_over_both_links() -> Result<()> {
        for link in [Link::Wired, Link::Receiver] {
            let mut keyboard = F75::dry_run(link);
            assert!(keyboard.online()?);
            assert_eq!(
                p::side_light_from_block(&keyboard.get_led_block()?),
                Some(0)
            );
            for mode in p::SideLightMode::ALL {
                keyboard.set_side_light(mode)?;
                assert_eq!(
                    p::side_light_from_block(&keyboard.get_led_block()?),
                    Some(mode as u8)
                );
            }
        }
        Ok(())
    }
}
