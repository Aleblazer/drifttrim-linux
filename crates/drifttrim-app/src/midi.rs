//! Fader input from the ALSA sequencer, with hotplug through its announce port.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::io::{PipeReader, PipeWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use alsa::Direction;
use alsa::PollDescriptors;
use alsa::poll::{poll, pollfd};
use alsa::seq::{
    Addr, ClientIter, EvCtrl, EventType, PortCap, PortInfo, PortIter, PortSubscribe, PortType, Seq,
};
use drifttrim_core::midi::CcDecoder;
use drifttrim_core::source::{MidiMode, midi_key};

use crate::runtime::{Event, clock};

const SYSTEM_CLIENT: i32 = 0;
const SYSTEM_ANNOUNCE_PORT: i32 = 1;

/// Handle for telling the MIDI thread which sources to listen to.
pub struct MidiHandle {
    wanted: Arc<Mutex<HashSet<String>>>,
    wake: Mutex<PipeWriter>,
}

impl MidiHandle {
    pub fn set_wanted(&self, keys: &[String]) {
        *self.wanted.lock().unwrap() = keys
            .iter()
            .filter(|k| k.starts_with("midi:"))
            .cloned()
            .collect();
        let _ = self.wake.lock().unwrap().write_all(&[1]);
    }
}

pub fn start(events: Sender<Event>) -> Option<MidiHandle> {
    let (reader, writer) = std::io::pipe().ok()?;
    let wanted = Arc::new(Mutex::new(HashSet::new()));
    let shared = Arc::clone(&wanted);
    thread::Builder::new()
        .name("midi".into())
        .spawn(move || {
            if let Err(err) = run(events, shared, reader) {
                eprintln!("drifttrim: MIDI input stopped: {err}");
            }
        })
        .ok()?;
    Some(MidiHandle {
        wanted,
        wake: Mutex::new(writer),
    })
}

struct Endpoint {
    addr: Addr,
    name: String,
    keys: [(MidiMode, String); 3],
}

struct Open {
    addr: Addr,
    decoder: CcDecoder,
    keys: [(MidiMode, String); 3],
}

fn keys_for(id: &str) -> [(MidiMode, String); 3] {
    MidiMode::ALL.map(|mode| (mode, midi_key(id, mode)))
}

fn scan(seq: &Seq, own_client: i32) -> HashMap<String, Endpoint> {
    let mut found = HashMap::new();
    let mut seen_names: HashMap<String, u32> = HashMap::new();
    for client in ClientIter::new(seq) {
        let number = client.get_client();
        let Ok(client_name) = client.get_name() else {
            continue;
        };
        if number == SYSTEM_CLIENT
            || number == own_client
            || client_name == "Midi Through"
            || client_name.starts_with("PipeWire-")
        {
            continue;
        }
        for port in PortIter::new(seq, number) {
            let caps = port.get_capability();
            if !caps.contains(PortCap::READ | PortCap::SUBS_READ)
                || caps.contains(PortCap::NO_EXPORT)
            {
                continue;
            }
            if !port
                .get_type()
                .intersects(PortType::MIDI_GENERIC | PortType::APPLICATION)
            {
                continue;
            }
            let Ok(port_name) = port.get_name() else {
                continue;
            };
            let port_name = port_name.trim();
            let base = format!("{client_name}:{port_name}");
            // Client numbers change between plugs, so identity uses names. Two
            // identical controllers are told apart by their order.
            let ordinal = seen_names
                .entry(base.clone())
                .and_modify(|n| *n += 1)
                .or_insert(0);
            let id = if *ordinal == 0 {
                base
            } else {
                format!("{base}#{ordinal}")
            };
            found.insert(
                id.clone(),
                Endpoint {
                    addr: port.addr(),
                    name: port_name.to_owned(),
                    keys: keys_for(&id),
                },
            );
        }
    }
    found
}

fn subscribe(seq: &Seq, sender: Addr, dest: Addr) -> alsa::Result<()> {
    let sub = PortSubscribe::empty()?;
    sub.set_sender(sender);
    sub.set_dest(dest);
    seq.subscribe_port(&sub)
}

