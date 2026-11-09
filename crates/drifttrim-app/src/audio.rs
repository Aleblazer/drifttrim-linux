//! Output devices and per-app volume through PipeWire's PulseAudio server.
//!
//! One worker thread owns every libpulse object. Callbacks run on libpulse's
//! own thread and only copy data into a channel, so the worker holds all the
//! state and takes the mainloop lock just for the calls it makes.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use drifttrim_core::engine::{AppInfo, DesiredVolumes, OutputInfo, SYSTEM_APP_KEY};
use drifttrim_core::expected::{ExpectedWrites, VOLUME_EPSILON};
use drifttrim_core::identity;
use libpulse_binding as pulse;
use pulse::callbacks::ListResult;
use pulse::context::introspect::{SinkInfo, SinkInputInfo};
use pulse::context::subscribe::{Facility, InterestMaskSet, Operation};
use pulse::context::{Context, FlagSet, State};
use pulse::mainloop::threaded::Mainloop;
use pulse::proplist::Proplist;
use pulse::volume::{ChannelVolumes, Volume};

use crate::runtime::{Event, clock};

/// System-wide ceiling on volume calls, after category fan-out.
const MIN_CALL_INTERVAL_MS: u64 = 25;
const MIN_SNAPSHOT_INTERVAL_MS: u64 = 50;
const EXPECTED_WINDOW_MS: u64 = 1000;
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub enum AudioRequest {
    Desired(DesiredVolumes),
    Resolve {
        key: String,
        is_endpoint: bool,
        continue_driving: bool,
    },
    Stop,
}

enum Msg {
    Request(AudioRequest),
    StateChanged,
    Sink(SinkData),
    Stream(StreamData),
    SinkGone(u32),
    StreamGone(u32),
    ListSink(u32),
    ListStream(u32),
}

struct SinkData {
    index: u32,
    name: String,
    description: String,
    volume: ChannelVolumes,
}

struct StreamData {
    index: u32,
    key: String,
    name: String,
    volume: ChannelVolumes,
}

struct Target {
    key: String,
    name: String,
    volume: ChannelVolumes,
    reported: f32,
    written: f32,
    expected: ExpectedWrites,
    was_desired: bool,
    hold: bool,
}

impl Target {
    fn new(key: String, name: String, volume: ChannelVolumes) -> Self {
        let reported = scalar(&volume);
        Self {
            key,
            name,
            volume,
            reported,
            written: reported,
            expected: ExpectedWrites::default(),
            was_desired: false,
            hold: false,
        }
    }
}

fn scalar(volume: &ChannelVolumes) -> f32 {
    (volume.max().0 as f32 / Volume::NORMAL.0 as f32).clamp(0.0, 1.0)
}

/// Sets the loudest channel to `value`, keeping the channel balance.
fn scaled(volume: &ChannelVolumes, value: f32) -> ChannelVolumes {
    let mut out = *volume;
    let level = (value as f64 * Volume::NORMAL.0 as f64).round() as u32;
    out.scale(Volume(level));
    out
}

fn stream_identity(props: &Proplist) -> Option<(String, String)> {
    let get = |key: &str| props.get_str(key);
    if get("media.role").as_deref() == Some("event") {
        return Some((SYSTEM_APP_KEY.to_owned(), "System Sounds".to_owned()));
    }
    let flatpak = get("application.flatpak.id").or_else(|| get("pipewire.access.portal.app_id"));
    let binary = get("application.process.binary");
    let name = get("application.name");
    let key = identity::app_key(flatpak.as_deref(), binary.as_deref(), name.as_deref())?;
    Some((
        key,
        identity::app_display_name(binary.as_deref(), name.as_deref()),
    ))
}

fn sink_message(info: &SinkInfo) -> Option<Msg> {
    Some(Msg::Sink(SinkData {
        index: info.index,
        name: info.name.as_deref()?.to_owned(),
        description: info
            .description
            .as_deref()
            .or(info.name.as_deref())?
            .to_owned(),
        volume: info.volume,
    }))
}

fn stream_message(info: &SinkInputInfo) -> Option<Msg> {
    let (key, name) = stream_identity(&info.proplist)?;
    Some(Msg::Stream(StreamData {
        index: info.index,
        key,
        name,
        volume: info.volume,
    }))
}

