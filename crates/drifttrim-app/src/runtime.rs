//! Connects the input backends, the engine, audio and the UI.
//!
//! One thread owns the engine and handles every event in order, so none of
//! the fader logic needs locks.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use drifttrim_core::engine::{
    AppInfo, AudioCommand, Command, Engine, HotkeyAction, OutputInfo, View,
};
use drifttrim_core::settings;
use serde::{Deserialize, Serialize};

use crate::audio::{self, AudioRequest};
use crate::hotkeys::{self, HotkeyFader};
use crate::{autostart, hid, midi, tray};

const IDLE_TICK: Duration = Duration::from_millis(500);
const FAST_TICK: Duration = Duration::from_millis(20);
const UI_INTERVAL_MS: u64 = 33;
const TRAY_INTERVAL_MS: u64 = 250;

pub fn clock() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// App-level requests from the UI that are not fader logic.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum AppCommand {
    SetAutostart { enabled: bool },
    OpenShortcuts,
    Quit,
}

pub enum Event {
    Audio {
        outputs: Vec<OutputInfo>,
        apps: Vec<AppInfo>,
    },
    External {
        key: String,
        is_endpoint: bool,
        volume: f32,
    },
    Sample {
        key: String,
        axis: usize,
        raw: i32,
    },
    Source {
        key: String,
        name: String,
        connected: bool,
    },
    Ui(Box<Command>),
    App(AppCommand),
    Hotkey {
        id: String,
        action: HotkeyAction,
    },
    ShowWindow,
    Quit,
    Shutdown,
}

/// Receives output destined for the Qt thread.
pub trait UiSink: Send {
    fn state(&self, json: String);
    fn show(&self);
    fn quit(&self);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UiState<'a> {
    #[serde(flatten)]
    view: &'a View,
    autostart: bool,
    version: &'static str,
    screenshot: Option<String>,
}

