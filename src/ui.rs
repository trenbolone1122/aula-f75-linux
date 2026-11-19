use crate::audio::BANDS;
use crate::fx;
use crate::kbview;
use crate::layout::{CANVAS, KEYS};
use crate::macros;
use crate::protocol::{self as p, Link, EFFECTS, LED_COUNT, PALETTE, RANDOM_COLOUR};
use crate::state::{MacroDef, Profile};
use crate::worker::{fx_by_name, macro_buffer, mode_index, sleep_label, Colours, Core, Event, Job, Live};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, FontFamily, FontId, Layout, Margin, Rect, RichText, Sense, Stroke,
    StrokeKind, Ui,
};
use egui_phosphor::regular as icon;
use std::collections::HashMap;
use std::time::Duration;

const BG: Color32 = Color32::from_rgb(7, 9, 8);
const PANEL: Color32 = Color32::from_rgb(12, 15, 13);
const CARD: Color32 = Color32::from_rgb(18, 22, 19);
const LINE: Color32 = Color32::from_rgb(33, 41, 35);
const ACC: Color32 = Color32::from_rgb(57, 255, 106);
const ACC_BG: Color32 = Color32::from_rgb(14, 42, 22);
const DIM: Color32 = Color32::from_rgb(118, 132, 122);
const TEXT: Color32 = Color32::from_rgb(228, 238, 231);
const RED: Color32 = Color32::from_rgb(255, 82, 82);
const AMBER: Color32 = Color32::from_rgb(255, 196, 60);

fn display() -> FontFamily {
    FontFamily::Name("display".into())
}

// ------------------------------------------------------------------ app

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Lighting,
    Keys,
    Macros,
    Audio,
    Power,
}