pub fn start(events: Sender<Event>) -> Sender<AudioRequest> {
    let (requests, request_rx) = mpsc::channel::<AudioRequest>();
    let (tx, rx) = mpsc::channel::<Msg>();
    let forward = tx.clone();
    thread::Builder::new()
        .name("audio-requests".into())
        .spawn(move || {
            for request in request_rx {
                let stop = matches!(request, AudioRequest::Stop);
                if forward.send(Msg::Request(request)).is_err() || stop {
                    return;
                }
            }
        })
        .expect("spawn audio request thread");
    thread::Builder::new()
        .name("audio".into())
        .spawn(move || worker(rx, tx, events))
        .expect("spawn audio thread");
    requests
}

fn worker(rx: Receiver<Msg>, tx: Sender<Msg>, events: Sender<Event>) {
    let mut desired = DesiredVolumes::default();
    loop {
        match session(&rx, &tx, &events, &mut desired) {
            Ended::Stop => return,
            Ended::Lost => {
                let _ = events.send(Event::Audio {
                    outputs: Vec::new(),
                    apps: Vec::new(),
                });
                let deadline = clock() + RECONNECT_DELAY.as_millis() as u64;
                // Keep following desired state while waiting for the server.
                while clock() < deadline {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(Msg::Request(AudioRequest::Stop))
                        | Err(RecvTimeoutError::Disconnected) => return,
                        Ok(Msg::Request(AudioRequest::Desired(next))) => desired = next,
                        _ => {}
                    }
                }
            }
        }
    }
}

enum Ended {
    Stop,
    Lost,
}

struct Session {
    sinks: BTreeMap<u32, Target>,
    streams: BTreeMap<u32, Target>,
    cursor: usize,
    next_call: u64,
    dirty: bool,
    next_snapshot: u64,
}

fn session(
    rx: &Receiver<Msg>,
    tx: &Sender<Msg>,
    events: &Sender<Event>,
    desired: &mut DesiredVolumes,
) -> Ended {
    let Some(mut mainloop) = Mainloop::new() else {
        return Ended::Lost;
    };
    let Some(mut context) = Context::new(&mainloop, "DriftTrim") else {
        return Ended::Lost;
    };

    let state_tx = tx.clone();
    context.set_state_callback(Some(Box::new(move || {
        let _ = state_tx.send(Msg::StateChanged);
    })));
    if context.connect(None, FlagSet::NOFLAGS, None).is_err() {
        return Ended::Lost;
    }
    mainloop.lock();
    if mainloop.start().is_err() {
        mainloop.unlock();
        return Ended::Lost;
    }
    mainloop.unlock();

    let ended = run_session(&mut mainloop, &mut context, rx, tx, events, desired);

    mainloop.lock();
    context.set_state_callback(None);
    context.set_subscribe_callback(None);
    context.disconnect();
    mainloop.unlock();
    mainloop.stop();
    ended
}

fn request_all(context: &Context, tx: &Sender<Msg>) {
    let introspect = context.introspect();
    let sink_tx = tx.clone();
    introspect.get_sink_info_list(move |result| {
        if let ListResult::Item(info) = result
            && let Some(msg) = sink_message(info)
        {
            let _ = sink_tx.send(msg);
        }
    });
    let stream_tx = tx.clone();
    introspect.get_sink_input_info_list(move |result| {
        if let ListResult::Item(info) = result
            && let Some(msg) = stream_message(info)
        {
            let _ = stream_tx.send(msg);
        }
    });
}

