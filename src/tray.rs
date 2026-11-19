//! System tray icon (freedesktop StatusNotifierItem via ksni). Works out of the box on KDE; GNOME
//! needs the "AppIndicator and KStatusNotifierItem Support" extension (Fedora:
//! gnome-shell-extension-appindicator). Without a tray host this returns None.
use crate::fx;
use crate::state::Profile;
use crate::worker::{Core, Job};
use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::MenuItem;
use std::sync::Arc;

struct Tray {
    core: Arc<Core>,
    icon: Vec<ksni::Icon>,
}

fn radio(labels: impl IntoIterator<Item = String>, enabled: bool) -> Vec<RadioItem> {
    labels.into_iter().map(|label| RadioItem { label, enabled, ..Default::default() }).collect()
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "f75-studio".into()
    }
    fn title(&self) -> String {
        "F75 Studio".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icon.clone()
    }
    fn activate(&mut self, _x: i32, _y: i32) {
        self.core.request_toggle();
    }
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let visible = self.core.window_visible();
        let live = *self.core.live.lock().unwrap();
        let wired = self.core.wired();
        let usb = if wired { "" } else { " (USB-C only)" };
        let profiles = Profile::list();
        let active = Profile::active().name;
        let fx_sel = live.fx.and_then(|f| fx::STUDIO.iter().position(|x| x.0 == f)).map(|i| i + 1).unwrap_or(0);
        vec![
            // Keep distinct D-Bus menu IDs so a cached click can't run the opposite action.
            StandardItem {
                label: "Open F75 Studio".into(),
                visible: !visible,
                activate: Box::new(|t: &mut Self| t.core.request_show()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Hide F75 Studio".into(),
                visible,
                activate: Box::new(|t: &mut Self| t.core.request_hide()),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            SubMenu {
                label: "Profile".into(),
                submenu: vec![RadioGroup {
                    selected: profiles.iter().position(|n| *n == active).unwrap_or(0),
                    select: Box::new(|t: &mut Self, i| {
                        if let Some(pr) = Profile::list().get(i).and_then(|n| Profile::load(n)) {
                            pr.save(); // marks it active
                            t.core.stop_live();
                            {
                                let mut l = t.core.live.lock().unwrap();
                                l.palette = pr.palette;
                                l.brightness = pr.brightness;
                            }
                            t.core.send(Job::ApplyProfile(Box::new(pr)));
                            t.core.profile_changed.store(true, std::sync::atomic::Ordering::SeqCst);
                            t.core.repaint();
                        }
                    }),
                    options: radio(profiles.clone(), true),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: format!("Audio sync{usb}"),
                checked: live.audio,
                enabled: wired,
                activate: Box::new(|t: &mut Self| {
                    let on = !t.core.live.lock().unwrap().audio;
                    if on {
                        let mut l = t.core.live.lock().unwrap();
                        l.fx = None;
                        l.audio = true;
                    } else {
                        t.core.stop_live();
                    }
                    t.core.repaint();
                }),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Audio effect".into(),
                enabled: wired,
                submenu: vec![RadioGroup {
                    selected: fx::AUDIO.iter().position(|f| f.0 == live.audio_fx).unwrap_or(0),
                    select: Box::new(|t: &mut Self, i| {
                        t.core.live.lock().unwrap().audio_fx = fx::AUDIO[i].0;
                        t.core.repaint();
                    }),
                    options: radio(fx::AUDIO.iter().map(|f| f.1.to_string()), wired),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: format!("Studio FX{usb}"),
                enabled: wired,
                submenu: vec![RadioGroup {
                    selected: fx_sel,
                    select: Box::new(|t: &mut Self, i| {
                        if i == 0 {
                            t.core.stop_live();
                        } else {
                            let mut l = t.core.live.lock().unwrap();
                            l.audio = false;
                            l.fx = Some(fx::STUDIO[i - 1].0);
                        }
                        t.core.repaint();
                    }),
                    options: radio(std::iter::once("Off".to_string()).chain(fx::STUDIO.iter().map(|f| f.1.to_string())), wired),
                }
                .into()],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| t.core.request_quit()), ..Default::default() }.into(),
        ]
    }
}

pub fn spawn(core: Arc<Core>) -> bool {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
        .map(|d| {
            let data = d.rgba.chunks_exact(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            vec![ksni::Icon { width: d.width as i32, height: d.height as i32, data }]
        })
        .unwrap_or_default();
    match (Tray { core: core.clone(), icon }).spawn() {
        Ok(handle) => {
            *core.tray_refresh.lock().unwrap() = Some(Box::new(move || {
                handle.update(|_| {});
            }));
            true
        }
        Err(_) => false,
    }
}

