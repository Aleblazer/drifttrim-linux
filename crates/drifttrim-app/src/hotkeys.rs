//! Global shortcuts through KDE's KGlobalAccel service over D-Bus.
//!
//! Each virtual fader registers Volume Up, Volume Down and Mute actions. The
//! user binds keys in System Settings, and KDE keeps those bindings.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use drifttrim_core::engine::HotkeyAction;
use zbus::blocking::{Connection, Proxy};

use crate::runtime::Event;

const COMPONENT: &str = "drifttrim";
const COMPONENT_NAME: &str = "DriftTrim";
const SERVICE: &str = "org.kde.kglobalaccel";
/// KGlobalAccel flag: the action is available in this session.
const SET_PRESENT: u32 = 2;
const REPEAT_DELAY: Duration = Duration::from_millis(400);
const REPEAT_INTERVAL: Duration = Duration::from_millis(100);
/// Stops a repeat if the release signal is ever lost.
const REPEAT_LIMIT: Duration = Duration::from_secs(5);

const ACTIONS: [(&str, &str, HotkeyAction); 3] = [
    ("up", "Volume Up", HotkeyAction::Up),
    ("down", "Volume Down", HotkeyAction::Down),
    ("mute", "Mute", HotkeyAction::Mute),
];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HotkeyFader {
    pub id: String,
    pub label: String,
}

pub struct HotkeysHandle(Sender<Vec<HotkeyFader>>);

impl HotkeysHandle {
    pub fn update(&self, faders: Vec<HotkeyFader>) {
        let _ = self.0.send(faders);
    }
}

fn action_id(fader: &HotkeyFader, unique: &str, friendly: &str) -> Vec<String> {
    vec![
        COMPONENT.to_owned(),
        format!("{}:{unique}", fader.id),
        COMPONENT_NAME.to_owned(),
        format!("{}: {friendly}", fader.label),
    ]
}

fn parse_action(action: &str) -> Option<(String, HotkeyAction)> {
    let (id, unique) = action.rsplit_once(':')?;
    let (_, _, kind) = ACTIONS.iter().find(|(name, _, _)| *name == unique)?;
    Some((id.to_owned(), *kind))
}

pub fn start(events: Sender<Event>) -> HotkeysHandle {
    let (tx, rx) = mpsc::channel();
    let _ = thread::Builder::new()
        .name("hotkeys".into())
        .spawn(move || {
            if let Err(err) = run(events, rx) {
                eprintln!("drifttrim: global shortcuts unavailable: {err}");
            }
        });
    HotkeysHandle(tx)
}

fn register(proxy: &Proxy, fader: &HotkeyFader) -> zbus::Result<()> {
    for (unique, friendly, _) in ACTIONS {
        let id = action_id(fader, unique, friendly);
        proxy.call_method("doRegister", &(id.clone(),))?;
        // No default key: this marks the action present and keeps any binding.
        proxy.call_method("setShortcut", &(id, Vec::<i32>::new(), SET_PRESENT))?;
    }
    Ok(())
}

fn unregister(proxy: &Proxy, fader: &HotkeyFader) -> zbus::Result<()> {
    for (unique, friendly, _) in ACTIONS {
        proxy.call_method("unRegister", &(action_id(fader, unique, friendly),))?;
    }
    Ok(())
}

fn run(events: Sender<Event>, updates: Receiver<Vec<HotkeyFader>>) -> zbus::Result<()> {
    let connection = Connection::session()?;
    let accel = Proxy::new(
        &connection,
        SERVICE,
        "/kglobalaccel",
        "org.kde.KGlobalAccel",
    )?;

    let (press_tx, press_rx) = mpsc::channel::<(String, bool)>();
    spawn_listener(&connection, "globalShortcutPressed", true, press_tx.clone())?;
    spawn_listener(&connection, "globalShortcutReleased", false, press_tx)?;
    spawn_repeater(press_rx, events);

    let mut registered: HashSet<HotkeyFader> = HashSet::new();
    for faders in updates {
        let next: HashSet<HotkeyFader> = faders.into_iter().collect();
        let added: HashMap<&str, &HotkeyFader> = next.iter().map(|f| (f.id.as_str(), f)).collect();
        for gone in registered
            .iter()
            .filter(|f| !added.contains_key(f.id.as_str()))
        {
            if let Err(err) = unregister(&accel, gone) {
                eprintln!("drifttrim: could not remove shortcut: {err}");
            }
        }
        for fader in next.difference(&registered) {
            if let Err(err) = register(&accel, fader) {
                eprintln!("drifttrim: could not register shortcut: {err}");
            }
        }
        registered = next;
    }
    Ok(())
}

fn spawn_listener(
    connection: &Connection,
    signal: &'static str,
    pressed: bool,
    out: Sender<(String, bool)>,
) -> zbus::Result<()> {
    let path = format!("/component/{COMPONENT}");
    let proxy = Proxy::new(connection, SERVICE, path, "org.kde.kglobalaccel.Component")?;
    let _ = thread::Builder::new()
        .name("hotkey-signals".into())
        .spawn(move || {
            let Ok(stream) = proxy.receive_signal(signal) else {
                return;
            };
            for message in stream {
                if let Ok((_, action, _)) = message.body().deserialize::<(String, String, i64)>()
                    && out.send((action, pressed)).is_err()
                {
                    return;
                }
            }
        });
    Ok(())
}

/// KGlobalAccel reports one press per hold, so holding a volume key is ramped
/// here. Repeating starts only once a release has been seen in this session: if
/// releases were not delivered, a single tap must never run the volume up.
fn spawn_repeater(presses: Receiver<(String, bool)>, events: Sender<Event>) {
    let _ = thread::Builder::new()
        .name("hotkey-repeat".into())
        .spawn(move || {
            let mut held: Option<(String, HotkeyAction, Instant, Instant)> = None;
            let mut releases_work = false;
            loop {
                let timeout = held
                    .as_ref()
                    .map_or(Duration::from_secs(3600), |(_, _, _, next)| {
                        next.saturating_duration_since(Instant::now())
                    });
                match presses.recv_timeout(timeout) {
                    Ok((action, true)) => {
                        let Some((id, kind)) = parse_action(&action) else {
                            continue;
                        };
                        let _ = events.send(Event::Hotkey {
                            id: id.clone(),
                            action: kind,
                        });
                        let now = Instant::now();
                        held = (releases_work && kind != HotkeyAction::Mute).then_some((
                            id,
                            kind,
                            now,
                            now + REPEAT_DELAY,
                        ));
                    }
                    Ok((_, false)) => {
                        releases_work = true;
                        held = None;
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        let Some((id, kind, started, next)) = &mut held else {
                            continue;
                        };
                        if started.elapsed() > REPEAT_LIMIT {
                            held = None;
                            continue;
                        }
                        *next = Instant::now() + REPEAT_INTERVAL;
                        let _ = events.send(Event::Hotkey {
                            id: id.clone(),
                            action: *kind,
                        });
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_names_round_trip() {
        let fader = HotkeyFader {
            id: "abc123".into(),
            label: "Game".into(),
        };
        for (unique, friendly, kind) in ACTIONS {
            let id = action_id(&fader, unique, friendly);
            assert_eq!(id[3], format!("Game: {friendly}"));
            assert_eq!(parse_action(&id[1]), Some(("abc123".to_owned(), kind)));
        }
    }

    #[test]
    fn unknown_actions_are_ignored() {
        assert_eq!(parse_action("abc:sideways"), None);
        assert_eq!(parse_action("nothing"), None);
    }
}