fn run_session(
    mainloop: &mut Mainloop,
    context: &mut Context,
    rx: &Receiver<Msg>,
    tx: &Sender<Msg>,
    events: &Sender<Event>,
    desired: &mut DesiredVolumes,
) -> Ended {
    let mut state = Session {
        sinks: BTreeMap::new(),
        streams: BTreeMap::new(),
        cursor: 0,
        next_call: 0,
        dirty: false,
        next_snapshot: 0,
    };
    let mut ready = false;
    let connect_deadline = clock() + 10_000;

    loop {
        let now = clock();
        let timeout = if !ready {
            Duration::from_millis(250)
        } else {
            let mut wait = 1000u64;
            if has_write_work(&state, desired) {
                wait = wait.min(state.next_call.saturating_sub(now));
            }
            if state.dirty {
                wait = wait.min(state.next_snapshot.saturating_sub(now));
            }
            Duration::from_millis(wait.max(1))
        };

        match rx.recv_timeout(timeout) {
            Ok(Msg::Request(AudioRequest::Stop)) | Err(RecvTimeoutError::Disconnected) => {
                return Ended::Stop;
            }
            Ok(Msg::Request(AudioRequest::Desired(next))) => {
                *desired = next;
                apply_desired_flags(&mut state, desired);
            }
            Ok(Msg::Request(AudioRequest::Resolve {
                key,
                is_endpoint,
                continue_driving,
            })) => {
                if continue_driving {
                    let targets = if is_endpoint {
                        &mut state.sinks
                    } else {
                        &mut state.streams
                    };
                    for target in targets.values_mut().filter(|t| t.key == key) {
                        target.hold = false;
                    }
                }
            }
            Ok(Msg::StateChanged) => {
                mainloop.lock();
                let current = context.get_state();
                mainloop.unlock();
                match current {
                    State::Ready if !ready => {
                        ready = true;
                        mainloop.lock();
                        subscribe(context, tx);
                        request_all(context, tx);
                        mainloop.unlock();
                    }
                    State::Failed | State::Terminated => return Ended::Lost,
                    _ => {}
                }
            }
            Ok(Msg::ListSink(index)) => {
                mainloop.lock();
                let sink_tx = tx.clone();
                context
                    .introspect()
                    .get_sink_info_by_index(index, move |result| {
                        if let ListResult::Item(info) = result
                            && let Some(msg) = sink_message(info)
                        {
                            let _ = sink_tx.send(msg);
                        }
                    });
                mainloop.unlock();
            }
            Ok(Msg::ListStream(index)) => {
                mainloop.lock();
                let stream_tx = tx.clone();
                context
                    .introspect()
                    .get_sink_input_info(index, move |result| {
                        if let ListResult::Item(info) = result
                            && let Some(msg) = stream_message(info)
                        {
                            let _ = stream_tx.send(msg);
                        }
                    });
                mainloop.unlock();
            }
            Ok(Msg::SinkGone(index)) => {
                state.dirty |= state.sinks.remove(&index).is_some();
                state.next_snapshot = 0;
            }
            Ok(Msg::StreamGone(index)) => {
                state.dirty |= state.streams.remove(&index).is_some();
                state.next_snapshot = 0;
            }
            Ok(Msg::Sink(data)) => {
                let name = data.name.clone();
                let desired_here = desired.endpoints.contains_key(&name);
                on_update(
                    &mut state,
                    true,
                    data.index,
                    name,
                    data.description,
                    data.volume,
                    desired_here,
                    events,
                );
            }
            Ok(Msg::Stream(data)) => {
                let desired_here = desired.apps.contains_key(&data.key);
                on_update(
                    &mut state,
                    false,
                    data.index,
                    data.key,
                    data.name,
                    data.volume,
                    desired_here,
                    events,
                );
            }
            Err(RecvTimeoutError::Timeout) => {
                if !ready && clock() > connect_deadline {
                    return Ended::Lost;
                }
            }
        }

        if ready {
            pump_writes(&mut state, desired, mainloop, context);
            if state.dirty && clock() >= state.next_snapshot {
                publish(&mut state, events);
            }
        }
    }
}

fn subscribe(context: &mut Context, tx: &Sender<Msg>) {
    let event_tx = tx.clone();
    context.set_subscribe_callback(Some(Box::new(move |facility, operation, index| {
        let removed = matches!(operation, Some(Operation::Removed));
        let msg = match (facility, removed) {
            (Some(Facility::Sink), true) => Msg::SinkGone(index),
            (Some(Facility::Sink), false) => Msg::ListSink(index),
            (Some(Facility::SinkInput), true) => Msg::StreamGone(index),
            (Some(Facility::SinkInput), false) => Msg::ListStream(index),
            _ => return,
        };
        let _ = event_tx.send(msg);
    })));
    context.subscribe(InterestMaskSet::SINK | InterestMaskSet::SINK_INPUT, |_| {});
}