pub struct App {
    s: Profile,
    profiles: Vec<String>,
    rename: String,
    tab: Tab,
    mode: usize,
    lv: Live,
    core: Arc<Core>,
    view3d: bool,
    cam: kbview::Cam,
    cam3d: kbview::Cam,
    zoom: f32,
    pan: egui::Vec2,
    zoom_now: f32,
    pan_now: egui::Vec2,
    turning: bool,
    live: bool,
    busy: bool,
    pending: bool,
    link: Option<Link>,
    online: bool,
    side_light: Option<u8>,
    side_light_pending: bool,
    selected: Option<usize>,
    painting: bool,
    layer: u8,
    keymaps: HashMap<u8, (Vec<[u8; 4]>, Vec<[u8; 4]>)>,
    filter: String,
    macro_name: String,
    macro_text: String,
    toasts: egui_notify::Toasts,
    targets: Vec<(String, [u8; 4])>,
    perm_issue: bool,
    perm_fix: Arc<Mutex<Option<Result<(), String>>>>,
    fixing: bool,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, core: Arc<Core>) -> Self {
        style(&cc.egui_ctx);
        *core.ctx.lock().unwrap() = Some(cc.egui_ctx.clone());
        let profiles = Profile::list();
        let s = Profile::active();
        let link = *core.link.lock().unwrap(); // known if the window was reopened from the tray
        let lv = *core.live.lock().unwrap();
        Self {
            mode: mode_index(s.mode),
            lv,
            core,
            view3d: false,
            cam: kbview::Cam::TOP,
            cam3d: kbview::Cam::ISO,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            zoom_now: 1.0,
            pan_now: egui::Vec2::ZERO,
            turning: false,
            rename: s.name.clone(),
            s,
            profiles,
            tab: Tab::Lighting,
            live: false,
            busy: false,
            pending: false,
            link,
            online: link.is_some(),
            side_light: None,
            side_light_pending: false,
            selected: None,
            painting: false,
            layer: 0,
            keymaps: HashMap::new(),
            filter: String::new(),
            macro_name: "Macro".into(),
            macro_text: String::new(),
            toasts: egui_notify::Toasts::default().with_anchor(egui_notify::Anchor::BottomRight),
            targets: p::targets(),
            perm_issue: false,
            perm_fix: Arc::new(Mutex::new(None)),
            fixing: false,
        }
    }

    fn send(&mut self, job: Job) {
        if self.link == Some(Link::Bluetooth) && !matches!(job, Job::Poll | Job::LoadKeymap(_)) {
            self.toasts.info("Switch the F75 to 2.4G or USB-C to change settings");
            return;
        }
        self.busy = true;
        self.core.send(job);
    }

    /// Auto-send the current lighting; coalesces while a write is in flight.
    fn apply(&mut self) {
        self.s.mode = EFFECTS[self.mode].0;
        self.s.fx = self.lv.fx.map(|f| format!("{f:?}")).unwrap_or_default();
        self.s.save();
        if self.link.is_none() || self.link == Some(Link::Bluetooth) {
            return;
        }
        if self.streaming() {
            return; // brightness etc. only change how streamed frames are scaled
        }
        if self.busy {
            self.pending = true;
            return;
        }
        let (hw, ..) = EFFECTS[self.mode];
        if hw == p::CUSTOM_MODE {
            self.send(Job::PerKey { colours: Box::new(self.s.colours), brightness: self.s.brightness });
        } else {
            self.send(Job::Effect { mode: hw, brightness: self.s.brightness, speed: self.s.speed, colour: self.s.colour });
        }
    }

    fn streaming(&self) -> bool {
        self.lv.streaming() && self.link == Some(Link::Wired)
    }

    fn switch_profile(&mut self, pr: Profile) {
        self.s.save();
        self.s = pr;
        self.s.save();
        self.mode = mode_index(self.s.mode);
        self.lv.fx = fx_by_name(&self.s.fx);
        self.lv.audio = false;
        self.lv.palette = self.s.palette;
        self.rename = self.s.name.clone();
        self.profiles = Profile::list();
        self.toasts.info(format!("Profile: {}", self.s.name));
        if self.link.is_some_and(|l| l != Link::Bluetooth) {
            let pr = Box::new(self.s.clone());
            self.send(Job::ApplyProfile(pr));
        }
    }

    fn unique_name(&self, base: &str) -> String {
        (1..).map(|i| if i == 1 { base.to_string() } else { format!("{base} {i}") })
            .find(|n| !self.profiles.iter().any(|p| p.eq_ignore_ascii_case(n)))
            .unwrap()
    }

    fn profile_menu(&mut self, ui: &mut Ui) {
        let mut pick = None;
        egui::ComboBox::from_id_salt("profile")
            .selected_text(RichText::new(format!("{}  {}", icon::STACK, self.s.name)).family(display()).size(17.0).color(TEXT))
            .width(190.0)
            .show_ui(ui, |ui| {
                for n in &self.profiles {
                    if ui.selectable_label(*n == self.s.name, RichText::new(n).family(display()).size(16.0)).clicked() && *n != self.s.name {
                        pick = Some(n.clone());
                    }
                }
            });
        if let Some(pr) = pick.and_then(|n| Profile::load(&n)) {
            self.switch_profile(pr);
        }
        ui.menu_button(RichText::new(icon::DOTS_THREE).size(20.0), |ui| {
            if ui.button(format!("{}  New profile", icon::PLUS)).clicked() {
                let mut pr = Profile::new(&self.unique_name("Profile"));
                preset_rainbow(&mut pr.colours);
                pr.save();
                self.switch_profile(pr);
                ui.close();
            }
            if ui.button(format!("{}  Duplicate", icon::COPY)).clicked() {
                let mut pr = self.s.clone();
                pr.name = self.unique_name(&format!("{} copy", self.s.name));
                pr.save();
                self.switch_profile(pr);
                ui.close();
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.rename).desired_width(130.0));
                let name = self.rename.trim().to_string();
                let ok = !name.is_empty() && name != self.s.name && !self.profiles.iter().any(|p| p.eq_ignore_ascii_case(&name));
                if ui.add_enabled(ok, egui::Button::new(format!("{}  Rename", icon::PENCIL_SIMPLE))).clicked() {
                    Profile::delete(&self.s.name);
                    self.s.name = name;
                    self.s.save();
                    self.profiles = Profile::list();
                    ui.close();
                }
            });
            ui.separator();
            if ui.add_enabled(self.profiles.len() > 1, egui::Button::new(format!("{}  Delete \"{}\"", icon::TRASH, self.s.name))).clicked() {
                Profile::delete(&self.s.name);
                self.profiles = Profile::list();
                if let Some(pr) = self.profiles.first().and_then(|n| Profile::load(n)) {
                    self.s = pr.clone();
                    self.switch_profile(pr);
                }
                ui.close();
            }
        });
    }

    fn poll(&mut self) {
        if self.core.profile_changed.swap(false, Ordering::SeqCst) {
            self.s = Profile::active();
            self.mode = mode_index(self.s.mode);
            self.rename = self.s.name.clone();
            self.profiles = Profile::list();
            self.keymaps.clear();
        }
        let events: Vec<Event> = self.core.events.lock().unwrap().try_iter().collect();
        for ev in events {
            match ev {
                Event::Connected(l) => {
                    self.link = Some(l);
                    self.perm_issue = false;
                    self.online = false;
                    self.side_light = None;
                    if l != Link::Wired && self.lv.streaming() {
                        self.lv.fx = None;
                        self.lv.audio = false;
                        self.toasts.info("Live effects need the USB-C cable - showing the profile lighting");
                    }
                    self.toasts.success(format!("AULA F75 connected · {}", l.name()));
                }
                Event::Lost(e) => {
                    self.busy = false;
                    self.perm_issue = e.contains("Permission denied");
                    if self.link.is_some() {
                        self.toasts.warning("Keyboard disconnected");
                    }
                    self.link = None;
                    self.side_light = None;
                }
                Event::Online(o) => {
                    if self.online && !o {
                        self.toasts.info("Keyboard asleep - press a key to wake it");
                    }
                    self.online = o;
                    if !o {
                        self.side_light = None;
                    }
                }
                Event::SideLightRead(value) => self.side_light = Some(value),
                Event::SideLightSaved(mode) => {
                    self.side_light_pending = false;
                    if let Some(mode) = mode {
                        self.side_light = Some(mode as u8);
                    }
                }
                Event::Block { mode, brightness, speed, colour, sleep } => {
                    self.mode = mode;
                    self.s.brightness = brightness;
                    self.s.speed = speed;
                    self.s.colour = colour;
                    self.s.sleep = sleep;
                }
                Event::Keymap { layer, codes, backup } => {
                    // the profile keeps its own layout (macro keys are re-derived from its macros)
                    let mut own: Vec<u8> = codes.iter().flatten().copied().collect();
                    if layer == 0 {
                        for m in &self.s.macros {
                            let o = m.led as usize * 4;
                            if let Some(b) = backup.get(m.led as usize) {
                                own[o..o + 4].copy_from_slice(b);
                            }
                        }
                    }
                    let bak: Vec<u8> = backup.iter().flatten().copied().collect();
                    self.s.keymaps[layer as usize & 1] = (own != bak).then_some(own);
                    self.s.save();
                    self.keymaps.insert(layer, (codes, backup));
                }
                Event::Done(m) => {
                    self.busy = false;
                    if !m.is_empty() {
                        self.toasts.success(m);
                    }
                    if self.pending {
                        self.pending = false;
                        self.apply();
                    }
                }
                Event::Error(e) => {
                    self.busy = false;
                    self.pending = false;
                    self.toasts.error(e);
                }
            }
        }
        if let Some(r) = self.perm_fix.lock().unwrap().take() {
            self.fixing = false;
            match r {
                Ok(()) => {
                    self.toasts.success("Permissions fixed - connecting…");
                    self.perm_issue = false;
                }
                Err(e) => {
                    self.toasts.error(e);
                }
            }
        }
    }

    fn shown_colours(&mut self, t: f32) -> Colours {
        let _ = t;
        let shown = |c: Colours, b: u8| {
            // screen colours are already gamma-encoded, so the preview scales linearly
            let k = 0.12 + 0.88 * (b.min(9) as f32 / 9.0);
            c.map(|p| p.map(|v| (v as f32 * k).round() as u8))
        };
        if self.streaming() {
            if let Some(c) = *self.core.frame.lock().unwrap() {
                return shown(c, self.s.brightness);
            }
        }
        // Built-in effects are animated by the keyboard itself and can't be read back,
        // so the preview is dimmed instead of showing the saved per-key colours.
        match EFFECTS[self.mode].0 {
            p::CUSTOM_MODE => shown(self.s.colours, self.s.brightness),
            0 => [[0; 3]; LED_COUNT],
            _ => self.s.colours.map(|c| c.map(|v| v / 8)),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let before = *self.core.live.lock().unwrap();
        self.lv = before;
        self.poll();
        let t = ui.input(|i| i.time) as f32;
        let colours = self.shown_colours(t);
        ui.ctx().request_repaint_after(Duration::from_millis(500)); // connection state

        let bar = |m: Margin| egui::Frame::new().fill(PANEL).inner_margin(m).stroke(Stroke::new(1.0, LINE));
        egui::Panel::top("top").exact_size(72.0).frame(bar(Margin::symmetric(18, 10))).show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("nav").exact_size(88.0).frame(bar(Margin::symmetric(8, 14))).show(ui, |ui| self.nav(ui));
        egui::Panel::right("side").exact_size(380.0).frame(bar(Margin::same(20))).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Lighting => self.lighting(ui),
                Tab::Keys => self.keys(ui),
                Tab::Macros => self.macros(ui),
                Tab::Audio => self.audio_tab(ui),
                Tab::Power => self.power(ui),
            });
        });
        egui::CentralPanel::no_frame()
            .frame(egui::Frame::new().fill(BG).inner_margin(Margin::same(26)))
            .show(ui, |ui| self.keyboard(ui, t, &colours));
        self.toasts.show(ui.ctx());
        self.lv.palette = self.s.palette;
        self.lv.brightness = self.s.brightness;
        if self.lv != before {
            *self.core.live.lock().unwrap() = self.lv;
            self.core.refresh_tray();
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.s.save();
        *self.core.ctx.lock().unwrap() = None;
    }
}

