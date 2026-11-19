//! Device thread: runs every keyboard job in order and, over USB-C, renders and streams the live
//! effects (studio FX / audio sync) itself, so they keep running with the window closed.
use crate::audio::{Audio, Snapshot, BANDS};
use crate::device::F75;
use crate::fx::{self, Fx};
use crate::macros;
use crate::protocol::{self as p, Link, EFFECTS, LED_COUNT};
use crate::state::{self, MacroDef, Profile};
use crate::window_state::{WindowAction, WindowState};
use eframe::egui;
use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
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
    pub ctx: Mutex<Option<egui::Context>>,
    pub profile_changed: AtomicBool,
    pub tray_refresh: Mutex<Option<Box<dyn Fn() + Send>>>,
    window: Mutex<WindowState>,
    window_changed: Condvar,
}

impl Core {
    pub fn start(pr: &Profile) -> Arc<Core> {
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
            ctx: Mutex::new(None),
            profile_changed: AtomicBool::new(false),
            tray_refresh: Mutex::new(None),
            window: Mutex::new(WindowState::default()),
            window_changed: Condvar::new(),
        });
        let c = core.clone();
        std::thread::spawn(move || worker(jr, et, c));
        core
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

    pub fn window_visible(&self) -> bool {
        self.window.lock().unwrap().is_visible()
    }

    fn request_window_change(&self, change: impl FnOnce(&mut WindowState)) {
        change(&mut self.window.lock().unwrap());
        self.window_changed.notify_one();
        self.repaint();
    }

    pub fn request_show(&self) {
        self.request_window_change(|window| window.set_visible(true));
    }

    pub fn request_hide(&self) {
        self.request_window_change(|window| window.set_visible(false));
    }

    pub fn request_toggle(&self) {
        self.request_window_change(WindowState::toggle);
    }

    pub fn request_quit(&self) {
        self.request_window_change(WindowState::quit);
    }

    pub fn window_frame(&self, close_requested: bool) -> WindowAction {
        let (action, became_visible) = {
            let mut window = self.window.lock().unwrap();
            let was_visible = window.is_visible();
            let action = window.frame(close_requested);
            (action, !was_visible && window.is_visible())
        };
        if became_visible {
            self.refresh_tray();
        }
        action
    }

    pub fn window_closed(&self) {
        self.window.lock().unwrap().closed();
        self.refresh_tray();
    }

    /// Wait for an open request, or return false if the process should quit.
    pub fn wait_for_window(&self) -> bool {
        let mut window = self.window.lock().unwrap();
        loop {
            if window.is_quitting() {
                return false;
            }
            if window.begin_open() {
                return true;
            }
            window = self.window_changed.wait(window).unwrap();
        }
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
    Sleep(u16),
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
    Block { mode: usize, brightness: u8, speed: u8, colour: u8, sleep: u16 },
    Keymap { layer: u8, codes: Vec<[u8; 4]>, backup: Vec<[u8; 4]> },
    Done(String),
    Error(String),
}

struct Worker {
    core: Arc<Core>,
    last_online: std::time::Instant,
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
                Ok(_) => F75::dry_run(Link::Receiver),
                Err(_) => F75::open()?,
            };
            let _ = self.tx.send(Event::Connected(kb.link));
            *self.core.link.lock().unwrap() = Some(kb.link);
            self.keymaps.clear();
            self.backups.clear();
            self.held = false;
            // while streaming, the stored brightness is the held 9, not the user's setting
            let streaming = self.core.live.lock().unwrap().streaming();
            if let Ok(online) = kb.online() {
                let _ = self.tx.send(Event::Online(online));
                if online && !kb.dry() && !streaming {
                    if let Ok(b) = kb.get_led_block() {
                        if let Some((mode, brightness, speed, colour)) = p::effect_from_block(&b) {
                            let hw = b[p::SLEEP_OFFSET];
                            let sleep = p::SLEEP.iter().find(|s| s.1 == hw).map(|s| s.0).unwrap_or(300);
                            let _ = self.tx.send(Event::Block { mode, brightness, speed, colour, sleep });
                        }
                    }
                }
            }
            self.kb = Some(kb);
        }
        Ok(self.kb.as_mut().unwrap())
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
        Ok(match job {
            Job::Poll => {
                // Hotplug: Bluetooth/cable have no I/O on poll, so check the device list itself.
                // Drop the handle if its node vanished or a preferred link (cable > 2.4G > BT) appeared.
                if let Some(kb) = self.kb.as_ref().filter(|k| !k.dry()) {
                    let present = F75::detect();
                    let still_here = present.iter().any(|(l, p)| *l == kb.link && *p == kb.path);
                    let best = present.first().map(|(l, _)| *l);
                    if !still_here || best != Some(kb.link) {
                        self.kb = None;
                        *self.core.link.lock().unwrap() = None;
                        let _ = self.tx.send(Event::Lost(if still_here { "switching connection…".into() } else { "keyboard disconnected".into() }));
                    }
                }
                // radio traffic can swallow key releases on 2.4G (auto-repeat), so ask rarely
                let fresh = self.kb.is_none();
                let due = self.last_online.elapsed() > std::time::Duration::from_secs(20);
                let kb = self.ensure()?;
                if fresh || due {
                    let online = kb.online()?;
                    self.last_online = std::time::Instant::now();
                    let _ = self.tx.send(Event::Online(online));
                }
                None
            }
            Job::Effect { mode, brightness, speed, colour } => {
                self.held = false;
                self.ensure()?.set_effect(mode, brightness, speed, colour)?;
                Some("Lighting updated".into())
            }
            Job::PerKey { colours, brightness } => {
                self.held = false;
                self.ensure()?.set_per_key(&colours, brightness)?;
                Some("Per-key colours saved to keyboard".into())
            }
            Job::Sleep(s) => {
                self.ensure()?.set_sleep(s)?;
                Some(if s == 0 { "Sleep disabled - lights stay on".into() } else { format!("Sleep after {}", sleep_label(s)) })
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
                    kb.set_sleep(pr.sleep)?;
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
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{:.1} min", s as f32 / 60.0),
    }
}

impl Worker {
    /// Runs a job; a failure only counts as "disconnected" if the device node is really gone.
    fn handle(&mut self, job: Job) -> Option<Event> {
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
            self.kb = None;
            *self.core.link.lock().unwrap() = None;
            Event::Lost(msg)
        } else {
            Event::Error(msg)
        }
    }
}

fn worker(rx: Receiver<Job>, tx: Sender<Event>, core: Arc<Core>) {
    let mut w = Worker {
        core: core.clone(),
        last_online: Instant::now(),
        kb: None,
        keymaps: HashMap::new(),
        backups: HashMap::new(),
        tx: tx.clone(),
        held: false,
    };
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
