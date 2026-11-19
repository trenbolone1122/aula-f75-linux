mod audio;
mod aula_music;
mod device;
mod doctor;
mod fx;
mod instance;
mod kbview;
mod layout;
mod macros;
mod perms;
mod protocol;
mod state;
mod tray;
mod ui;
mod window_state;
mod worker;

use anyhow::{bail, Result};
use device::F75;
use protocol::Link;

const USAGE: &str = "f75-studio                         open the app
f75-studio probe                   read only: link, online state, LED block
f75-studio keymap [LAYER]          read only: dump key assignments (0 = base, 1 = Fn)
f75-studio solid R G B             every key one colour (writes)
f75-studio key LED R G B           one LED lit, rest off (writes)
f75-studio effect ID [BRIGHT 0-9] [SPEED 0-4] [COLOUR 0-7]
f75-studio sleep SECONDS           0 = never, or 60 90 120 180 240 300 600 900 1200
f75-studio live [SECONDS]          stream a rainbow wave (real-time frames) and report fps
f75-studio macro KEY TEXT          assign a text macro to a key label (e.g. F12), then verify
f75-studio macro-dump              read back the macro area
f75-studio doctor                  test lighting + remap on the current connection, save a report
f75-studio fix-permissions         install the udev rule (one password prompt)
f75-studio quit                    stop the app running in the background / tray
add --dry-run [--wired] to print packets instead of sending them";

fn hue(h: f32) -> [u8; 3] {
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        ((1.0 - (k.min(4.0 - k).clamp(0.0, 1.0))) * 255.0) as u8
    };
    [f(5.0), f(3.0), f(1.0)]
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let dry = args.iter().any(|a| a == "--dry-run");
    let wired = args.iter().any(|a| a == "--wired");
    args.retain(|a| a != "--dry-run" && a != "--wired");
    if args.is_empty() {
        return ui::run();
    }
    if args[0] == "quit" {
        println!("{}", if instance::signal("quit") { "F75 Studio stopped" } else { "F75 Studio isn't running" });
        return Ok(());
    }
    if args[0] != "fix-permissions" && !dry && instance::signal("ping") {
        bail!("F75 Studio is running (window or tray) and owns the keyboard - run `f75-studio quit` first");
    }
    // The Enter that started us may still be "down" on a wireless link; let its release arrive
    // before we occupy the radio, or Linux auto-repeats it.
    std::thread::sleep(std::time::Duration::from_millis(600));
    if args[0] == "doctor" {
        let link = if wired { Link::Wired } else { Link::Receiver };
        return doctor::run(dry.then_some(link));
    }
    if args[0] == "fix-permissions" {
        perms::fix()?;
        println!("udev rule installed and applied - cable, receiver and Bluetooth are accessible now");
        return Ok(());
    }
    if !["probe", "keymap", "solid", "key", "effect", "sleep", "live", "macro", "macro-dump"].contains(&args[0].as_str()) {
        println!("{USAGE}");
        return Ok(());
    }
    let num = |i: usize, d: u16| -> Result<u16> { Ok(args.get(i).map(|s| s.parse()).transpose()?.unwrap_or(d)) };
    let mut kb = if dry { F75::dry_run(if wired { Link::Wired } else { Link::Receiver }) } else { F75::open()? };
    match args[0].as_str() {
        "probe" => {
            println!("link: {}", kb.link.name());
            println!("online: {}", kb.online()?);
            let b = kb.get_led_block()?;
            println!("LED block ({} bytes, signature ok: {}):", b.len(), protocol::led_block_valid(&b));
            for (i, row) in b.chunks(16).enumerate() {
                println!("  {:02x}: {}", i * 16, device::hex(row));
            }
            println!("mode {} · sleep byte {}", b[10], b[protocol::SLEEP_OFFSET]);
        }
        "keymap" => {
            let layer = num(1, 0)? as u8;
            let km = kb.get_keymap(layer)?;
            println!("layer {layer}, encoding ok: {}", protocol::keymap_encoding_ok(&km));
            for k in layout::KEYS {
                let i = k.led as usize * 4;
                let c = [km[i], km[i + 1], km[i + 2], km[i + 3]];
                println!("  {:>6} matrix {:>3}: {}  {}", k.label, k.led, device::hex(&c), protocol::describe_code(c));
            }
        }
        "solid" => {
            let c = [num(1, 255)? as u8, num(2, 0)? as u8, num(3, 0)? as u8];
            kb.set_per_key(&[c; protocol::LED_COUNT], 9)?;
        }
        "key" => {
            let led = num(1, 0)? as usize;
            if led >= protocol::LED_COUNT {
                bail!("LED index must be < {}", protocol::LED_COUNT);
            }
            let mut c = [[0u8; 3]; protocol::LED_COUNT];
            c[led] = [num(2, 255)? as u8, num(3, 0)? as u8, num(4, 0)? as u8];
            kb.set_per_key(&c, 9)?;
        }
        "effect" => kb.set_effect(num(1, 1)? as u8, num(2, 9)? as u8, num(3, 2)? as u8, num(4, 7)? as u8)?,
        "sleep" => kb.set_sleep(num(1, 300)?)?,
        "macro" => {
            let (Some(label), Some(text)) = (args.get(1), args.get(2)) else { bail!("usage: f75-studio macro KEY \"TEXT\"") };
            if kb.link != Link::Wired {
                bail!("macros need the USB-C cable (the 2.4G receiver doesn't apply them)");
            }
            let key = layout::KEYS.iter().find(|k| k.label.eq_ignore_ascii_case(label)).ok_or_else(|| anyhow::anyhow!("no key labelled {label}"))?;
            let mut km = kb.get_keymap(0)?;
            if !dry && !protocol::keymap_encoding_ok(&km) {
                bail!("keymap read looks wrong - refusing to write");
            }
            let i = key.led as usize * 4;
            km[i..i + 4].copy_from_slice(&macros::key_code(0, 1));
            let buf = macros::buffer(&[macros::Macro { name: "cli".into(), events: macros::text_events(text, 12) }]);
            kb.set_keymap(0, &km)?;
            kb.set_macros(&buf)?;
            let back = kb.get_macros(buf.len())?;
            println!("wrote macro to {} (matrix {}); read back {}", key.label, key.led,
                if back.get(..buf.len()) == Some(&buf[..]) { "MATCHES" } else { "DIFFERS" });
            println!("  sent: {}", device::hex(&buf));
            println!("  read: {}", device::hex(&back[..back.len().min(buf.len() + 8)]));
        }
        "macro-dump" => {
            let b = kb.get_macros(0x200)?;
            for (i, row) in b.chunks(16).take(8).enumerate() {
                println!("  {:03x}: {}", i * 16, device::hex(row));
            }
        }
        "live" => {
            let secs = num(1, 5)? as f32;
            let t0 = std::time::Instant::now();
            let mut frames = 0u32;
            while t0.elapsed().as_secs_f32() < secs {
                let t = t0.elapsed().as_secs_f32();
                let mut c = [[0u8; 3]; protocol::LED_COUNT];
                for k in layout::KEYS {
                    let h = ((k.rect[0] / layout::CANVAS[0]) * 1.4 - t * 0.4).rem_euclid(1.0);
                    c[k.led as usize] = hue(h);
                }
                kb.realtime(&c)?;
                frames += 1;
            }
            kb.log.clear();
            println!("{frames} frames in {secs:.0} s = {:.1} fps over {}", frames as f32 / secs, kb.link.name());
        }
        _ => {
            println!("{USAGE}");
            return Ok(());
        }
    }
    for l in &kb.log {
        eprintln!("# {l}");
    }
    Ok(())
}