// ------------------------------------------------------------------ chrome

impl App {
    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            logo(ui);
            ui.add_space(10.0);
            ui.label(RichText::new("F75 STUDIO").family(display()).size(25.0).color(TEXT).strong());
            ui.add_space(6.0);
            ui.label(RichText::new("AULA").family(display()).size(25.0).color(ACC));
            ui.add_space(26.0);
            self.profile_menu(ui);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !self.streaming() && self.link == Some(Link::Wired) {
                    ui.add(toggle(&mut self.live)).on_hover_text("Stream paint strokes to the keyboard instantly");
                    ui.label(RichText::new("LIVE").family(display()).size(15.0).color(DIM));
                }
                ui.add_space(18.0);
                ui.allocate_ui_with_layout(vec2(250.0, 44.0), Layout::left_to_right(Align::Center), |ui| {
                egui::Frame::new().fill(CARD).corner_radius(10).stroke(Stroke::new(1.0, LINE))
                    .inner_margin(Margin::symmetric(14, 4)).show(ui, |ui| {
                        ui.set_width(220.0);
                        ui.horizontal(|ui| {
                            let (ic, state, col) = match (self.link, self.online) {
                                (None, _) => (icon::PLUGS, "Not connected".to_string(), RED),
                                (Some(l), false) => (link_icon(l), format!("{} · asleep", l.name()), AMBER),
                                (Some(l), true) => (link_icon(l), format!("{} · online", l.name()), ACC),
                            };
                            ui.label(RichText::new(ic).size(20.0).color(col));
                            ui.vertical(|ui| {
                                ui.label(RichText::new("AULA F75").family(display()).size(16.0).color(TEXT));
                                ui.label(RichText::new(state).size(11.0).color(DIM));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                                ui.painter().circle_filled(r.center(), 4.0, col);
                            });
                        });
                    });
                });
            });
        });
    }

    fn nav(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            for (tab, ic, name) in [
                (Tab::Lighting, icon::LIGHTBULB, "LIGHTING"),
                (Tab::Keys, icon::KEYBOARD, "KEYS"),
                (Tab::Macros, icon::LIGHTNING, "MACROS"),
                (Tab::Audio, icon::WAVEFORM, "AUDIO"),
                (Tab::Power, icon::MOON, "POWER"),
            ] {
                let on = self.tab == tab;
                let (r, resp) = ui.allocate_exact_size(vec2(72.0, 64.0), Sense::click());
                let pt = ui.painter();
                if on || resp.hovered() {
                    pt.rect_filled(r, 10, if on { ACC_BG } else { CARD });
                }
                if on {
                    pt.rect_filled(Rect::from_min_size(r.left_top() + vec2(0.0, 14.0), vec2(3.0, 36.0)), 2, ACC);
                }
                let c = if on { ACC } else { DIM };
                pt.text(r.center() - vec2(0.0, 9.0), Align2::CENTER_CENTER, ic, FontId::proportional(24.0), c);
                pt.text(r.center() + vec2(0.0, 17.0), Align2::CENTER_CENTER, name, FontId::new(12.0, display()), c);
                if resp.clicked() {
                    self.tab = tab;
                    if matches!(tab, Tab::Keys | Tab::Macros) {
                        if tab == Tab::Macros {
                            self.layer = 0;
                        }
                        if !self.keymaps.contains_key(&self.layer) {
                            let l = self.layer;
                            self.send(Job::LoadKeymap(l));
                        }
                    }
                }
                ui.add_space(6.0);
            }
        });
    }

    // ------------------------------------------------------------------ keyboard + knob

    fn keyboard(&mut self, ui: &mut Ui, t: f32, colours: &Colours) {
        let builtin = self.tab == Tab::Lighting && !self.streaming() && !matches!(EFFECTS[self.mode].0, p::CUSTOM_MODE | 0);
        let hint = match self.tab {
            Tab::Lighting if self.streaming() => "Studio FX: the same frame is shown here and on the keyboard",
            Tab::Lighting if builtin => "Animated by the keyboard itself, so it can't be shown here",
            _ if self.view3d => "Scroll to zoom · drag empty space to turn · double-click to reset",
            Tab::Lighting => "Click or drag across keys to paint · drag the knob for brightness",
            Tab::Keys => "Select a key, then pick what it should do",
            Tab::Macros => "Select a key to assign the macro",
            Tab::Audio => "Live audio visualizer",
            Tab::Power => "Lighting preview",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(match self.tab {
                Tab::Lighting if self.streaming() => "STUDIO FX",
                Tab::Lighting if builtin => "BUILT-IN EFFECT",
                Tab::Lighting => "PER-KEY RGB",
                Tab::Keys => "KEY ASSIGNMENT",
                Tab::Macros => "MACROS",
                Tab::Audio => "AUDIO SYNC",
                Tab::Power => "POWER",
            }).family(display()).size(17.0).color(ACC));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                for (on, label) in [(true, "3D"), (false, "2D")] {
                    let sel = self.view3d == on;
                    let b = egui::Button::new(RichText::new(label).family(display()).size(15.0).color(if sel { BG } else { TEXT }))
                        .fill(if sel { ACC } else { CARD }).stroke(Stroke::new(1.0, if sel { ACC } else { LINE })).corner_radius(6).min_size(vec2(40.0, 26.0));
                    if ui.add(b).on_hover_text(if on { "3D view: scroll to zoom, drag empty space to turn, middle-drag to pan, double-click to reset" } else { "Flat top view" }).clicked() {
                        self.view3d = on;
                    }
                }
                ui.add_space(12.0);
                ui.label(RichText::new(hint).color(DIM));
            });
        });
        ui.add_space(14.0);
        if self.perm_issue {
            egui::Frame::new().fill(Color32::from_rgb(40, 30, 10)).corner_radius(12).stroke(Stroke::new(1.0, AMBER))
                .inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::WARNING).size(26.0).color(AMBER));
                        ui.vertical(|ui| {
                            ui.label(RichText::new("KEYBOARD FOUND, ACCESS BLOCKED").family(display()).size(17.0).color(AMBER));
                            let tip = if crate::perms::rule_installed() {
                                "Rule is installed - click to re-apply, or turn the keyboard's Bluetooth/2.4G off and on once."
                            } else {
                                "Linux needs a one-time permission rule for the F75 (cable, 2.4G and Bluetooth)."
                            };
                            ui.label(RichText::new(tip).color(TEXT));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let label = if self.fixing { "Waiting for password…" } else { "Fix permissions" };
                            if ui.add_enabled(!self.fixing, egui::Button::new(RichText::new(label).family(display()).size(17.0).color(BG))
                                .fill(AMBER).corner_radius(8).min_size(vec2(190.0, 38.0))).clicked()
                            {
                                self.fixing = true;
                                let slot = self.perm_fix.clone();
                                let ctx = ui.ctx().clone();
                                std::thread::spawn(move || {
                                    *slot.lock().unwrap() = Some(crate::perms::fix().map_err(|e| format!("{e:#}")));
                                    ctx.request_repaint();
                                });
                            }
                        });
                    });
                });
            ui.add_space(12.0);
        }
        if self.link == Some(Link::Bluetooth) {
            egui::Frame::new().fill(Color32::from_rgb(14, 26, 40)).corner_radius(12).stroke(Stroke::new(1.0, Color32::from_rgb(80, 160, 255)))
                .inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::BLUETOOTH).size(26.0).color(Color32::from_rgb(80, 160, 255)));
                        ui.vertical(|ui| {
                            ui.label(RichText::new("CONNECTED OVER BLUETOOTH · VIEW ONLY").family(display()).size(17.0).color(Color32::from_rgb(120, 180, 255)));
                            ui.label(RichText::new("The F75 can't be configured over Bluetooth (it breaks typing). Switch to the 2.4G receiver or USB-C to change lighting or keys - your settings stay on the keyboard and keep working over Bluetooth.").color(TEXT));
                        });
                    });
                });
            ui.add_space(12.0);
        }
        // camera: flat top view or 3D, animated between the two
        let (target, zoom_to, pan_to) = if self.view3d { (self.cam3d, self.zoom, self.pan) } else { (kbview::Cam::TOP, 1.0, egui::Vec2::ZERO) };
        let dt = ui.input(|i| i.stable_dt).min(0.05);
        let k = 1.0 - (-dt * 9.0).exp();
        self.cam = self.cam.toward(target, k);
        self.zoom_now += (zoom_to - self.zoom_now) * k;
        self.pan_now += (pan_to - self.pan_now) * k;
        let settled = self.cam.near(target) && (self.zoom_now - zoom_to).abs() < 0.002 && (self.pan_now - pan_to).length() < 0.3;
        if settled {
            (self.cam, self.zoom_now, self.pan_now) = (target, zoom_to, pan_to);
        } else {
            ui.ctx().request_repaint();
        }
        let avail = ui.available_size();
        // room for both views, so switching doesn't move the rest of the page
        let aspect = kbview::View::aspect(kbview::Cam::TOP).min(kbview::View::aspect(kbview::Cam::ISO));
        let h = (avail.x / aspect).min(avail.y - 100.0).max(200.0);
        let (resp, _) = ui.allocate_painter(vec2(avail.x, h), Sense::click_and_drag());
        // zoomed in: keep the keyboard inside its area; otherwise let the glow spill past it
        let painter = if self.zoom_now > 1.02 { ui.painter().with_clip_rect(resp.rect) } else { ui.painter().clone() };

        // 3D controls: scroll / pinch to zoom at the cursor, drag empty space or right-drag to
        // turn, middle-drag to pan, double-click empty space to reset
        if self.view3d {
            let base = kbview::View::fit(resp.rect, self.cam, self.zoom_now, self.pan_now);
            let (kc0, kr0) = kbview::knob(&base);
            let over_key = resp.hover_pos().is_some_and(|q| kbview::hit(&base, q).is_some() || (q - kc0).length() < kr0 * 1.3);
            if let Some(q) = resp.hover_pos() {
                let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
                let f = (scroll * 0.0025).exp() * pinch;
                if (f - 1.0).abs() > 1e-4 {
                    let z1 = (self.zoom * f).clamp(0.6, 4.0);
                    let r = z1 / self.zoom;
                    self.pan = self.pan + (q - resp.rect.center() - self.pan) * (1.0 - r);
                    self.zoom = z1;
                    (self.zoom_now, self.pan_now) = (self.zoom, self.pan);
                }
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                self.turning = !over_key;
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                self.turning = false;
            }
            let d = resp.drag_delta();
            if resp.dragged_by(egui::PointerButton::Secondary) || (self.turning && resp.dragged_by(egui::PointerButton::Primary)) {
                let yaw = (self.cam3d.yaw - d.x * 0.0045 + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
                self.cam3d = kbview::Cam { yaw, pitch: (self.cam3d.pitch - d.y * 0.004).clamp(0.12, 1.40) };
                self.cam = self.cam3d;
            }
            if resp.dragged_by(egui::PointerButton::Middle) {
                self.pan += d;
                self.pan_now = self.pan;
            }
            if (resp.double_clicked() && !over_key) || resp.double_clicked_by(egui::PointerButton::Secondary) {
                (self.cam3d, self.zoom, self.pan) = (kbview::Cam::ISO, 1.0, egui::Vec2::ZERO);
            }
        }
        let view = kbview::View::fit(resp.rect, self.cam, self.zoom_now, self.pan_now);

        let keymap = self.keymaps.get(&self.layer).cloned();
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        let paint_mode = self.tab == Tab::Lighting && !self.streaming();
        let (kc, kr) = kbview::knob(&view);
        let knob = ui.interact(Rect::from_center_size(kc, vec2(kr * 2.6, kr * 2.6)), ui.id().with("knob"), Sense::click_and_drag());
        let hovered = if knob.hovered() || knob.dragged() || self.turning { None } else { pointer.and_then(|q| kbview::hit(&view, q)) };
        let remapped: Vec<bool> = KEYS.iter().map(|k| {
            matches!(self.tab, Tab::Keys | Tab::Macros) && keymap.as_ref().is_some_and(|(cur, bak)| cur.get(k.led as usize) != bak.get(k.led as usize))
        }).collect();
        if let (Some(i), Some((cur, _))) = (hovered, keymap.as_ref()) {
            if remapped[i] {
                resp.clone().on_hover_text(format!("{} → {}", key_name(i), p::describe_code(cur[KEYS[i].led as usize])));
            }
        }
        let selected = self.selected.filter(|_| matches!(self.tab, Tab::Keys | Tab::Macros)).map(|i| {
            ui.ctx().request_repaint();
            (i, 0.55 + 0.45 * (t * 4.0).sin())
        });

        // brightness knob: drag or scroll
        let scroll = if knob.hovered() { ui.input(|i| i.smooth_scroll_delta.y) } else { 0.0 };
        if knob.dragged() || scroll != 0.0 {
            let d = knob.drag_delta();
            let step = (d.x - d.y) / 14.0 + scroll / 40.0;
            let acc = ui.ctx().data_mut(|m| {
                let a: &mut f32 = m.get_temp_mut_or(knob.id, self.s.brightness as f32);
                if knob.drag_started() {
                    *a = self.s.brightness as f32;
                }
                *a = (*a + step).clamp(0.0, 9.0);
                *a
            });
            self.s.brightness = acc.round() as u8;
        }
        if knob.drag_stopped() || (scroll != 0.0 && !knob.dragged()) {
            self.apply();
        }
        let marks = kbview::Marks { hovered, selected, remapped, accent: ACC };
        kbview::paint(&painter, &view, colours, &marks, self.s.brightness as f32 / 9.0, knob.hovered() || knob.dragged());
        if knob.hovered() || knob.dragged() {
            painter.text(kc + vec2(0.0, kr + 22.0), Align2::CENTER_CENTER, format!("BRIGHTNESS {}", self.s.brightness), FontId::new(13.0, display()), ACC);
        }

        // input
        if let Some(i) = hovered {
            let k = &KEYS[i];
            if paint_mode && (resp.clicked() || resp.dragged_by(egui::PointerButton::Primary)) {
                if EFFECTS[self.mode].0 != p::CUSTOM_MODE {
                    self.mode = EFFECTS.iter().position(|e| e.0 == p::CUSTOM_MODE).unwrap();
                }
                self.s.colours[k.led as usize] = self.s.brush;
                self.painting = true;
                if self.live && !self.busy && self.link == Some(Link::Wired) {
                    self.send(Job::Frame(Box::new(self.s.colours)));
                }
            } else if !paint_mode && resp.clicked() {
                self.selected = Some(i);
            }
        }
        if self.painting && !resp.dragged() && !ui.input(|i| i.pointer.any_down()) {
            self.painting = false;
            self.apply();
        }

        ui.add_space(22.0);
        let w = (resp.rect.width() - 3.0 * 12.0) / 4.0;
        let effect = if self.lv.audio {
            fx::AUDIO.iter().find(|f| f.0 == self.lv.audio_fx).map(|f| f.1.split(" - ").next().unwrap_or(f.1)).unwrap_or("Audio")
        } else if let Some(f) = self.lv.fx {
            fx::STUDIO.iter().find(|x| x.0 == f).map(|x| x.1).unwrap_or("FX")
        } else {
            EFFECTS[self.mode].1
        };
        let sel = self.selected.filter(|_| matches!(self.tab, Tab::Keys | Tab::Macros)).map(key_name);
        let cards = [
            (icon::LIGHTBULB, "EFFECT", effect.to_string()),
            (icon::GAUGE, "BRIGHTNESS", format!("{} / 9", self.s.brightness)),
            (icon::MOON, "SLEEP", sleep_label(self.s.sleep)),
            match sel {
                Some(k) => (icon::KEYBOARD, "SELECTED KEY", k),
                None => (self.link.map(link_icon).unwrap_or(icon::PLUGS), "CONNECTION", self.link.map(|l| l.name().to_string()).unwrap_or("Offline".into())),
            },
        ];
        ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            for (ic, k, v) in cards {
                egui::Frame::new().fill(CARD).corner_radius(12).stroke(Stroke::new(1.0, LINE)).inner_margin(Margin::symmetric(16, 12)).show(ui, |ui| {
                    ui.set_width(w - 32.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(ic).size(26.0).color(ACC));
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(k).family(display()).size(13.0).color(DIM));
                            ui.label(RichText::new(v).family(display()).size(21.0).color(TEXT));
                        });
                    });
                });
            }
        });
    }

    // ------------------------------------------------------------------ tabs

    fn lighting(&mut self, ui: &mut Ui) {
        let (hw, name, has_speed, has_light, has_colour) = EFFECTS[self.mode];
        let title = match self.lv.fx.filter(|_| self.streaming()) {
            Some(f) => fx::STUDIO.iter().find(|x| x.0 == f).map(|x| x.1).unwrap_or(name),
            None => name,
        };
        heading(ui, "LIGHTING", title);
        section(ui, "STUDIO FX · SAME ON SCREEN AND KEYBOARD");
        let wired = self.link == Some(Link::Wired);
        if !wired {
            usb_only(ui);
        }
        let mut pick = None;
        ui.add_enabled_ui(wired, |ui| egui::Grid::new("studio").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            for (i, (f, name)) in fx::STUDIO.iter().enumerate() {
                if tile(ui, name, self.lv.fx == Some(*f) && wired, [162.0, 36.0]) {
                    pick = Some(*f);
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        }));
        if let Some(f) = pick {
            self.lv.audio = false;
            let on = self.lv.fx != Some(f);
            self.lv.fx = on.then_some(f);
            if on {
                self.s.fx = format!("{f:?}");
                self.s.save();
            } else {
                self.apply(); // back to the profile's own lighting
            }
        }
        if self.lv.fx.is_some() && wired {
            self.palette_picker(ui);
            section(ui, "FX SPEED");
            ui.add(egui::Slider::new(&mut self.lv.speed, 0.2..=3.0).show_value(false));
            ui.label(RichText::new(format!("{} Keeps running with the window closed (tray icon).", icon::INFO)).size(12.0).color(DIM));
        }
        ui.add_space(10.0);
        section(ui, "BUILT-IN EFFECTS · RUN ON THE KEYBOARD");
        self.side_light_picker(ui);
        if hw == p::CUSTOM_MODE {
            section(ui, "BRUSH");
            ui.horizontal_wrapped(|ui| {
                ui.color_edit_button_srgb(&mut self.s.brush);
                for c in PALETTE.iter().chain([[255, 120, 0], [120, 60, 255], [0, 0, 0]].iter()) {
                    if swatch(ui, *c, self.s.brush == *c).clicked() {
                        self.s.brush = *c;
                    }
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                let mut changed = true;
                if action(ui, icon::PAINT_BRUSH, "Fill") {
                    self.s.colours = [self.s.brush; LED_COUNT];
                } else if action(ui, icon::SPARKLE, "Rainbow") {
                    preset_rainbow(&mut self.s.colours);
                } else if action(ui, icon::KEYBOARD, "WASD") {
                    self.s.colours = [[8, 26, 14]; LED_COUNT];
                    for k in KEYS.iter().filter(|k| [0x57, 0x41, 0x53, 0x44].contains(&k.vk)) {
                        self.s.colours[k.led as usize] = self.s.brush;
                    }
                } else if action(ui, icon::TRASH, "Clear") {
                    self.s.colours = [[0; 3]; LED_COUNT];
                } else {
                    changed = false;
                }
                if changed {
                    self.lv.fx = None;
                    self.apply();
                }
            });
        }
        if has_light {
            section(ui, "BRIGHTNESS");
            let r = ui.add(egui::Slider::new(&mut self.s.brightness, 0..=9).show_value(true));
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.apply();
            }
        }
        if has_speed {
            section(ui, "SPEED");
            let r = ui.add(egui::Slider::new(&mut self.s.speed, 0..=4));
            if r.drag_stopped() || (r.changed() && !r.dragged()) {
                self.apply();
            }
        }
        if has_colour {
            section(ui, "COLOUR");
            ui.horizontal_wrapped(|ui| {
                for (i, c) in PALETTE.iter().enumerate() {
                    if swatch(ui, *c, self.s.colour == i as u8).clicked() {
                        self.s.colour = i as u8;
                        self.apply();
                    }
                }
                if ui.selectable_label(self.s.colour == RANDOM_COLOUR, "Random").clicked() {
                    self.s.colour = RANDOM_COLOUR;
                    self.apply();
                }
            });
        }
        ui.add_space(6.0);
        egui::Grid::new("fx").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            for (i, e) in EFFECTS.iter().enumerate() {
                if tile(ui, e.1, i == self.mode && !self.streaming(), [162.0, 36.0]) {
                    self.mode = i;
                    self.lv.fx = None;
                    self.lv.audio = false;
                    self.apply();
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    }

    fn side_light_picker(&mut self, ui: &mut Ui) {
        let enabled = matches!(self.link, Some(Link::Wired | Link::Receiver))
            && self.online
            && !self.busy
            && !self.side_light_pending;
        let active = self.side_light.and_then(p::SideLightMode::from_byte);
        let label = match (self.side_light, active) {
            (_, Some(mode)) => mode.label(),
            (Some(_), None) => "Unknown",
            (None, None) => "Unavailable",
        };

        ui.horizontal(|ui| {
            ui.label("Side light:");
            ui.add_enabled_ui(enabled, |ui| {
                egui::ComboBox::from_id_salt("side-light")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for mode in p::SideLightMode::ALL {
                            let selected = active == Some(mode);
                            if ui.selectable_label(selected, mode.label()).clicked() && !selected {
                                self.side_light_pending = true;
                                self.core.send(Job::SetSideLight(mode));
                            }
                        }
                    });
            });
        });
    }

    fn palette_picker(&mut self, ui: &mut Ui) {
        section(ui, "PALETTE");
        let mut pick = None;
        egui::Grid::new("palettes").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            for (i, (name, pal)) in fx::PALETTES.iter().enumerate() {
                let on = self.s.palette as usize == i;
                let (r, resp) = ui.allocate_exact_size(vec2(162.0, 34.0), Sense::click());
                let pt = ui.painter();
                pt.rect_filled(r, 7, if on { ACC_BG } else { CARD });
                let bar = Rect::from_min_size(r.left_top() + vec2(8.0, 9.0), vec2(56.0, 16.0));
                for x in 0..28 {
                    let c = fx::sample(pal, x as f32 / 28.0);
                    pt.rect_filled(Rect::from_min_size(bar.left_top() + vec2(x as f32 * 2.0, 0.0), vec2(2.0, 16.0)), 0, Color32::from_rgb(c[0], c[1], c[2]));
                }
                pt.rect_stroke(r, 7, Stroke::new(1.0, if on { ACC } else { LINE }), StrokeKind::Inside);
                pt.text(bar.right_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, *name, FontId::new(16.0, display()), if on { ACC } else { TEXT });
                if resp.clicked() {
                    pick = Some(i as u8);
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if let Some(i) = pick {
            self.s.palette = i;
            self.s.save();
        }
    }

    fn keys(&mut self, ui: &mut Ui) {
        heading(ui, "KEYS", "Remap");
        ui.horizontal(|ui| {
            for (l, n) in [(0u8, "Base layer"), (1u8, "Fn layer")] {
                if tile(ui, n, self.layer == l, [160.0, 32.0]) && self.layer != l {
                    self.layer = l;
                    if !self.keymaps.contains_key(&l) {
                        self.send(Job::LoadKeymap(l));
                    }
                }
            }
        });
        let Some(sel) = self.selected else {
            ui.add_space(14.0);
            ui.label(RichText::new("Select a key on the keyboard.").color(DIM));
            self.restore_button(ui);
            return;
        };
        let led = KEYS[sel].led;
        let current = self.keymaps.get(&self.layer).and_then(|(c, _)| c.get(led as usize).copied());
        card(ui, |ui| {
            ui.label(RichText::new("CURRENT ACTION").family(display()).size(13.0).color(DIM));
            ui.label(RichText::new(current.map(p::describe_code).unwrap_or_else(|| if self.link.is_some() { "reading…".into() } else { "Connect your F75".into() })).family(display()).size(24.0).color(TEXT));
            ui.label(RichText::new(format!("{} · matrix {}", key_name(sel), led)).size(11.0).color(DIM));
        });
        section(ui, "ASSIGN");
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(format!("{}  Search actions", icon::MAGNIFYING_GLASS)).desired_width(f32::INFINITY));
        ui.add_space(6.0);
        let f = self.filter.to_lowercase();
        let mut pick = None;
        egui::Grid::new("targets").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            for (i, (n, code)) in self.targets.iter().filter(|(n, _)| n.to_lowercase().contains(&f)).enumerate() {
                if tile(ui, n, current == Some(*code), [162.0, 30.0]) {
                    pick = Some(*code);
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if let Some(code) = pick {
            let layer = self.layer;
            self.send(Job::Remap { layer, led, code });
        }
        self.restore_button(ui);
    }

    fn restore_button(&mut self, ui: &mut Ui) {
        ui.add_space(14.0);
        if action(ui, icon::ARROW_COUNTER_CLOCKWISE, "Restore default layout") {
            let l = self.layer;
            self.send(Job::RestoreKeymap(l));
        }
    }

    fn macros(&mut self, ui: &mut Ui) {
        heading(ui, "MACROS", "Text macros");
        ui.label(RichText::new("Type text, pick a key, and the keyboard replays it, no app needed.").color(DIM));
        // macros only take over the cable (the 2.4G receiver doesn't apply them)
        let wired = self.link == Some(Link::Wired);
        if !wired {
            ui.add_space(8.0);
            usb_note(ui, "USB-C only - plug in the cable to add or change macros");
        }
        ui.add_enabled_ui(wired, |ui| self.macro_editor(ui));
    }

    fn macro_editor(&mut self, ui: &mut Ui) {
        section(ui, "NEW MACRO");
        ui.add(egui::TextEdit::singleline(&mut self.macro_name).hint_text("Name").desired_width(f32::INFINITY));
        ui.add(egui::TextEdit::multiline(&mut self.macro_text).hint_text("Text to type, e.g. gg wp").desired_rows(3).desired_width(f32::INFINITY));
        let target = self.selected.map(|i| KEYS[i].led);
        ui.add_space(6.0);
        let label = match self.selected {
            Some(i) => format!("Assign to {}", key_name(i)),
            None => "Select a key first".into(),
        };
        let mut changed = false;
        let can = ui.is_enabled() && target.is_some() && !self.macro_text.is_empty();
        if ui.add_enabled(can, primary(&label, can)).clicked() {
            let led = target.unwrap();
            self.s.macros.retain(|m| m.led != led);
            self.s.macros.push(MacroDef { name: self.macro_name.clone(), text: self.macro_text.clone(), led });
            self.macro_text.clear();
            changed = true;
        }
        section(ui, "ON KEYBOARD");
        let mut remove = None;
        for (i, m) in self.s.macros.iter().enumerate() {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&m.name).family(display()).size(18.0).color(TEXT));
                        let key = KEYS.iter().position(|k| k.led == m.led).map(key_name).unwrap_or_default();
                        ui.label(RichText::new(format!("{key} · \"{}\"", m.text.chars().take(28).collect::<String>())).size(11.0).color(DIM));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button(icon::TRASH).clicked() {
                            remove = Some(i);
                        }
                    });
                });
            });
            ui.add_space(6.0);
        }
        let mut clear = vec![];
        if let Some(i) = remove {
            clear.push(self.s.macros.remove(i).led);
            changed = true;
        }
        if changed {
            self.s.save();
            let assign = self.s.macros.iter().enumerate().map(|(i, m)| (m.led, macros::key_code(i as u8, 1))).collect();
            self.send(Job::Macros { buf: macro_buffer(&self.s.macros), assign, clear });
        }
    }

    fn audio_tab(&mut self, ui: &mut Ui) {
        heading(ui, "AUDIO", "Audio sync");
        ui.label(RichText::new("Reacts to whatever is playing on your speakers (PipeWire monitor).").color(DIM));
        ui.add_space(10.0);
        let wired = self.link == Some(Link::Wired);
        if !wired {
            usb_only(ui);
        }
        let mut on = self.lv.audio && wired;
        ui.add_enabled_ui(wired, |ui| {
            ui.horizontal(|ui| {
                if ui.add(toggle(&mut on)).changed() {
                    if on {
                        self.lv.fx = None;
                        self.lv.audio = true;
                    } else {
                        self.lv.audio = false;
                        self.apply();
                    }
                }
                ui.label(RichText::new(if on { "Listening" } else { "Off" }).family(display()).size(18.0).color(if on { ACC } else { DIM }));
            });
            section(ui, "EFFECT");
            egui::Grid::new("audiofx").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
                for (i, (f, name)) in fx::AUDIO.iter().filter(|f| !matches!(f.0, fx::Fx::Aula(_))).enumerate() {
                    if tile(ui, name, self.lv.audio_fx == *f, [162.0, 34.0]) {
                        self.lv.audio_fx = *f;
                    }
                    if i % 2 == 1 {
                        ui.end_row();
                    }
                }
            });
            section(ui, "AULA MUSIC");
            for (f, name) in fx::AUDIO.iter().filter(|f| matches!(f.0, fx::Fx::Aula(_))) {
                if tile(ui, name, self.lv.audio_fx == *f, [332.0, 30.0]) {
                    self.lv.audio_fx = *f;
                }
                ui.add_space(4.0);
            }
            if matches!(self.lv.audio_fx, fx::Fx::Aula(_)) {
                section(ui, "GAIN FACTOR");
                ui.add(
                    egui::Slider::new(&mut self.lv.gain, 0..=crate::aula_music::GAIN_STEPS - 1)
                        .custom_formatter(|v, _| format!("x{:.1}", crate::aula_music::gain(v as u8))),
                );
                section(ui, "SMOOTHNESS");
                ui.add(egui::Slider::new(&mut self.lv.smooth, 1..=10));
                section(ui, "COLOR");
                ui.horizontal(|ui| {
                    if tile(ui, "ColorFull", self.lv.colorful, [110.0, 30.0]) {
                        self.lv.colorful = !self.lv.colorful;
                    }
                    if !self.lv.colorful {
                        for c in PALETTE.iter().take(7) {
                            if swatch(ui, *c, self.lv.colour == *c).clicked() {
                                self.lv.colour = *c;
                            }
                        }
                    }
                });
            } else {
                self.palette_picker(ui);
                section(ui, "SENSITIVITY");
                ui.add(egui::Slider::new(&mut self.lv.sensitivity, 0.4..=2.5).show_value(false));
            }
            section(ui, "BRIGHTNESS");
            if ui.add(egui::Slider::new(&mut self.s.brightness, 0..=9).show_value(true)).changed() {
                self.apply();
            }
        });
        if on {
            section(ui, "INPUT");
            let bands = *self.core.bands.lock().unwrap();
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
            let w = r.width() / BANDS as f32;
            for (i, b) in bands.iter().enumerate() {
                let h = b * r.height();
                let bar = Rect::from_min_max(pos2(r.left() + i as f32 * w + 1.0, r.bottom() - h), pos2(r.left() + (i + 1) as f32 * w - 1.0, r.bottom()));
                ui.painter().rect_filled(bar, 2, ACC.gamma_multiply(0.4 + 0.6 * b));
            }
            ui.add_space(8.0);
            ui.label(RichText::new(format!("{} Keeps running with the window closed (tray icon).", icon::INFO)).size(12.0).color(DIM));
        }
    }

    fn power(&mut self, ui: &mut Ui) {
        heading(ui, "POWER", "Sleep");
        ui.label(RichText::new("In wireless mode the keyboard turns its lights off after this much idle time.").color(DIM));
        section(ui, "SLEEP AFTER");
        let mut pick = None;
        egui::Grid::new("sleep").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            for (i, (secs, _)) in p::SLEEP.iter().enumerate() {
                let label = if *secs == 0 { "Never (always on)".to_string() } else { sleep_label(*secs) };
                if tile(ui, &label, self.s.sleep == *secs, [162.0, 36.0]) {
                    pick = Some(*secs);
                }
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if let Some(s) = pick {
            self.s.sleep = s;
            self.s.save();
            self.send(Job::Sleep(s));
        }
        ui.add_space(10.0);
        ui.label(RichText::new(format!("{} Always-on drains the battery faster. The cable never sleeps.", icon::INFO)).size(12.0).color(DIM));
    }
}