fn run(
    events: Sender<Event>,
    wanted: Arc<Mutex<HashSet<String>>>,
    mut wake: PipeReader,
) -> alsa::Result<()> {
    let seq = Seq::open(None, Some(Direction::Capture), true)?;
    let name = CString::new("DriftTrim").unwrap();
    seq.set_client_name(&name)?;
    let mut info = PortInfo::empty()?;
    info.set_capability(PortCap::WRITE | PortCap::SUBS_WRITE);
    info.set_type(PortType::MIDI_GENERIC | PortType::APPLICATION);
    info.set_name(&CString::new("Fader input").unwrap());
    seq.create_port(&info)?;
    let own = Addr {
        client: seq.client_id()?,
        port: info.get_port(),
    };
    subscribe(
        &seq,
        Addr {
            client: SYSTEM_CLIENT,
            port: SYSTEM_ANNOUNCE_PORT,
        },
        own,
    )?;

    let mut open: HashMap<String, Open> = HashMap::new();
    let mut known: HashMap<String, String> = HashMap::new();
    let mut by_addr: HashMap<(i32, i32), String> = HashMap::new();
    let mut needs_scan = true;

    loop {
        if needs_scan {
            needs_scan = false;
            let endpoints = scan(&seq, own.client);
            let want = wanted.lock().unwrap().clone();

            for (id, old_name) in known.iter() {
                if !endpoints.contains_key(id) {
                    for (_, key) in keys_for(id) {
                        let _ = events.send(Event::Source {
                            key,
                            name: old_name.clone(),
                            connected: false,
                        });
                    }
                }
            }
            open.retain(|id, input| {
                let keep = endpoints.get(id).is_some_and(|e| {
                    e.addr == input.addr && input.keys.iter().any(|(_, key)| want.contains(key))
                });
                if !keep && endpoints.contains_key(id) {
                    let _ = seq.unsubscribe_port(input.addr, own);
                }
                keep
            });
            by_addr.clear();
            known.clear();
            for (id, endpoint) in &endpoints {
                known.insert(id.clone(), endpoint.name.clone());
                for (_, key) in &endpoint.keys {
                    let _ = events.send(Event::Source {
                        key: key.clone(),
                        name: endpoint.name.clone(),
                        connected: true,
                    });
                }
                by_addr.insert((endpoint.addr.client, endpoint.addr.port), id.clone());
                let needed = endpoint.keys.iter().any(|(_, key)| want.contains(key));
                if needed && !open.contains_key(id) && subscribe(&seq, endpoint.addr, own).is_ok() {
                    open.insert(
                        id.clone(),
                        Open {
                            addr: endpoint.addr,
                            decoder: CcDecoder::default(),
                            keys: endpoint.keys.clone(),
                        },
                    );
                }
            }
        }

        let descriptors = (&seq, Some(Direction::Capture));
        let mut fds: Vec<pollfd> = vec![pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let start = fds.len();
        fds.resize(
            start + PollDescriptors::count(&descriptors),
            pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
        );
        PollDescriptors::fill(&descriptors, &mut fds[start..])?;
        poll(&mut fds, -1)?;
        if fds[0].revents != 0 {
            let mut buffer = [0u8; 64];
            let _ = wake.read(&mut buffer);
            needs_scan = true;
        }

        let want = wanted.lock().unwrap().clone();
        let mut input = seq.input();
        loop {
            match input.event_input_pending(true) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let event = match input.event_input() {
                Ok(event) => event,
                Err(_) => break,
            };
            match event.get_type() {
                EventType::Controller => {
                    let Some(data) = event.get_data::<EvCtrl>() else {
                        continue;
                    };
                    if data.param >= 120 || data.value < 0 || data.value > 127 {
                        continue;
                    }
                    let source = event.get_source();
                    let Some(id) = by_addr.get(&(source.client, source.port)) else {
                        continue;
                    };
                    let Some(port) = open.get_mut(id) else {
                        continue;
                    };
                    let now = clock();
                    let (channel, controller, value) =
                        (data.channel, data.param as u8, data.value as u8);
                    let completed = port.decoder.accept14(channel, controller, value, now);
                    for (mode, key) in &port.keys {
                        if !want.contains(key) {
                            continue;
                        }
                        let sample = match mode {
                            MidiMode::Cc7 => {
                                Some((channel as usize * 128 + controller as usize, value as i32))
                            }
                            MidiMode::Cc14 | MidiMode::Qmk => completed,
                        };
                        if let Some((axis, raw)) = sample {
                            let _ = events.send(Event::Sample {
                                key: key.clone(),
                                axis,
                                raw,
                            });
                        }
                    }
                }
                EventType::PortStart
                | EventType::PortExit
                | EventType::PortChange
                | EventType::ClientExit => {
                    needs_scan = true;
                }
                _ => {}
            }
        }
    }
}
