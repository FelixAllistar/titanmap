//! Desktop titanmap: a hidden, borderless, always-on-top window over the game.
//!
//! - The toggle hotkey (default Alt+M) shows or hides it. Showing it first takes a screenshot
//!   and locates you in the background, so the map opens where the in-game map was.
//! - Watch mode (default on, key M): when you press M in the game, titanmap waits for the map
//!   to open, takes one screenshot, and quietly updates your position. If no map is on screen
//!   the screenshot is dropped.
//!
//! Settings live in titanmap.json next to the executable (written on first run).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use titanmap_core::capture;
use titanmap_core::locate::{Fix, Locator};
use titanmap_core::{Cam, Player, State};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct Config {
    /// Global hotkey that shows/hides the overlay (Rune accelerator syntax).
    toggle: String,
    /// Watch this key (the game's map key) and locate after each press. Empty turns it off.
    watch_key: String,
    /// How long the in-game map takes to open after the key, in milliseconds.
    watch_delay_ms: u64,
    /// Match scores below this are treated as "no map on screen".
    min_score: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config { toggle: "alt+m".into(), watch_key: "M".into(), watch_delay_ms: 450, min_score: 0.55 }
    }
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())).unwrap_or_else(|| ".".into())
}

fn load_config() -> Config {
    let path = exe_dir().join("titanmap.json");
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => {
            let c = Config::default();
            let _ = std::fs::write(&path, serde_json::to_string_pretty(&c).unwrap());
            c
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Why { Toggle, Watch, Button }

struct Job { shot: image::RgbaImage, prefer: String, why: Why }

thread_local! {
    /// The toggle action; global shortcut handlers run on this (the app) thread.
    static TOGGLE: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

fn primary_size() -> (u32, u32) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
        return (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32);
    }
    #[allow(unreachable_code)]
    (1920, 1080)
}

fn main() {
    let cfg = load_config();
    let (w, h) = primary_size();
    let options = rune::WindowOptions {
        borderless: true,
        always_on_top: true,
        visible: true,
        position: Some((0, 0)),
        physical_size: Some((w, h)),
        ..Default::default()
    };
    let app = rune::App::new(titanmap_core::spec()).window_options(options).keep_running().allow_unmodified_shortcuts();
    let handle = app.handle();
    let (tx, rx) = mpsc::channel::<Job>();
    let min_score = cfg.min_score;

    // Locator worker: builds the index on first use, then answers jobs in order.
    {
        let handle = handle.clone();
        std::thread::spawn(move || {
            let assets = titanmap_assets();
            let mut loc: Option<Locator> = None;
            while let Ok(mut job) = rx.recv() {
                // Only the newest screenshot matters.
                while let Ok(newer) = rx.try_recv() { job = newer; }
                let loc = loc.get_or_insert_with(|| {
                    let maps: Vec<_> = titanmap_core::data::MAP_IDS.iter().map(|id| titanmap_core::data::MapData::load(id)).collect();
                    Locator::new(&assets, &maps.iter().map(|m| (m.id.clone(), (m.image.w, m.image.h), m.image.levels)).collect::<Vec<_>>())
                });
                let fix = loc.locate(&job.shot, Some(&job.prefer));
                let why = job.why;
                handle.update(move |s: &mut State, _tx| apply(s, fix, why, min_score));
            }
        });
    }

    // Toggle: hide if showing; else screenshot first, then show.
    {
        let handle = handle.clone();
        let tx = tx.clone();
        let toggle = move || {
            if handle.is_visible() {
                handle.hide();
                return;
            }
            if let Some(shot) = capture::capture_foreground_monitor() {
                let _ = tx.send(Job { shot, prefer: String::new(), why: Why::Toggle });
            }
            handle.show();
            handle.focus();
        };
        TOGGLE.with(|t| *t.borrow_mut() = Some(Box::new(toggle)));
    }

    // Watch the game's map key without taking it from the game.
    if let Some(vk) = capture::vk_from_name(&cfg.watch_key) {
        let tx = tx.clone();
        let delay = cfg.watch_delay_ms;
        let me = std::process::id();
        std::thread::spawn(move || {
            let mut was = false;
            loop {
                std::thread::sleep(Duration::from_millis(25));
                let down = capture::key_down(vk);
                if down && !was && capture::foreground_pid() != me {
                    std::thread::sleep(Duration::from_millis(delay));
                    if let Some(shot) = capture::capture_foreground_monitor() {
                        let _ = tx.send(Job { shot, prefer: String::new(), why: Why::Watch });
                    }
                }
                was = down;
            }
        });
    }

    let hide_handle = handle.clone();
    let locate_tx = tx.clone();
    let locate_handle = handle.clone();
    let app = app
        .init(move || {
            let mut s = State::new();
            let h = hide_handle.clone();
            s.hide = Some(Box::new(move || h.hide()));
            let tx = locate_tx.clone();
            let h = locate_handle.clone();
            // The button can't screenshot the game under us: hide, wait a frame, capture, show.
            s.locate = Some(Box::new(move || {
                let tx = tx.clone();
                let h = h.clone();
                h.hide();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(250));
                    if let Some(shot) = capture::capture_foreground_monitor() {
                        let _ = tx.send(Job { shot, prefer: String::new(), why: Why::Button });
                    }
                    h.show();
                    h.focus();
                });
            }));
            s.status = format!("{} toggles · press {} in game to update your position", cfg.toggle, cfg.watch_key);
            s
        })
        .global_shortcut(&load_config().toggle, |_s, _tx| {
            TOGGLE.with(|t| if let Some(t) = t.borrow().as_ref() { t() });
        });
    app.run();
}

fn titanmap_assets() -> PathBuf {
    let dir = exe_dir().join("assets");
    if dir.join("maps").exists() { return dir; }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets")
}

fn apply(s: &mut State, fix: Option<Fix>, why: Why, min_score: f32) {
    let Some(fix) = fix.filter(|f| f.score >= min_score) else {
        if why != Why::Watch { s.status = "Couldn't find the in-game map in the screenshot — open it (M) first".into(); }
        return;
    };
    let Some(map) = s.maps.iter().position(|m| m.id == fix.map) else { return };
    s.player = Some(Player { map, at: fix.at });
    let g = s.maps[map].to_game(fix.at);
    s.status = format!("You: {} {:.0}, {:.0}  (match {:.2}, {} ms)", fix.map, g[0], g[1], fix.score, fix.millis);
    if why != Why::Watch || s.cur != map {
        s.select_map(map);
        // Match the in-game view: same centre and zoom as the map we just saw.
        let zoom = if why == Why::Watch { s.cam().zoom } else { fix.zoom.clamp(0.05, 4.0) };
        s.set_cam(Cam { c: fix.at, zoom });
    }
}