// ------------------------------------------------------------------ widgets

fn style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert("rajdhani".into(), egui::FontData::from_static(include_bytes!("../assets/fonts/Rajdhani-Bold.ttf")).into());
    fonts.font_data.insert("rajdhani-semi".into(), egui::FontData::from_static(include_bytes!("../assets/fonts/Rajdhani-SemiBold.ttf")).into());
    fonts.families.insert(display(), vec!["rajdhani".into(), "rajdhani-semi".into()]);
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    if let Some(f) = fonts.families.get_mut(&display()) {
        f.push("phosphor".into());
    }
    ctx.set_fonts(fonts);
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = PANEL;
    v.extreme_bg_color = BG;
    v.selection.bg_fill = ACC.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, ACC);
    v.widgets.inactive.bg_fill = CARD;
    v.widgets.inactive.weak_bg_fill = CARD;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACC.gamma_multiply(0.6));
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACC);
    v.slider_trailing_fill = true;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.all_styles_mut(|s| s.spacing.slider_width = 240.0);
}

fn logo(ui: &mut Ui) {
    let (r, _) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::hover());
    let pt = ui.painter();
    pt.rect_filled(r, 9, ACC_BG);
    pt.rect_stroke(r, 9, Stroke::new(1.0, ACC.gamma_multiply(0.7)), StrokeKind::Inside);
    for i in 0..3 {
        for j in 0..3 {
            let c = r.min + vec2(9.5 + i as f32 * 7.0, 9.5 + j as f32 * 7.0);
            pt.rect_filled(Rect::from_min_size(c, vec2(5.0, 5.0)), 1, if (i + j) % 2 == 0 { ACC } else { ACC.gamma_multiply(0.5) });
        }
    }
}

