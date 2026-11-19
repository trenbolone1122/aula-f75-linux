//! Device thread: runs every keyboard job in order and, over USB-C, renders and streams the live
//! effects (studio FX / audio sync) itself, so they keep running with the window closed.
use crate::audio::{Audio, Snapshot, BANDS};
use crate::device::F75;
use crate::fx::{self, Fx};
use crate::macros;
use crate::protocol::{self as p, Link, EFFECTS, LED_COUNT};
use crate::settings::{Change, Settings, reset_allowed};
use crate::state::{self, MacroDef, Profile};
use eframe::egui;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub type Colours = [[u8; 3]; LED_COUNT];

/// What is streamed live (USB-C only).
#[derive(Clone, Copy, PartialEq)]
pub struct Live {
    pub fx: Option<Fx>,
    pub audio: bool,
    pub audio_fx: Fx,
    pub palette: u8,
    pub speed: f32,
    pub sensitivity: f32,
    pub brightness: u8, // 0..9, applied to streamed frames
    // AULA music modes
    pub gain: u8,
    pub smooth: u8,
    pub colorful: bool,
    pub colour: [u8; 3],
}

/// Brightness 0..9 as a linear LED level, on a curve that looks even to the eye (4/9 ~ half).
pub fn level(b: u8) -> f32 {
    (b.min(9) as f32 / 9.0).powf(2.2)
}

pub fn dim(c: &Colours, b: u8) -> Colours {
    let k = level(b);
    c.map(|p| p.map(|v| (v as f32 * k).round() as u8))
}

impl Live {
    pub fn streaming(&self) -> bool {
        self.fx.is_some() || self.audio
    }
}

/// State shared by the window, the tray and the device thread; outlives the window.
pub struct Core {
    jobs: Mutex<Sender<Job>>,
    pub events: Mutex<Receiver<Event>>,
    pub live: Mutex<Live>,
    pub frame: Mutex<Option<Colours>>,
    pub bands: Mutex<[f32; BANDS]>,
    pub link: Mutex<Option<Link>>,
    pub settings: Mutex<Option<Settings>>,
    pub reset_session: Mutex<Option<u64>>,
    pub ctx: Mutex<Option<egui::Context>>,
    pub profile_changed: AtomicBool,
    pub tray_refresh: Mutex<Option<Box<dyn Fn() + Send>>>,
    show: (Mutex<bool>, Condvar),
    pub quit: AtomicBool,
}

impl Core {
    pub fn start(pr: &Profile) -> Arc<Core> {
        let (core, jobs, events) = Self::new(pr);
        let shared = core.clone();
        std::thread::spawn(move || worker(jobs, events, shared));
        core
    }

    fn new(pr: &Profile) -> (Arc<Self>, Receiver<Job>, Sender<Event>) {
        let (jt, jr) = channel();
        let (et, er) = channel();
        let core = Arc::new(Core {
            jobs: Mutex::new(jt),
            events: Mutex::new(er),
            live: Mutex::new(Live {
                fx: fx_by_name(&pr.fx),
                audio: false,
                audio_fx: Fx::Spectrum,
                palette: pr.palette,
                speed: 1.0,
                sensitivity: 1.0,
                brightness: pr.brightness,
                gain: crate::aula_music::DEFAULT_GAIN,
                smooth: crate::aula_music::DEFAULT_SMOOTH,
                colorful: true,
                colour: crate::aula_music::DEFAULT_COLOUR,
            }),
            frame: Mutex::new(None),
            bands: Mutex::new([0.0; BANDS]),
            link: Mutex::new(None),
            settings: Mutex::new(None),
            reset_session: Mutex::new(None),
            ctx: Mutex::new(None),
            profile_changed: AtomicBool::new(false),
            tray_refresh: Mutex::new(None),
            show: (Mutex::new(false), Condvar::new()),
            quit: AtomicBool::new(false),
        });
        (core, jr, et)
    }