#[allow(clippy::too_many_arguments)]
fn on_update(
    state: &mut Session,
    is_endpoint: bool,
    index: u32,
    key: String,
    name: String,
    volume: ChannelVolumes,
    desired_here: bool,
    events: &Sender<Event>,
) {
    let map = if is_endpoint {
        &mut state.sinks
    } else {
        &mut state.streams
    };
    let Some(target) = map.get_mut(&index) else {
        let mut target = Target::new(key, name, volume);
        target.was_desired = desired_here;
        map.insert(index, target);
        state.dirty = true;
        state.next_snapshot = 0;
        return;
    };
    if target.key != key || target.name != name {
        target.key = key;
        target.name = name;
        state.dirty = true;
        state.next_snapshot = 0;
    }
    target.volume = volume;
    let actual = scalar(&volume);
    let changed = (actual - target.reported).abs() >= VOLUME_EPSILON;
    let ours = target.expected.consume(actual, clock());
    target.reported = actual;
    if changed {
        state.dirty = true;
    }
    if ours || !changed {
        return;
    }
    target.written = actual;
    if !target.was_desired {
        return;
    }
    target.hold = true;
    let key = target.key.clone();
    // The engine reads this level when arming pickup, so it must arrive first.
    publish(state, events);
    let _ = events.send(Event::External {
        key,
        is_endpoint,
        volume: actual,
    });
}

fn apply_desired_flags(state: &mut Session, desired: &DesiredVolumes) {
    let mark = |target: &mut Target, is_desired: bool| {
        target.was_desired = is_desired;
        if !is_desired {
            target.hold = false;
        }
    };
    for target in state.sinks.values_mut() {
        mark(target, desired.endpoints.contains_key(&target.key));
    }
    for target in state.streams.values_mut() {
        mark(target, desired.apps.contains_key(&target.key));
    }
}

fn wanted(target: &Target, value: Option<&f32>) -> Option<f32> {
    let value = *value?;
    (!target.hold && (target.written - value).abs() >= VOLUME_EPSILON).then_some(value)
}

fn has_write_work(state: &Session, desired: &DesiredVolumes) -> bool {
    state
        .sinks
        .values()
        .any(|t| wanted(t, desired.endpoints.get(&t.key)).is_some())
        || state
            .streams
            .values()
            .any(|t| wanted(t, desired.apps.get(&t.key)).is_some())
}

/// Performs at most one volume call per budget slot, in round-robin order so
/// a category with many streams cannot starve another fader.
fn pump_writes(
    state: &mut Session,
    desired: &DesiredVolumes,
    mainloop: &mut Mainloop,
    context: &Context,
) {
    let now = clock();
    if now < state.next_call {
        return;
    }
    let order: Vec<(bool, u32)> = state
        .sinks
        .keys()
        .map(|i| (true, *i))
        .chain(state.streams.keys().map(|i| (false, *i)))
        .collect();
    let count = order.len();
    for step in 0..count {
        let (is_endpoint, index) = order[(state.cursor + step) % count];
        let target = if is_endpoint {
            state.sinks.get_mut(&index)
        } else {
            state.streams.get_mut(&index)
        };
        let Some(target) = target else { continue };
        let map = if is_endpoint {
            &desired.endpoints
        } else {
            &desired.apps
        };
        let Some(value) = wanted(target, map.get(&target.key)) else {
            continue;
        };
        let volumes = scaled(&target.volume, value);
        target.expected.add(value, now + EXPECTED_WINDOW_MS, now);
        target.written = value;
        target.volume = volumes;
        mainloop.lock();
        let mut introspect = context.introspect();
        if is_endpoint {
            introspect.set_sink_volume_by_index(index, &volumes, None);
        } else {
            introspect.set_sink_input_volume(index, &volumes, None);
        }
        mainloop.unlock();
        state.cursor = (state.cursor + step + 1) % count;
        state.next_call = now + MIN_CALL_INTERVAL_MS;
        return;
    }
}

fn publish(state: &mut Session, events: &Sender<Event>) {
    state.dirty = false;
    state.next_snapshot = clock() + MIN_SNAPSHOT_INTERVAL_MS;
    let mut outputs: Vec<OutputInfo> = state
        .sinks
        .values()
        .map(|t| OutputInfo {
            id: t.key.clone(),
            name: t.name.clone(),
            volume: t.reported,
        })
        .collect();
    outputs.sort_by_key(|a| a.name.to_lowercase());

    let mut grouped: HashMap<&str, (&str, f32, u32)> = HashMap::new();
    for target in state.streams.values() {
        let entry = grouped.entry(&target.key).or_insert((&target.name, 0.0, 0));
        entry.1 += target.reported;
        entry.2 += 1;
    }
    let mut apps: Vec<AppInfo> = grouped
        .into_iter()
        .map(|(key, (name, total, count))| AppInfo {
            key: key.to_owned(),
            name: name.to_owned(),
            volume: total / count as f32,
        })
        .collect();
    apps.sort_by(|a, b| a.key.cmp(&b.key));
    let _ = events.send(Event::Audio { outputs, apps });
}