fn heading(ui: &mut Ui, kicker: &str, title: &str) {
    ui.label(RichText::new(kicker).family(display()).size(14.0).color(ACC));
    ui.label(RichText::new(title).family(display()).size(32.0).color(TEXT));
    ui.add_space(10.0);
}

fn section(ui: &mut Ui, s: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(s).family(display()).size(14.0).color(DIM));
    ui.add_space(2.0);
}

fn card(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::new().fill(CARD).corner_radius(10).stroke(Stroke::new(1.0, LINE)).inner_margin(Margin::same(14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
}

fn tile(ui: &mut Ui, text: &str, on: bool, size: [f32; 2]) -> bool {
    let b = egui::Button::new(RichText::new(text).family(display()).size(16.0).color(if on { ACC } else { TEXT }))
        .fill(if on { ACC_BG } else { CARD })
        .stroke(Stroke::new(1.0, if on { ACC } else { LINE }))
        .corner_radius(7);
    ui.add_sized(size, b).clicked()
}

fn primary(text: &str, on: bool) -> egui::Button<'static> {
    let (fill, ink) = if on { (ACC, BG) } else { (CARD, DIM) };
    egui::Button::new(RichText::new(text.to_string()).family(display()).size(18.0).color(ink)).fill(fill).corner_radius(8).min_size(vec2(330.0, 40.0))
}

fn action(ui: &mut Ui, ic: &str, s: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(format!("{ic}  {s}")).color(TEXT)).fill(CARD).stroke(Stroke::new(1.0, LINE)).corner_radius(7)
        .min_size(vec2(74.0, 32.0))).clicked()
}