    pub fn send(&self, job: Job) {
        let _ = self.jobs.lock().unwrap().send(job);
    }

    pub fn repaint(&self) {
        if let Some(c) = self.ctx.lock().unwrap().as_ref() {
            c.request_repaint();
        }
    }

    pub fn refresh_tray(&self) {
        if let Some(f) = self.tray_refresh.lock().unwrap().as_ref() {
            f();
        }
    }

    pub fn wired(&self) -> bool {
        *self.link.lock().unwrap() == Some(Link::Wired)
    }

    pub fn request_show(&self) {
        *self.show.0.lock().unwrap() = true;
        self.show.1.notify_all();
        if let Some(c) = self.ctx.lock().unwrap().as_ref() {
            c.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
    }

    pub fn request_quit(&self) {
        self.quit.store(true, Ordering::SeqCst);
        self.request_show();
        if let Some(c) = self.ctx.lock().unwrap().as_ref() {
            c.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Blocks until the tray / a second launch asks for the window (or quit).
    pub fn wait_show(&self) {
        let mut g = self.show.0.lock().unwrap();
        while !*g {
            g = self.show.1.wait(g).unwrap();
        }
        *g = false;
    }

    /// Stops streaming and puts the profile's own lighting back.
    pub fn stop_live(&self) {
        let mut l = self.live.lock().unwrap();
        let was = l.streaming();
        l.fx = None;
        l.audio = false;
        drop(l);
        if was {
            self.send(lighting_job(&Profile::active()));
        }
    }

    /// Waits (bounded) until the device thread has finished queued jobs.
    pub fn flush(&self, timeout: Duration) {
        let (t, r) = channel();
        self.send(Job::Flush(t));
        let _ = r.recv_timeout(timeout);
    }
}

pub fn lighting_job(pr: &Profile) -> Job {
    if pr.mode == p::CUSTOM_MODE {
        Job::PerKey { colours: Box::new(pr.colours), brightness: pr.brightness }
    } else {
        Job::Effect { mode: pr.mode, brightness: pr.brightness, speed: pr.speed, colour: pr.colour }
    }
}

pub enum Job {
    Poll,
    Effect { mode: u8, brightness: u8, speed: u8, colour: u8 },
    PerKey { colours: Box<Colours>, brightness: u8 },
    SetSetting(Change),
    FactoryReset(u64),
    Frame(Box<Colours>),
    LoadKeymap(u8),
    Remap { layer: u8, led: u8, code: [u8; 4] },
    RestoreKeymap(u8),
    Macros { buf: Vec<u8>, assign: Vec<(u8, [u8; 4])>, clear: Vec<u8> },
    ApplyProfile(Box<Profile>),
    /// replies once every earlier job has run
    Flush(Sender<()>),
}

pub enum Event {
    Connected(Link),
    Lost(String),
    Online(bool),
    SettingFinished(Option<Change>),
    ResetFinished(Result<(), String>),
    Block { mode: usize, brightness: u8, speed: u8, colour: u8 },
    Keymap { layer: u8, codes: Vec<[u8; 4]>, backup: Vec<[u8; 4]> },
    Done(String),
    Error(String),
}

struct Worker {
    core: Arc<Core>,
    last_online: std::time::Instant,
    online: Option<bool>,
    session: u64,
    resetting: Option<Instant>,
    kb: Option<F75>,
    keymaps: HashMap<u8, Vec<u8>>,
    backups: HashMap<u8, Vec<u8>>,
    tx: Sender<Event>,
    /// The keyboard's own brightness is held at 9 while we stream, so streamed frames are only
    /// dimmed once (here). Any lighting write clears this.
    held: bool,
}

fn codes(b: &[u8]) -> Vec<[u8; 4]> {
    b.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
}

impl Worker {
    fn ensure(&mut self) -> anyhow::Result<&mut F75> {
        if self.kb.is_none() {
            // F75_STUDIO_DRY=wired|24g: pretend a keyboard is connected (screenshots without hardware)
            let mut kb = match std::env::var("F75_STUDIO_DRY").as_deref() {
                Ok("wired") => F75::dry_run(Link::Wired),
                Ok("bluetooth") => F75::dry_run(Link::Bluetooth),
                Ok("24g-offline") => F75::dry_offline_receiver(),
                Ok(_) => F75::dry_run(Link::Receiver),
                Err(_) => F75::open()?,
            };
            let _ = self.tx.send(Event::Connected(kb.link));
            *self.core.link.lock().unwrap() = Some(kb.link);
            self.keymaps.clear();
            self.backups.clear();
            self.held = false;
            self.online = None;
            if let Ok(online) = kb.online() {
                self.report_online(kb.link, online);
                if online {
                    let streaming = self.core.live.lock().unwrap().streaming();
                    Self::sync_led_block(&mut kb, &self.core, &self.tx, !streaming);
                }
            }
            self.kb = Some(kb);
        }
        Ok(self.kb.as_mut().unwrap())
    }

    fn report_online(&mut self, link: Link, online: bool) {
        self.last_online = Instant::now();
        if self.online != Some(online) {
            self.session = self.session.wrapping_add(1);
        }
        self.online = Some(online);
        *self.core.reset_session.lock().unwrap() =
            (self.resetting.is_none() && reset_allowed(Some(link), online)).then_some(self.session);
        if !online {
            *self.core.settings.lock().unwrap() = None;
        }
        let _ = self.tx.send(Event::Online(online));
    }

    fn disconnect(&mut self) {
        self.kb = None;
        self.online = None;
        *self.core.link.lock().unwrap() = None;
        *self.core.settings.lock().unwrap() = None;
        *self.core.reset_session.lock().unwrap() = None;
        self.keymaps.clear();
        self.backups.clear();
        self.held = false;
    }

    fn sync_led_block(kb: &mut F75, core: &Core, tx: &Sender<Event>, include_effect: bool) {
        if kb.link == Link::Bluetooth {
            return;
        }
        let Ok(block) = kb.get_led_block() else {
            return;
        };
        *core.settings.lock().unwrap() = Settings::read(&block);
        if include_effect
            && !kb.dry()
            && let Some((mode, brightness, speed, colour)) = p::effect_from_block(&block)
        {
            let _ = tx.send(Event::Block {
                mode,
                brightness,
                speed,
                colour,
            });
        }
    }

    fn keymap(&mut self, layer: u8) -> anyhow::Result<Vec<u8>> {
        if let Some(k) = self.keymaps.get(&layer) {
            return Ok(k.clone());
        }
        let km = self.ensure()?.get_keymap(layer)?;
        let backup = state::keymap_backup(layer, &km);
        self.keymaps.insert(layer, km.clone());
        self.backups.insert(layer, backup);
        Ok(km)
    }

    fn send_keymap(&self, layer: u8) {
        if let (Some(k), Some(b)) = (self.keymaps.get(&layer), self.backups.get(&layer)) {
            let _ = self.tx.send(Event::Keymap { layer, codes: codes(k), backup: codes(b) });
        }
    }

    fn guard(&mut self) -> anyhow::Result<()> {
        self.keymap(0)?;
        if !p::keymap_encoding_ok(&self.backups[&0]) {
            anyhow::bail!("unexpected key encoding on this keyboard - remap disabled (send `f75-studio keymap` output)");
        }
        Ok(())
    }

    fn run(&mut self, job: Job) -> anyhow::Result<Option<String>> {
        if let Some(until) = self.resetting {
            match job {
                Job::Flush(done) => {
                    let _ = done.send(());
                }
                Job::Poll if Instant::now() >= until => {
                    let core = self.core.clone();
                    let tx = self.tx.clone();
                    if self.online == Some(true) {
                        Self::sync_led_block(self.ensure()?, &core, &tx, true);
                    } else {
                        self.disconnect();
                        self.ensure()?;
                    }
                    if self.online == Some(true) && self.core.settings.lock().unwrap().is_some() {
                        self.resetting = None;
                        self.report_online(self.kb.as_ref().unwrap().link, true);
                    }
                }
                Job::Poll => {}
                _ => anyhow::bail!("Waiting for the keyboard after factory reset"),
            }
            return Ok(None);
        }
        Ok(match job {
            Job::Poll => {
                // Hotplug: Bluetooth/cable have no I/O on poll, so check the device list itself.
                // Drop the handle if its node vanished or a preferred link (cable > 2.4G > BT) appeared.
                if let Some(kb) = self.kb.as_ref().filter(|k| !k.dry()) {
                    let present = F75::detect();
                    let still_here = present.iter().any(|(l, p)| *l == kb.link && *p == kb.path);
                    let best = present.first().map(|(l, _)| *l);
                    if !still_here || best != Some(kb.link) {
                        self.disconnect();
                        let _ = self.tx.send(Event::Lost(if still_here { "switching connection…".into() } else { "keyboard disconnected".into() }));
                    }
                }
                // radio traffic can swallow key releases on 2.4G (auto-repeat), so ask rarely
                self.ensure()?;
                let due = self.last_online.elapsed() > Duration::from_secs(20);
                if self.online.is_none() || due {
                    let kb = self.kb.as_mut().unwrap();
                    let link = kb.link;
                    let online = kb.online();
                    let waking = self.online != Some(true);
                    self.report_online(link, online.as_ref().is_ok_and(|value| *value));
                    if online? && (waking || self.core.settings.lock().unwrap().is_none()) {
                        Self::sync_led_block(
                            self.kb.as_mut().unwrap(),
                            &self.core,
                            &self.tx,
                            false,
                        );
                    }
                }
                None
            }
            Job::Effect { mode, brightness, speed, colour } => {
                self.held = false;
                self.ensure()?.set_effect(mode, brightness, speed, colour)?;
                if let Some(settings) = self.core.settings.lock().unwrap().as_mut() {
                    settings.brightness = (mode != 0).then_some(brightness);
                }
                Some("Lighting updated".into())
            }
            Job::PerKey { colours, brightness } => {
                self.held = false;
                self.ensure()?.set_per_key(&colours, brightness)?;
                if let Some(settings) = self.core.settings.lock().unwrap().as_mut() {
                    settings.brightness = Some(brightness);
                }
                Some("Per-key colours saved to keyboard".into())
            }
            Job::SetSetting(change) => {
                anyhow::ensure!(self.online == Some(true), "Keyboard is not online");
                let block = self.ensure()?.set_setting(change)?;
                *self.core.settings.lock().unwrap() = Settings::read(&block);
                None
            }
            Job::FactoryReset(session) => {
                anyhow::ensure!(
                    *self.core.reset_session.lock().unwrap() == Some(session),
                    "Keyboard connection changed - reset cancelled"
                );
                let result = self
                    .kb
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("Keyboard disconnected - reset cancelled"))?
                    .factory_reset();
                if let Err(error) = result {
                    self.disconnect();
                    let _ = self.tx.send(Event::Lost(
                        "Keyboard connection could not be verified".into(),
                    ));
                    return Err(error);
                }
                self.disconnect();
                let mut live = self.core.live.lock().unwrap();
                live.fx = None;
                live.audio = false;
                drop(live);
                self.resetting = Some(Instant::now() + Duration::from_secs(5));
                None
            }
            Job::Frame(c) => {
                // studio FX, audio sync and LIVE painting: dimmed here by the brightness setting
                let b = self.core.live.lock().unwrap().brightness;
                if !self.held {
                    self.ensure()?.set_effect(p::CUSTOM_MODE, 9, 0, 0)?;
                    self.held = true;
                }
                self.ensure()?.realtime(&dim(&c, b))?;
                None
            }
            Job::LoadKeymap(layer) => {
                self.keymap(layer)?;
                self.send_keymap(layer);
                None
            }
            Job::Remap { layer, led, code } => {
                self.guard()?;
                let mut km = self.keymap(layer)?;
                km[led as usize * 4..led as usize * 4 + 4].copy_from_slice(&code);
                self.ensure()?.set_keymap(layer, &km)?;
                self.keymaps.insert(layer, km);
                self.send_keymap(layer);
                Some(format!("Key remapped to {}", p::describe_code(code)))
            }
            Job::RestoreKeymap(layer) => {
                self.keymap(layer)?;
                let b = self.backups[&layer].clone();
                self.ensure()?.set_keymap(layer, &b)?;
                self.keymaps.insert(layer, b);
                self.send_keymap(layer);
                Some("Default key layout restored".into())
            }
            Job::Macros { buf, assign, clear } => {
                if self.ensure()?.link != Link::Wired {
                    anyhow::bail!("macros need the USB-C cable");
                }
                self.guard()?;
                let mut km = self.keymap(0)?;
                let backup = self.backups[&0].clone();
                for led in clear {
                    let i = led as usize * 4;
                    km[i..i + 4].copy_from_slice(&backup[i..i + 4]);
                }
                for (led, code) in &assign {
                    let i = *led as usize * 4;
                    km[i..i + 4].copy_from_slice(code);
                }
                // same order as OemDrv ApplySetting: key matrix first, then the macro area
                let kb = self.ensure()?;
                kb.set_keymap(0, &km)?;
                std::thread::sleep(std::time::Duration::from_millis(50));
                if !buf.is_empty() {
                    kb.set_macros(&buf)?;
                }
                self.keymaps.insert(0, km);
                self.send_keymap(0);
                Some(format!("{} macro(s) on the keyboard", assign.len()))
            }
            Job::Flush(done) => {
                let _ = done.send(());
                None
            }
            Job::ApplyProfile(pr) => {
                self.held = false;
                {
                    let kb = self.ensure()?;
                    if pr.mode == p::CUSTOM_MODE {
                        kb.set_per_key(&pr.colours, pr.brightness)?;
                    } else {
                        kb.set_effect(pr.mode, pr.brightness, pr.speed, pr.colour)?;
                    }
                    let block = kb.set_setting(Change::Sleep(pr.sleep))?;
                    *self.core.settings.lock().unwrap() = Settings::read(&block);
                }
                // key layouts (missing = factory) + macros; skipped if the layout read looks wrong.
                // Macros only take over the cable: on 2.4G the macro keys are left as they are.
                let wired = self.ensure()?.link == Link::Wired;
                if self.guard().is_ok() {
                    for layer in 0..2u8 {
                        self.keymap(layer)?;
                        let mut want = pr.keymaps[layer as usize].clone().unwrap_or_else(|| self.backups[&layer].clone());
                        if layer == 0 {
                            for (i, m) in pr.macros.iter().enumerate() {
                                let o = m.led as usize * 4;
                                let code = if wired { macros::key_code(i as u8, 1) } else { self.keymaps[&0][o..o + 4].try_into().unwrap() };
                                want[o..o + 4].copy_from_slice(&code);
                            }
                        }
                        if self.keymaps[&layer] != want {
                            self.ensure()?.set_keymap(layer, &want)?;
                            self.keymaps.insert(layer, want);
                        }
                        self.send_keymap(layer);
                    }
                    if wired && !pr.macros.is_empty() {
                        self.ensure()?.set_macros(&macro_buffer(&pr.macros))?;
                    }
                }
                Some(format!("Profile \"{}\" applied", pr.name))
            }
        })
    }
}

pub fn macro_buffer(defs: &[MacroDef]) -> Vec<u8> {
    let m: Vec<macros::Macro> = defs.iter().map(|d| macros::Macro { name: d.name.clone(), events: macros::text_events(&d.text, 12) }).collect();
    macros::buffer(&m)
}

pub fn mode_index(hw: u8) -> usize {
    EFFECTS.iter().position(|e| e.0 == hw).or_else(|| EFFECTS.iter().position(|e| e.0 == p::CUSTOM_MODE)).unwrap()
}

pub fn fx_by_name(n: &str) -> Option<Fx> {
    fx::STUDIO.iter().find(|f| format!("{:?}", f.0) == n).map(|f| f.0)
}

pub fn sleep_label(s: u16) -> String {
    match s {
        0 => "Never".into(),
        s if s.is_multiple_of(60) => format!("{} min", s / 60),
        s => format!("{} min {} s", s / 60, s % 60),
    }
}

impl Worker {
    /// Runs a job; a failure only counts as "disconnected" if the device node is really gone.
    fn handle(&mut self, job: Job) -> Option<Event> {
        match job {
            Job::SetSetting(change) => {
                return match self.run(job) {
                    Ok(_) => Some(Event::SettingFinished(Some(change))),
                    Err(error) => {
                        let _ = self.tx.send(Event::SettingFinished(None));
                        Some(Event::Error(format!("{error:#}")))
                    }
                };
            }
            Job::FactoryReset(_) => {
                return Some(Event::ResetFinished(
                    self.run(job).map(|_| ()).map_err(|e| format!("{e:#}")),
                ));
            }
            _ => {}
        }
        let was_connected = self.kb.is_some();
        match self.run(job) {
            Ok(Some(m)) => Some(Event::Done(m)),
            Ok(None) => Some(Event::Done(String::new())),
            Err(e) => Some(self.fail(e, was_connected)),
        }
    }

    fn fail(&mut self, e: anyhow::Error, was_connected: bool) -> Event {
        let msg = format!("{e:#}");
        let present = self.kb.as_ref().is_some_and(|k| k.dry() || F75::detect().iter().any(|(l, p)| *l == k.link && *p == k.path));
        if !present || !was_connected {
            self.disconnect();
            Event::Lost(msg)
        } else {
            Event::Error(msg)
        }
    }
}

impl Worker {
    fn new(core: Arc<Core>, tx: Sender<Event>) -> Self {
        Self {
            core,
            last_online: Instant::now(),
            online: None,
            session: 0,
            resetting: None,
            kb: None,
            keymaps: HashMap::new(),
            backups: HashMap::new(),
            tx,
            held: false,
        }
    }
}

fn worker(rx: Receiver<Job>, tx: Sender<Event>, core: Arc<Core>) {
    let mut w = Worker::new(core.clone(), tx.clone());
    let (mut engine, mut audio, t0) = (fx::Engine::default(), None::<Audio>, Instant::now());
    let mut last_poll = Instant::now() - Duration::from_secs(60);
    let (mut music, mut music_at, mut music_frame) = (crate::aula_music::Music::default(), Instant::now(), [[0u8; 3]; LED_COUNT]);
    loop {
        let live = *core.live.lock().unwrap();
        let wired = w.kb.as_ref().is_some_and(|k| k.link == Link::Wired);
        let streaming = live.streaming() && wired;
        if live.audio && wired {
            audio.get_or_insert_with(Audio::start);
        } else {
            audio = None;
        }
        match rx.recv_timeout(Duration::from_millis(if streaming { 12 } else { 250 })) {
            Ok(job) => {
                if let Some(ev) = w.handle(job) {
                    let _ = tx.send(ev);
                }
                core.repaint();
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if streaming {
            let snap = match &audio {
                Some(a) => {
                    if let Some(e) = a.levels.lock().unwrap().error.take() {
                        core.live.lock().unwrap().audio = false;
                        let _ = tx.send(Event::Error(e));
                    }
                    a.snapshot(live.sensitivity)
                }
                None => Snapshot::default(),
            };
            let pal = fx::PALETTES[live.palette as usize % fx::PALETTES.len()].1;
            let f = if live.audio { live.audio_fx } else { live.fx.unwrap_or(Fx::Wave) };
            let c = match (f, &audio) {
                (Fx::Aula(m), Some(a)) => {
                    // AULA steps its music modes every 20 ms
                    let step = Duration::from_millis(20);
                    if music_at.elapsed() > step * 5 {
                        music_at = Instant::now() - step;
                    }
                    if music_at.elapsed() >= step {
                        let lv = a.music(live.gain, live.smooth);
                        while music_at.elapsed() >= step {
                            music_at += step;
                            music_frame = music.step(m, &lv, live.colorful, crate::aula_music::colorref(live.colour));
                        }
                    }
                    music_frame
                }
                _ => engine.frame(f, pal, t0.elapsed().as_secs_f32(), live.speed, &snap),
            };
            *core.frame.lock().unwrap() = Some(c);
            *core.bands.lock().unwrap() = snap.bands;
            let was = w.kb.is_some();
            if let Err(e) = w.run(Job::Frame(Box::new(c))) {
                let _ = tx.send(w.fail(e, was));
            }
            core.repaint();
        } else {
            if core.frame.lock().unwrap().take().is_some() {
                core.repaint();
            }
            // connection / hotplug / online check, without telling the UI a job finished
            if last_poll.elapsed() > Duration::from_secs(if w.kb.is_some() { 3 } else { 2 }) {
                last_poll = Instant::now();
                let was = w.kb.is_some();
                if let Err(e) = w.run(Job::Poll) {
                    let _ = tx.send(w.fail(e, was));
                }
                core.repaint();
            }
        }
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;

    fn receiver_worker(online: bool) -> Worker {
        let (core, _, tx) = Core::new(&Profile::new("test"));
        let mut worker = Worker::new(core, tx);
        worker.kb = Some(if online {
            F75::dry_run(Link::Receiver)
        } else {
            F75::dry_offline_receiver()
        });
        worker.report_online(Link::Receiver, online);
        worker
    }

    #[test]
    fn reset_refuses_a_stale_confirmation_and_checks_again_before_sending() {
        let mut worker = receiver_worker(true);
        let confirmed = worker.session;
        worker.report_online(Link::Receiver, false);
        worker.report_online(Link::Receiver, true);
        assert!(worker.run(Job::FactoryReset(confirmed)).is_err());
        assert!(worker.kb.is_some());
        assert!(worker.resetting.is_none());

        let mut worker = receiver_worker(false);
        worker.report_online(Link::Receiver, true); // Receiver went offline since last poll.
        assert!(worker.run(Job::FactoryReset(worker.session)).is_err());
        assert!(worker.resetting.is_none());
        assert!(worker.core.reset_session.lock().unwrap().is_none());
    }

    #[test]
    fn reset_clears_caches_and_discards_queued_writes_while_rebooting() -> anyhow::Result<()> {
        let mut worker = receiver_worker(true);
        worker.keymaps.insert(0, vec![0]);
        worker.backups.insert(0, vec![0]);
        worker.core.live.lock().unwrap().fx = Some(Fx::Wave);
        worker.run(Job::FactoryReset(worker.session))?;
        assert!(worker.resetting.is_some());
        assert!(worker.kb.is_none());
        assert!(worker.keymaps.is_empty());
        assert!(worker.backups.is_empty());
        assert!(!worker.core.live.lock().unwrap().streaming());
        assert!(worker.core.reset_session.lock().unwrap().is_none());
        assert!(worker.run(Job::SetSetting(Change::WinLock(true))).is_err());
        assert!(
            worker
                .run(Job::ApplyProfile(Box::new(Profile::new("test"))))
                .is_err()
        );
        assert!(worker.kb.is_none());
        Ok(())
    }
}