struct Running {
    events: Sender<Event>,
    thread: JoinHandle<()>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static SENDER: OnceLock<Sender<Event>> = OnceLock::new();

/// Forwards a JSON command from QML.
pub fn dispatch(json: &str) {
    let Some(events) = SENDER.get() else { return };
    let event = match serde_json::from_str::<Command>(json) {
        Ok(command) => Event::Ui(Box::new(command)),
        Err(engine_err) => match serde_json::from_str::<AppCommand>(json) {
            Ok(command) => Event::App(command),
            Err(_) => {
                eprintln!("drifttrim: ignoring bad UI command {json}: {engine_err}");
                return;
            }
        },
    };
    let _ = events.send(event);
}

/// Sender for other threads started before the runtime, such as D-Bus.
pub fn event_channel() -> (Sender<Event>, Receiver<Event>) {
    let channel = mpsc::channel();
    let _ = SENDER.set(channel.0.clone());
    channel
}

pub fn start(events: Sender<Event>, rx: Receiver<Event>, ui: Box<dyn UiSink>) {
    let thread_events = events.clone();
    let thread = thread::Builder::new()
        .name("engine".into())
        .spawn(move || run(thread_events, rx, ui))
        .expect("spawn engine thread");
    *RUNNING.lock().unwrap() = Some(Running { events, thread });
}

/// Saves settings and stops the engine thread after the Qt loop has ended.
pub fn shutdown() {
    if let Some(running) = RUNNING.lock().unwrap().take() {
        let _ = running.events.send(Event::Shutdown);
        let _ = running.thread.join();
    }
}

fn settings_path() -> Option<PathBuf> {
    settings::default_path()
}

fn run(events: Sender<Event>, rx: Receiver<Event>, ui: Box<dyn UiSink>) {
    let path = settings_path();
    let loaded = match path.as_deref().map(settings::load) {
        Some(Ok(loaded)) => loaded,
        Some(Err(err)) => {
            eprintln!("drifttrim: could not read settings: {err}");
            settings::Loaded {
                settings: Default::default(),
                recovered_to: None,
            }
        }
        None => settings::Loaded {
            settings: Default::default(),
            recovered_to: None,
        },
    };
    if let Some(backup) = &loaded.recovered_to {
        eprintln!(
            "drifttrim: damaged settings were moved to {}",
            backup.display()
        );
    }
    let mut engine = Engine::new(loaded.settings);

    let audio = audio::start(events.clone());
    let midi = midi::start(events.clone());
    hid::start(events.clone());
    let hotkeys = hotkeys::start(events.clone());
    let tray = tray::start(events.clone());

    let save = |settings: &settings::Settings| {
        if let Some(path) = &path
            && let Err(err) = settings::save(path, settings)
        {
            eprintln!("drifttrim: could not save settings: {err}");
        }
    };

    let screenshot = std::env::var("DRIFTTRIM_SCREENSHOT").ok();
    let mut wanted: Option<Vec<String>> = None;
    let mut hotkey_set: Option<HashSet<(String, String)>> = None;
    let mut last_json = String::new();
    let mut ui_dirty = true;
    let mut next_ui = 0u64;
    let mut next_tray = 0u64;
    let mut last_tooltip = String::new();

    loop {
        let timeout = if engine.needs_fast_tick() {
            FAST_TICK
        } else if ui_dirty {
            Duration::from_millis(UI_INTERVAL_MS)
        } else {
            IDLE_TICK
        };

        let first = rx.recv_timeout(timeout);
        let mut next = match first {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut handled = 0;
        let mut stop = false;
        while let Some(event) = next.take() {
            ui_dirty = true;
            let now = clock();
            match event {
                Event::Audio { outputs, apps } => engine.set_audio(outputs, apps, now),
                Event::External {
                    key,
                    is_endpoint,
                    volume,
                } => engine.external_change(&key, is_endpoint, volume, now),
                Event::Sample { key, axis, raw } => engine.sample(&key, axis, raw, now),
                Event::Source {
                    key,
                    name,
                    connected,
                } => engine.set_source(&key, &name, connected),
                Event::Ui(command) => engine.command(*command, now),
                Event::Hotkey { id, action } => engine.hotkey(&id, action, now),
                Event::App(AppCommand::SetAutostart { enabled }) => {
                    if let Err(err) = autostart::set(enabled) {
                        eprintln!("drifttrim: could not change autostart: {err}");
                    }
                    last_json.clear();
                }
                Event::App(AppCommand::OpenShortcuts) => {
                    let _ = std::process::Command::new("systemsettings")
                        .arg("kcm_keys")
                        .spawn();
                }
                Event::App(AppCommand::Quit) | Event::Quit => ui.quit(),
                Event::ShowWindow => ui.show(),
                Event::Shutdown => stop = true,
            }
            handled += 1;
            if stop || handled >= 512 {
                break;
            }
            next = rx.try_recv().ok();
        }
        if stop {
            break;
        }

        let now = clock();
        engine.tick(now);
        for command in engine.take_commands() {
            let request = match command {
                AudioCommand::Desired(desired) => AudioRequest::Desired(desired),
                AudioCommand::ResolveExternal {
                    key,
                    is_endpoint,
                    continue_driving,
                } => AudioRequest::Resolve {
                    key,
                    is_endpoint,
                    continue_driving,
                },
            };
            let _ = audio.send(request);
        }

        let keys = engine.wanted_sources();
        if wanted.as_ref() != Some(&keys) {
            if let Some(midi) = &midi {
                midi.set_wanted(&keys);
            }
            wanted = Some(keys);
        }

        let faders = engine.hotkey_faders();
        let set: HashSet<(String, String)> = faders.iter().cloned().collect();
        if hotkey_set.as_ref() != Some(&set) {
            hotkeys.update(
                faders
                    .into_iter()
                    .map(|(id, label)| HotkeyFader { id, label })
                    .collect(),
            );
            hotkey_set = Some(set);
        }

        if let Some(settings) = engine.take_save(now) {
            save(&settings);
        }

        if ui_dirty && now >= next_ui {
            ui_dirty = false;
            next_ui = now + UI_INTERVAL_MS;
            let view = engine.view();
            let state = UiState {
                view: &view,
                autostart: autostart::enabled(),
                version: env!("CARGO_PKG_VERSION"),
                screenshot: screenshot.clone(),
            };
            if let Ok(json) = serde_json::to_string(&state)
                && json != last_json
            {
                ui.state(json.clone());
                last_json = json;
            }
            if now >= next_tray {
                next_tray = now + TRAY_INTERVAL_MS;
                let levels: Vec<String> = view
                    .faders
                    .iter()
                    .map(|f| format!("{}%", f.value))
                    .collect();
                let tooltip = if levels.is_empty() {
                    "No faders configured".to_owned()
                } else {
                    levels.join(" · ")
                };
                if tooltip != last_tooltip {
                    if let Some(tray) = &tray {
                        tray.set_tooltip(tooltip.clone());
                    }
                    last_tooltip = tooltip;
                }
            }
        }
    }

    save(&engine.snapshot_settings());
    let _ = audio.send(AudioRequest::Stop);
}