fn swatch(ui: &mut Ui, c: [u8; 3], on: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    ui.painter().rect_filled(r, 6, Color32::from_rgb(c[0], c[1], c[2]));
    let s = if on { Stroke::new(2.0, ACC) } else { Stroke::new(1.0, LINE) };
    ui.painter().rect_stroke(r.expand(if on { 2.0 } else { 0.0 }), 7, s, StrokeKind::Outside);
    resp
}

fn toggle(on: &mut bool) -> impl egui::Widget + '_ {
    move |ui: &mut Ui| {
        let (r, mut resp) = ui.allocate_exact_size(vec2(46.0, 24.0), Sense::click());
        if resp.clicked() {
            *on = !*on;
            resp.mark_changed();
        }
        let a = ui.ctx().animate_bool_responsive(resp.id, *on);
        let pt = ui.painter();
        pt.rect_filled(r, 12, if *on { ACC_BG } else { CARD });
        pt.rect_stroke(r, 12, Stroke::new(1.0, if *on { ACC } else { LINE }), StrokeKind::Inside);
        let x = egui::lerp((r.left() + 12.0)..=(r.right() - 12.0), a);
        pt.circle_filled(pos2(x, r.center().y), 8.0, if *on { ACC } else { DIM });
        resp
    }
}

fn usb_only(ui: &mut Ui) {
    usb_note(ui, "USB-C only - plug in the cable to use live effects");
}

