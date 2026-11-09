mod audio;
mod autostart;
mod bridge;
mod hid;
mod hotkeys;
mod instance;
mod midi;
mod runtime;
mod tray;

use std::time::{Duration, Instant};

use runtime::Event;

/// Prints what the backends can see, without starting the UI.
fn list_devices() {
    let (events, rx) = std::sync::mpsc::channel();
    let _audio = audio::start(events.clone());
    let _midi = midi::start(events.clone());
    hid::start(events);
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut outputs = Vec::new();
    let mut apps = Vec::new();
    let mut sources = std::collections::BTreeMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Audio {
                outputs: o,
                apps: a,
            }) => (outputs, apps) = (o, a),
            Ok(Event::Source {
                key,
                name,
                connected,
            }) => {
                sources.insert(key, (name, connected));
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    println!("Outputs:");
    for output in outputs {
        println!(
            "  {:>3.0}%  {}  [{}]",
            output.volume * 100.0,
            output.name,
            output.id
        );
    }
    println!("Apps:");
    for app in apps {
        println!(
            "  {:>3.0}%  {}  [{}]",
            app.volume * 100.0,
            app.name,
            app.key
        );
    }
    println!("Fader sources:");
    for (key, (name, connected)) in sources {
        println!(
            "  {}  {name}  [{key}]",
            if connected { "online " } else { "offline" }
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--list-devices") {
        list_devices();
        return;
    }

    let (events, rx) = runtime::event_channel();
    let _instance = match instance::claim(events.clone()) {
        instance::Claim::AlreadyRunning => return,
        instance::Claim::Primary(connection) => connection,
    };

    let _ = bridge::START_HIDDEN.set(args.iter().any(|a| a == "--minimized"));
    *bridge::PENDING.lock().unwrap() = Some((events, rx));
    let code = bridge::run_app();
    runtime::shutdown();
    std::process::exit(code);
}