fn usb_note(ui: &mut Ui, text: &str) {
    egui::Frame::new().fill(CARD).corner_radius(8).stroke(Stroke::new(1.0, AMBER.gamma_multiply(0.6))).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(format!("{}  {text}", icon::USB)).color(AMBER));
    });
    ui.add_space(6.0);
}

fn link_icon(l: Link) -> &'static str {
    match l {
        Link::Wired => icon::USB,
        Link::Receiver => icon::WIFI_HIGH,
        Link::Bluetooth => icon::BLUETOOTH,
    }
}

fn key_name(i: usize) -> String {
    match KEYS[i].label {
        "" => "Space".into(),
        l => l.into(),
    }
}

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

pub fn preset_rainbow(c: &mut Colours) {
    for k in KEYS {
        let x = (k.rect[0] + k.rect[2]) / 2.0 / CANVAS[0];
        c[k.led as usize] = hsv(x * 0.85, 1.0, 1.0);
    }
}

pub fn run() -> anyhow::Result<()> {
    if crate::instance::signal("show") {
        return Ok(()); // already running: it brings its window up
    }
    let core = Core::start(&Profile::active());
    crate::instance::listen(core.clone());
    let tray = crate::tray::spawn(core.clone());
    loop {
        window(core.clone())?;
        // closed: stay in the background if there is a tray to come back from
        // (or a live effect running - launching the app again reopens the window)
        let background = tray || core.live.lock().unwrap().streaming();
        if core.quit.load(Ordering::SeqCst) || !background {
            break;
        }
        core.wait_show();
        if core.quit.load(Ordering::SeqCst) {
            break;
        }
    }
    core.stop_live();
    core.flush(Duration::from_secs(3));
    Ok(())
}

fn window(core: Arc<Core>) -> anyhow::Result<()> {
    // 4x multisampling smooths the 3D keyboard's edges; drivers without it get the plain window
    static NO_MSAA: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let msaa = !NO_MSAA.load(Ordering::SeqCst);
    match open_window(core.clone(), if msaa { 4 } else { 0 }) {
        Err(e) if msaa && format!("{e}").contains("config") => {
            NO_MSAA.store(true, Ordering::SeqCst);
            open_window(core, 0)
        }
        r => r,
    }
}

fn open_window(core: Arc<Core>, multisampling: u16) -> anyhow::Result<()> {
    let icon_png = include_bytes!("../assets/icon.png");
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("F75 Studio")
            .with_app_id("f75-studio")
            .with_inner_size([1400.0, 820.0])
            .with_min_inner_size([1180.0, 680.0])
            .with_icon(eframe::icon_data::from_png_bytes(icon_png).unwrap_or_default()),
        multisampling,
        ..Default::default()
    };
    eframe::run_native("F75 Studio", opts, Box::new(move |cc| Ok(Box::new(App::new(cc, core)))))
        .map_err(|e| anyhow::anyhow!("{e}"))
}
