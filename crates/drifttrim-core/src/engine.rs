//! The fader pipeline: raw readings in, desired volumes out.
//!
//! The engine has no threads, clocks or I/O. Callers pass the current time and
//! collect [`AudioCommand`]s, so the behaviour is fully covered by unit tests.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::calibration::{CurvePoint, Taper};
use crate::capture;
use crate::logic::{has_reached, resolve_output, toggle_mute};
use crate::settings::{
    self, Category, CloseBehavior, FaderConfig, OutputPref, Settings, TargetKind,
    UNASSIGNED_CATEGORY,
};
use crate::source;

pub const SYSTEM_APP_KEY: &str = "#system";
pub const UNASSIGNED_DISPLAY: &str = "Everything Else";

/// Hold the current percent until the value moves more than this off it.
const HYSTERESIS: f64 = 0.9;
/// A raw jump beyond this (mV) is real movement rather than wiper noise.
const SNAP_BAND: f64 = 60.0;
/// The mute dead zone unlatches only this far (mV) past its threshold.
const MUTE_EXIT_BAND: i32 = 15;
const PICKUP_TOLERANCE: i32 = 2;
const MIN_VOL_WRITE_MS: u64 = 50;
const QMK_TIMEOUT_MS: u64 = 3500;
const QMK_CHECK_MS: u64 = 1000;
const SAVE_DEBOUNCE_MS: u64 = 400;
const MAX_FADERS: usize = 128;
const MAX_RECORD_SAMPLES: usize = 20_000;
const MAX_KNOWN_APPS: usize = 4096;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OutputInfo {
    pub id: String,
    pub name: String,
    pub volume: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppInfo {
    pub key: String,
    pub name: String,
    pub volume: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesiredVolumes {
    pub endpoints: BTreeMap<String, f32>,
    pub apps: BTreeMap<String, f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AudioCommand {
    Desired(DesiredVolumes),
    /// Answers an external change. While false, the target stays hands-off.
    ResolveExternal {
        key: String,
        is_endpoint: bool,
        continue_driving: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyAction {
    Up,
    Down,
    Mute,
}

/// Everything the UI can ask for. Arrives as JSON from QML.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Command {
    AddVirtual,
    Remove {
        id: String,
    },
    Move {
        id: String,
        delta: i32,
    },
    Rename {
        id: String,
        label: String,
    },
    SetVirtual {
        id: String,
        value: i32,
        committed: bool,
    },
    ToggleMute {
        id: String,
    },
    ResetVirtual {
        id: String,
    },
    SetTargetOutput {
        id: String,
        output: Option<String>,
    },
    SetTargetApp {
        id: String,
        app: Option<String>,
    },
    SetTargetCategory {
        id: String,
        category: Option<String>,
    },
    SetOutputs {
        id: String,
        outputs: Vec<String>,
    },
    SetMax {
        id: String,
        max: i32,
    },
    SetStep {
        id: String,
        step: i32,
    },
    SetCalibration {
        id: String,
        taper: Taper,
        inverted: bool,
        mute_raw: i32,
    },
    StartRecord {
        id: String,
    },
    StopRecord,
    ApplyRecord,
    CancelRecord,
    StartCapture {
        key: String,
        editing: Option<String>,
    },
    SetCaptureKey {
        key: String,
    },
    CommitCapture,
    CancelCapture,
    SetCategories {
        categories: Vec<Category>,
    },
    SetSoftTakeover {
        enabled: bool,
    },
    SetCloseBehavior {
        behavior: CloseBehavior,
    },
}

#[derive(Clone, Debug)]
struct Pickup {
    target: i32,
    position: Option<i32>,
}

#[derive(Clone, Debug)]
struct Fader {
    cfg: FaderConfig,
    curve: Vec<CurvePoint>,
    smoothed: f64,
    last_raw: Option<i32>,
    in_mute_zone: bool,
    last_applied: i32,
    current: f64,
    display: i32,
    pickup: Option<Pickup>,
    pickup_position: Option<i32>,
    active_output: Option<String>,
    drive_sequence: u64,
    vol_pending: bool,
    vol_last_write: Option<u64>,
}

impl Fader {
    fn new(cfg: FaderConfig) -> Self {
        let current = cfg.value as f64;
        let curve = cfg.cal.build_curve();
        let display = cfg.value;
        Self {
            cfg,
            curve,
            smoothed: -1.0,
            last_raw: None,
            in_mute_zone: false,
            last_applied: -1,
            current,
            display,
            pickup: None,
            pickup_position: None,
            active_output: None,
            drive_sequence: 0,
            vol_pending: false,
            vol_last_write: None,
        }
    }

    fn key(&self) -> &str {
        self.cfg.source_device_key.as_deref().unwrap_or("")
    }

    fn is_bound_to(&self, key: &str, axis: usize) -> bool {
        !self.cfg.is_virtual && self.key() == key && self.cfg.axis_index as usize == axis
    }
}

#[derive(Clone, Debug)]
struct Capture {
    key: String,
    editing: Option<String>,
    ranges: HashMap<usize, (i32, i32)>,
}

#[derive(Clone, Debug)]
struct Record {
    id: String,
    samples: Vec<i32>,
    result: Option<capture::Analysis>,
    range: Option<(i32, i32)>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FaderView {
    pub id: String,
    pub label: String,
    pub is_virtual: bool,
    pub source: String,
    pub connected: bool,
    pub live: bool,
    pub value: i32,
    pub syncing: bool,
    pub pickup_target: Option<i32>,
    pub pickup_position: Option<i32>,
    pub muted: bool,
    pub max: i32,
    pub step: i32,
    pub target: TargetKind,
    pub app_key: Option<String>,
    pub category_name: Option<String>,
    pub active_output: Option<String>,
    pub ranked_outputs: Vec<OutputPref>,
    pub status: String,
    pub raw: Option<i32>,
    pub units: &'static str,
    pub cal_min: i32,
    pub cal_max: i32,
    pub taper: Taper,
    pub inverted: bool,
    pub mute_raw: i32,
    pub raw_maximum: i32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    pub key: String,
    pub name: String,
    pub connected: bool,
    pub is_midi: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    pub key: String,
    pub name: String,
    pub live: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureView {
    pub key: String,
    pub editing: Option<String>,
    pub connected: bool,
    pub detected: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordView {
    pub id: String,
    pub samples: usize,
    pub finished: bool,
    pub good: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub faders: Vec<FaderView>,
    pub outputs: Vec<OutputInfo>,
    pub apps: Vec<AppView>,
    pub categories: Vec<Category>,
    pub sources: Vec<SourceView>,
    pub capture: Option<CaptureView>,
    pub record: Option<RecordView>,
    pub soft_takeover: bool,
    pub close_behavior: CloseBehavior,
    pub audio_ready: bool,
}

pub struct Engine {
    settings: Settings,
    faders: Vec<Fader>,
    outputs: Vec<OutputInfo>,
    apps: BTreeMap<String, AppInfo>,
    audio_ready: bool,
    sources: BTreeMap<String, (String, bool)>,
    midi_last_seen: HashMap<(String, usize), u64>,
    midi_expired: HashSet<(String, usize)>,
    midi_raw: HashMap<(String, usize), i32>,
    next_qmk_check: u64,
    next_sequence: u64,
    capture: Option<Capture>,
    record: Option<Record>,
    outbox: Vec<AudioCommand>,
    last_desired: Option<DesiredVolumes>,
    save_at: Option<u64>,
}

fn round(value: f64) -> i32 {
    value.round_ties_even() as i32
}

impl Engine {
    pub fn new(mut settings: Settings) -> Self {
        let faders = std::mem::take(&mut settings.layout)
            .into_iter()
            .map(Fader::new)
            .collect();
        Self {
            settings,
            faders,
            outputs: Vec::new(),
            apps: BTreeMap::new(),
            audio_ready: false,
            sources: BTreeMap::new(),
            midi_last_seen: HashMap::new(),
            midi_expired: HashSet::new(),
            midi_raw: HashMap::new(),
            next_qmk_check: 0,
            next_sequence: 0,
            capture: None,
            record: None,
            outbox: Vec::new(),
            last_desired: None,
            save_at: None,
        }
    }

    // ---- lookups ----------------------------------------------------------

    fn index_of(&self, id: &str) -> Option<usize> {
        self.faders.iter().position(|f| f.cfg.id == id)
    }

    fn is_present(&self, id: &str) -> bool {
        self.outputs.iter().any(|o| o.id == id)
    }

    fn resolve(&self, i: usize) -> Option<String> {
        let f = &self.faders[i];
        resolve_output(
            f.cfg.outputs.iter().map(|o| o.id.as_str()),
            |id| self.is_present(id),
            f.cfg.override_id.as_deref(),
        )
        .map(str::to_owned)
    }

    fn source_connected(&self, key: &str) -> bool {
        self.sources
            .get(key)
            .is_some_and(|(_, connected)| *connected)
    }

    fn midi_live(&self, f: &Fader) -> bool {
        if !source::is_midi(f.key()) {
            return true;
        }
        let binding = (f.key().to_owned(), f.cfg.axis_index as usize);
        self.midi_last_seen.contains_key(&binding) && !self.midi_expired.contains(&binding)
    }

    fn connected(&self, i: usize) -> bool {
        let f = &self.faders[i];
        f.cfg.is_virtual || (self.source_connected(f.key()) && self.midi_live(f))
    }

    /// Effective Max %. Outputs remember their own cap; virtual faders have none.
    fn cap(&self, i: usize) -> i32 {
        let f = &self.faders[i];
        if f.cfg.is_virtual {
            100
        } else if f.cfg.target == TargetKind::Output {
            self.resolve(i)
                .and_then(|id| self.settings.device_max.get(&id).copied())
                .unwrap_or(100)
                .clamp(1, 100)
        } else {
            f.cfg.max
        }
    }

    fn unassigned_known(&self) -> Vec<String> {
        self.settings
            .known_apps
            .keys()
            .filter(|key| {
                !self
                    .settings
                    .categories
                    .iter()
                    .any(|c| c.app_keys.contains(key))
                    && !(0..self.faders.len()).any(|i| {
                        let f = &self.faders[i];
                        self.connected(i)
                            && f.cfg.target == TargetKind::App
                            && f.cfg.app_key.as_ref() == Some(key)
                    })
            })
            .cloned()
            .collect()
    }

    fn unassigned_live(&self) -> Vec<String> {
        self.unassigned_known()
            .into_iter()
            .filter(|k| self.apps.contains_key(k))
            .collect()
    }

    fn category(&self, name: Option<&str>) -> Option<&Category> {
        self.settings
            .categories
            .iter()
            .find(|c| Some(c.name.as_str()) == name)
    }

    /// Live app keys this fader drives, for app and category targets.
    fn live_app_targets(&self, i: usize) -> Vec<String> {
        let f = &self.faders[i];
        match f.cfg.target {
            TargetKind::Output => Vec::new(),
            TargetKind::App => f
                .cfg
                .app_key
                .iter()
                .filter(|k| self.apps.contains_key(*k))
                .cloned()
                .collect(),
            TargetKind::Category if f.cfg.category_name.as_deref() == Some(UNASSIGNED_CATEGORY) => {
                self.unassigned_live()
            }
            TargetKind::Category => self
                .category(f.cfg.category_name.as_deref())
                .map(|c| {
                    c.app_keys
                        .iter()
                        .filter(|k| self.apps.contains_key(*k))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn drivable(&self, i: usize) -> bool {
        if !self.connected(i) {
            return false;
        }
        match self.faders[i].cfg.target {
            TargetKind::Output => self.resolve(i).is_some(),
            _ => !self.live_app_targets(i).is_empty(),
        }
    }

    // ---- inbound events ---------------------------------------------------

    pub fn set_audio(&mut self, outputs: Vec<OutputInfo>, apps: Vec<AppInfo>, now: u64) {
        let first = !self.audio_ready;
        self.audio_ready = true;
        let previous: HashSet<String> = self.apps.keys().cloned().collect();
        let before_known = self.settings.known_apps.clone();
        let stamp = now as i64;
        self.outputs = outputs;
        self.apps = apps.into_iter().map(|a| (a.key.clone(), a)).collect();
        for app in self.apps.values() {
            if self.settings.known_apps.len() < MAX_KNOWN_APPS
                || self.settings.known_apps.contains_key(&app.key)
            {
                self.settings
                    .known_apps
                    .insert(app.key.clone(), app.name.clone());
                self.settings.app_seen.insert(app.key.clone(), stamp);
            }
        }
        if before_known != self.settings.known_apps {
            self.mark_dirty(now);
        }
        let added: HashSet<String> = self
            .apps
            .keys()
            .filter(|k| !previous.contains(*k))
            .cloned()
            .collect();

        for i in 0..self.faders.len() {
            let resolved = self.resolve(i);
            let output_changed = self.faders[i].cfg.target == TargetKind::Output
                && self.faders[i].active_output != resolved;
            let gained_app = self.live_app_targets(i).iter().any(|k| added.contains(k));
            self.faders[i].active_output = resolved;
            if output_changed || gained_app {
                self.faders[i].last_applied = -1;
                if self.faders[i].cfg.is_virtual {
                    self.render(i, false);
                } else {
                    self.arm_pickup(i);
                }
            }
        }
        if first {
            for i in 0..self.faders.len() {
                if !self.faders[i].cfg.is_virtual {
                    self.arm_pickup(i);
                }
            }
        }
        self.publish();
    }

    /// Another mixer changed a volume we are driving.
    pub fn external_change(&mut self, key: &str, is_endpoint: bool, volume: f32, now: u64) {
        let targets: Vec<usize> = (0..self.faders.len())
            .filter(|&i| self.targets_change(i, key, is_endpoint))
            .collect();
        let mut physical_pickup = targets
            .iter()
            .any(|&i| !self.faders[i].cfg.is_virtual && self.faders[i].pickup.is_some());
        if !physical_pickup {
            let owner = targets
                .iter()
                .copied()
                .filter(|&i| {
                    let f = &self.faders[i];
                    f.last_applied >= 0 && f.pickup.is_none() && self.connected(i)
                })
                .max_by_key(|&i| self.faders[i].drive_sequence);
            if let Some(i) = owner {
                if self.faders[i].cfg.is_virtual {
                    // No physical position to reconcile, so follow the mixer.
                    let level = round(volume as f64 * 100.0).clamp(0, 100);
                    let f = &mut self.faders[i];
                    f.cfg.muted = false;
                    f.cfg.value = level;
                    f.current = level as f64;
                    f.last_applied = level;
                    f.vol_pending = false;
                    self.render(i, false);
                    self.mark_dirty(now);
                } else if !self.faders[i].cfg.muted {
                    self.arm_pickup(i);
                    physical_pickup = self.faders[i].pickup.is_some();
                }
            }
        }
        self.outbox.push(AudioCommand::ResolveExternal {
            key: key.to_owned(),
            is_endpoint,
            continue_driving: !physical_pickup,
        });
        if physical_pickup {
            self.publish();
        }
    }

    fn targets_change(&self, i: usize, key: &str, is_endpoint: bool) -> bool {
        let f = &self.faders[i];
        if is_endpoint {
            return f.cfg.target == TargetKind::Output && self.resolve(i).as_deref() == Some(key);
        }
        match f.cfg.target {
            TargetKind::Output => false,
            TargetKind::App => f.cfg.app_key.as_deref() == Some(key),
            TargetKind::Category if f.cfg.category_name.as_deref() == Some(UNASSIGNED_CATEGORY) => {
                self.unassigned_known().iter().any(|k| k == key)
            }
            TargetKind::Category => self
                .category(f.cfg.category_name.as_deref())
                .is_some_and(|c| c.app_keys.iter().any(|k| k == key)),
        }
    }

    pub fn set_source(&mut self, key: &str, name: &str, connected: bool) {
        let was = self.source_connected(key);
        self.sources
            .insert(key.to_owned(), (name.to_owned(), connected));
        if was == connected {
            return;
        }
        if !connected {
            self.clear_source(key);
        } else if !source::is_midi(key) {
            for i in 0..self.faders.len() {
                if self.faders[i].key() == key && !self.faders[i].cfg.is_virtual {
                    self.arm_pickup(i);
                }
            }
        }
        self.publish();
    }

    fn clear_source(&mut self, key: &str) {
        self.midi_last_seen.retain(|(k, _), _| k != key);
        self.midi_expired.retain(|(k, _)| k != key);
        self.midi_raw.retain(|(k, _), _| k != key);
        for f in self.faders.iter_mut().filter(|f| f.key() == key) {
            f.smoothed = -1.0;
            f.last_raw = None;
            f.pickup = None;
            f.pickup_position = None;
        }
    }

    /// One reading from a physical control.
    pub fn sample(&mut self, key: &str, axis: usize, raw: i32, now: u64) {
        self.feed_capture(key, axis, raw);
        let matching: Vec<usize> = (0..self.faders.len())
            .filter(|&i| self.faders[i].is_bound_to(key, axis))
            .collect();
        if !source::is_midi(key) {
            for i in matching {
                self.apply_axis(i, raw);
            }
            return;
        }
        let binding = (key.to_owned(), axis);
        let resumed =
            self.midi_expired.remove(&binding) || !self.midi_last_seen.contains_key(&binding);
        self.midi_last_seen.insert(binding.clone(), now);
        let previous = self.midi_raw.insert(binding, raw);
        for i in matching {
            if resumed {
                self.faders[i].smoothed = -1.0;
                self.arm_pickup(i);
            }
            // Heartbeats refresh liveness without taking ownership of the target.
            if resumed || previous != Some(raw) || self.faders[i].smoothed < 0.0 {
                self.apply_axis(i, raw);
            }
        }
    }

    fn feed_capture(&mut self, key: &str, axis: usize, raw: i32) {
        if let Some(record) = &mut self.record
            && record.result.is_none()
            && record.samples.len() < MAX_RECORD_SAMPLES
        {
            let bound = self
                .faders
                .iter()
                .any(|f| f.cfg.id == record.id && f.is_bound_to(key, axis));
            if bound {
                record.samples.push(raw);
            }
        }
        if let Some(capture) = &mut self.capture
            && capture.key == key
            && axis < source::axes(key)
        {
            let range = capture.ranges.entry(axis).or_insert((raw, raw));
            range.0 = range.0.min(raw);
            range.1 = range.1.max(raw);
        }
    }

    fn apply_axis(&mut self, i: usize, raw: i32) {
        let f = &mut self.faders[i];
        f.last_raw = Some(raw);
        // Smooth only inside the noise band. Reports arrive per change, so easing
        // a large jump would leave the value far from where the fader stopped.
        f.smoothed = if source::is_midi(f.key())
            || f.smoothed < 0.0
            || (raw as f64 - f.smoothed).abs() > SNAP_BAND
        {
            raw as f64
        } else {
            f.smoothed * 0.85 + raw as f64 * 0.15
        };
        self.render(i, true);
    }

    pub fn hotkey(&mut self, id: &str, action: HotkeyAction, now: u64) {
        let Some(i) = self.index_of(id) else { return };
        if !self.faders[i].cfg.is_virtual {
            return;
        }
        match action {
            HotkeyAction::Up => self.nudge(i, self.faders[i].cfg.step, now),
            HotkeyAction::Down => self.nudge(i, -self.faders[i].cfg.step, now),
            HotkeyAction::Mute => self.toggle_mute(i, now),
        }
    }

    fn nudge(&mut self, i: usize, delta: i32, now: u64) {
        let f = &mut self.faders[i];
        f.cfg.muted = false;
        f.cfg.value = (f.cfg.value + delta).clamp(0, 100);
        self.mark_dirty(now);
    }

    fn toggle_mute(&mut self, i: usize, now: u64) {
        self.faders[i].pickup = None;
        self.faders[i].pickup_position = None;
        if self.faders[i].cfg.is_virtual {
            let f = &mut self.faders[i];
            let state = toggle_mute(f.cfg.muted, f.cfg.value, f.cfg.pre_mute);
            f.cfg.muted = state.muted;
            f.cfg.value = state.target;
            f.cfg.pre_mute = state.pre_mute;
        } else {
            let f = &mut self.faders[i];
            if !f.cfg.muted {
                f.cfg.pre_mute = f.last_applied.max(0);
            }
            f.cfg.muted = !f.cfg.muted;
            f.last_applied = -1;
            self.render(i, true);
        }
        self.mark_dirty(now);
    }

    // ---- soft takeover ----------------------------------------------------

    fn arm_pickup(&mut self, i: usize) {
        if !self.settings.soft_takeover {
            self.cancel_pickup(i);
            return;
        }
        let f = &self.faders[i];
        if f.cfg.is_virtual || f.cfg.muted || !self.audio_ready || !self.connected(i) {
            return;
        }
        let mut values: Vec<f32> = Vec::new();
        let mut any_target = false;
        if f.cfg.target == TargetKind::Output {
            if let Some(id) = self.resolve(i) {
                any_target = true;
                values.extend(self.outputs.iter().filter(|o| o.id == id).map(|o| o.volume));
            }
        } else {
            for key in self.live_app_targets(i) {
                any_target = true;
                values.extend(self.apps.get(&key).map(|a| a.volume));
            }
        }
        if !any_target || values.is_empty() {
            self.cancel_pickup(i);
            if any_target {
                self.faders[i].last_applied = -1;
                self.render(i, false);
            }
            return;
        }
        let average = values.iter().map(|v| *v as f64).sum::<f64>() / values.len() as f64;
        let level = round(average * 100.0).clamp(0, 100);
        // A cap can put the current level out of reach. Taking control at the
        // fader's maximum is the safest handoff.
        let target = level.min(self.cap(i));
        let f = &mut self.faders[i];
        f.pickup = Some(Pickup {
            target,
            position: None,
        });
        f.last_applied = target;
        self.render(i, false);
    }

    fn cancel_pickup(&mut self, i: usize) {
        self.faders[i].pickup = None;
        self.faders[i].pickup_position = None;
    }

    // ---- rendering --------------------------------------------------------

    fn render(&mut self, i: usize, take_ownership: bool) {
        let cap = self.cap(i) as f64;
        let recording = self.record.as_ref().is_some_and(|r| r.result.is_none());
        let f = &mut self.faders[i];
        let is_virtual = f.cfg.is_virtual;
        if !is_virtual && f.smoothed < 0.0 {
            return;
        }
        let mut percent = if is_virtual {
            f.current
        } else {
            f.cfg
                .cal
                .map_raw(&f.curve, f.smoothed.round_ties_even() as i32)
        };
        if !is_virtual {
            // Gate on the instantaneous reading so crossing the threshold mutes
            // on that very report, with an exit band so noise cannot flicker it.
            let raw = f.last_raw.unwrap_or(i32::MIN);
            if f.cfg.cal.mute_raw > 0 {
                if f.cfg.cal.is_in_mute_zone(raw) {
                    f.in_mute_zone = true;
                } else {
                    let band = if source::is_midi(f.key()) {
                        ((f.cfg.cal.max - f.cfg.cal.min).abs() / 200).max(1)
                    } else {
                        MUTE_EXIT_BAND
                    };
                    if f.cfg.cal.is_outside_mute_zone(raw, band) {
                        f.in_mute_zone = false;
                    }
                }
            } else {
                f.in_mute_zone = false;
            }
            if f.in_mute_zone {
                percent = 0.0;
            }
        }
        let cap = if is_virtual { 100.0 } else { cap };
        let mut level = (percent * cap / 100.0).clamp(0.0, 100.0);
        if !is_virtual && f.cfg.muted {
            level = 0.0;
        }

        if !is_virtual
            && !f.cfg.muted
            && let Some(pickup) = &mut f.pickup
        {
            let position = round(level);
            f.pickup_position = Some(position);
            let previous = pickup.position.replace(position);
            f.display = pickup.target;
            if !has_reached(previous, position, pickup.target, PICKUP_TOLERANCE) {
                return;
            }
            f.pickup = None;
            f.pickup_position = None;
            f.last_applied = -1;
        }

        let applied = if f.last_applied < 0 || (level - f.last_applied as f64).abs() > HYSTERESIS {
            round(level)
        } else {
            f.last_applied
        };
        f.display = applied;
        if applied == f.last_applied {
            return;
        }
        if take_ownership {
            self.next_sequence += 1;
            f.drive_sequence = self.next_sequence;
        }
        f.last_applied = applied;
        if !recording {
            f.vol_pending = true;
        }
    }

    /// Builds the complete desired map. Older owners are written first so the
    /// most recently moved fader wins where targets overlap.
    fn desired(&self) -> DesiredVolumes {
        let mut desired = DesiredVolumes::default();
        let mut order: Vec<usize> = (0..self.faders.len()).collect();
        order.sort_by_key(|&i| self.faders[i].drive_sequence);
        for i in order {
            let f = &self.faders[i];
            if f.last_applied < 0 || f.pickup.is_some() || !self.connected(i) {
                continue;
            }
            let scalar = f.last_applied as f32 / 100.0;
            if f.cfg.target == TargetKind::Output {
                if let Some(id) = self.resolve(i) {
                    desired.endpoints.insert(id, scalar);
                }
            } else {
                for key in self.live_app_targets(i) {
                    desired.apps.insert(key, scalar);
                }
            }
        }
        desired
    }

    fn publish(&mut self) {
        let desired = self.desired();
        if self.last_desired.as_ref() != Some(&desired) {
            self.last_desired = Some(desired.clone());
            self.outbox.push(AudioCommand::Desired(desired));
        }
    }

    pub fn take_commands(&mut self) -> Vec<AudioCommand> {
        std::mem::take(&mut self.outbox)
    }

    // ---- timing -----------------------------------------------------------

    /// True while a fader is ramping or a volume write is waiting.
    pub fn needs_fast_tick(&self) -> bool {
        self.faders
            .iter()
            .any(|f| f.vol_pending || (f.cfg.is_virtual && f.current != f.cfg.value as f64))
    }

    pub fn tick(&mut self, now: u64) {
        for i in 0..self.faders.len() {
            let f = &mut self.faders[i];
            if !f.cfg.is_virtual {
                continue;
            }
            let diff = f.cfg.value as f64 - f.current;
            if diff.abs() < 0.5 {
                if f.current != f.cfg.value as f64 {
                    f.current = f.cfg.value as f64;
                    self.render(i, true);
                }
            } else {
                f.current += diff * 0.35;
                self.render(i, true);
            }
        }

        let mut changed = false;
        for f in &mut self.faders {
            if f.vol_pending
                && f.vol_last_write
                    .is_none_or(|at| now.saturating_sub(at) >= MIN_VOL_WRITE_MS)
            {
                f.vol_pending = false;
                f.vol_last_write = Some(now);
                changed = true;
            }
        }
        if changed {
            self.publish();
        }

        if now >= self.next_qmk_check {
            self.next_qmk_check = now + QMK_CHECK_MS;
            self.expire_qmk(now);
        }
    }

    fn expire_qmk(&mut self, now: u64) {
        let stale: Vec<(String, usize)> = self
            .midi_last_seen
            .iter()
            .filter(|((key, _), seen)| {
                source::is_qmk(key) && now.saturating_sub(**seen) > QMK_TIMEOUT_MS
            })
            .map(|(binding, _)| binding.clone())
            .filter(|binding| !self.midi_expired.contains(binding))
            .collect();
        if stale.is_empty() {
            return;
        }
        for binding in stale {
            self.midi_raw.remove(&binding);
            for f in self
                .faders
                .iter_mut()
                .filter(|f| f.is_bound_to(&binding.0, binding.1))
            {
                f.pickup = None;
                f.pickup_position = None;
                f.smoothed = -1.0;
                f.last_raw = None;
            }
            self.midi_expired.insert(binding);
        }
        self.publish();
    }

    fn mark_dirty(&mut self, now: u64) {
        self.save_at = Some(now + SAVE_DEBOUNCE_MS);
    }

    /// Returns settings to write once edits have settled.
    pub fn take_save(&mut self, now: u64) -> Option<Settings> {
        if self.save_at.is_some_and(|at| now >= at) {
            self.save_at = None;
            return Some(self.snapshot_settings());
        }
        None
    }

    pub fn snapshot_settings(&self) -> Settings {
        let mut settings = self.settings.clone();
        settings.layout = self.faders.iter().map(|f| f.cfg.clone()).collect();
        settings
    }

    // ---- commands ---------------------------------------------------------

    /// Source keys that input backends should be listening to.
    pub fn wanted_sources(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .faders
            .iter()
            .filter(|f| !f.cfg.is_virtual)
            .map(|f| f.key().to_owned())
            .chain(self.capture.iter().map(|c| c.key.clone()))
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    /// Virtual faders with their labels, for global shortcut registration.
    pub fn hotkey_faders(&self) -> Vec<(String, String)> {
        self.faders
            .iter()
            .filter(|f| f.cfg.is_virtual)
            .map(|f| (f.cfg.id.clone(), f.cfg.label.clone()))
            .collect()
    }

    pub fn command(&mut self, command: Command, now: u64) {
        match command {
            Command::AddVirtual => {
                if self.faders.len() < MAX_FADERS {
                    self.faders.push(Fader::new(FaderConfig {
                        id: settings::new_fader_id(),
                        label: "Virtual fader".to_owned(),
                        is_virtual: true,
                        axis_index: -1,
                        ..Default::default()
                    }));
                }
            }
            Command::Remove { id } => {
                self.faders.retain(|f| f.cfg.id != id);
                if self.record.as_ref().is_some_and(|r| r.id == id) {
                    self.record = None;
                }
                self.publish();
            }
            Command::Move { id, delta } => {
                if let Some(i) = self.index_of(&id) {
                    let j =
                        (i as i64 + delta as i64).clamp(0, self.faders.len() as i64 - 1) as usize;
                    let fader = self.faders.remove(i);
                    self.faders.insert(j, fader);
                }
            }
            Command::Rename { id, label } => {
                if let Some(i) = self.index_of(&id) {
                    let label: String = label.trim().chars().take(80).collect();
                    if !label.is_empty() {
                        self.faders[i].cfg.label = label;
                    }
                }
            }
            Command::SetVirtual {
                id,
                value,
                committed,
            } => {
                if let Some(i) = self
                    .index_of(&id)
                    .filter(|&i| self.faders[i].cfg.is_virtual)
                {
                    let value = value.clamp(0, 100);
                    let f = &mut self.faders[i];
                    f.cfg.value = value;
                    f.current = value as f64;
                    f.cfg.muted = false;
                    self.render(i, true);
                    if !committed {
                        return;
                    }
                }
            }
            Command::ToggleMute { id } => {
                if let Some(i) = self.index_of(&id) {
                    self.toggle_mute(i, now);
                }
            }
            Command::ResetVirtual { id } => {
                if let Some(i) = self
                    .index_of(&id)
                    .filter(|&i| self.faders[i].cfg.is_virtual)
                {
                    let f = &mut self.faders[i];
                    f.cfg.muted = false;
                    f.cfg.value = 100;
                    f.current = 100.0;
                    self.render(i, true);
                }
            }
            Command::SetTargetOutput { id, output } => {
                if let Some(i) = self.index_of(&id) {
                    let f = &mut self.faders[i];
                    f.cfg.target = TargetKind::Output;
                    f.cfg.override_id = output.clone();
                    if let Some(output) = output
                        && !f.cfg.outputs.iter().any(|o| o.id == output)
                    {
                        let name = self
                            .outputs
                            .iter()
                            .find(|o| o.id == output)
                            .map(|o| o.name.clone());
                        let f = &mut self.faders[i];
                        f.cfg.outputs.push(OutputPref {
                            name: name.unwrap_or_else(|| output.clone()),
                            id: output,
                        });
                    }
                    self.retarget(i);
                }
            }
            Command::SetTargetApp { id, app } => {
                if let Some(i) = self.index_of(&id) {
                    let f = &mut self.faders[i];
                    f.cfg.target = if app.is_some() {
                        TargetKind::App
                    } else {
                        TargetKind::Output
                    };
                    f.cfg.app_key = app;
                    self.retarget(i);
                }
            }
            Command::SetTargetCategory { id, category } => {
                if let Some(i) = self.index_of(&id) {
                    let f = &mut self.faders[i];
                    f.cfg.target = if category.is_some() {
                        TargetKind::Category
                    } else {
                        TargetKind::Output
                    };
                    f.cfg.category_name = category;
                    self.retarget(i);
                }
            }
            Command::SetOutputs { id, outputs } => {
                if let Some(i) = self.index_of(&id) {
                    let known = &self.faders[i].cfg.outputs;
                    let prefs = outputs
                        .into_iter()
                        .map(|output| {
                            let name = self
                                .outputs
                                .iter()
                                .find(|o| o.id == output)
                                .map(|o| o.name.clone())
                                .or_else(|| {
                                    known
                                        .iter()
                                        .find(|o| o.id == output)
                                        .map(|o| o.name.clone())
                                })
                                .unwrap_or_else(|| output.clone());
                            OutputPref { id: output, name }
                        })
                        .collect();
                    let f = &mut self.faders[i];
                    f.cfg.outputs = prefs;
                    f.cfg.override_id = None;
                    self.retarget(i);
                }
            }
            Command::SetMax { id, max } => {
                if let Some(i) = self.index_of(&id) {
                    let max = max.clamp(1, 100);
                    match (self.faders[i].cfg.target, self.resolve(i)) {
                        (TargetKind::Output, Some(output)) => {
                            self.settings.device_max.insert(output, max);
                        }
                        _ => self.faders[i].cfg.max = max,
                    }
                    self.faders[i].last_applied = -1;
                    self.render(i, true);
                }
            }
            Command::SetStep { id, step } => {
                if let Some(i) = self.index_of(&id) {
                    self.faders[i].cfg.step = step.clamp(1, 100);
                }
            }
            Command::SetCalibration {
                id,
                taper,
                inverted,
                mute_raw,
            } => {
                if let Some(i) = self.index_of(&id) {
                    let f = &mut self.faders[i];
                    f.cfg.cal.taper = taper;
                    f.cfg.cal.inverted = inverted;
                    f.cfg.cal.mute_raw = mute_raw.clamp(0, u16::MAX as i32);
                    f.curve = f.cfg.cal.build_curve();
                    f.last_applied = -1;
                    self.render(i, true);
                }
            }
            Command::StartRecord { id } => {
                if self.index_of(&id).is_some() {
                    self.record = Some(Record {
                        id,
                        samples: Vec::new(),
                        result: None,
                        range: None,
                    });
                }
                return;
            }
            Command::StopRecord => {
                self.stop_record();
                return;
            }
            Command::ApplyRecord => self.apply_record(),
            Command::CancelRecord => {
                self.record = None;
                return;
            }
            Command::StartCapture { key, editing } => {
                self.capture = Some(Capture {
                    key: source::canonical_key(&key),
                    editing,
                    ranges: HashMap::new(),
                });
                return;
            }
            Command::SetCaptureKey { key } => {
                if let Some(capture) = &mut self.capture {
                    capture.key = source::canonical_key(&key);
                    capture.ranges.clear();
                }
                return;
            }
            Command::CommitCapture => self.commit_capture(),
            Command::CancelCapture => {
                self.capture = None;
                return;
            }
            Command::SetCategories { categories } => {
                let normalized = settings::normalize(Settings {
                    categories,
                    ..Default::default()
                });
                self.settings.categories = normalized.categories;
                self.publish();
            }
            Command::SetSoftTakeover { enabled } => {
                self.settings.soft_takeover = enabled;
                for i in 0..self.faders.len() {
                    if self.faders[i].cfg.is_virtual {
                        continue;
                    }
                    if enabled {
                        self.arm_pickup(i);
                    } else {
                        self.cancel_pickup(i);
                    }
                }
                self.publish();
            }
            Command::SetCloseBehavior { behavior } => self.settings.close_behavior = behavior,
        }
        self.mark_dirty(now);
    }

    /// The fader now points elsewhere, so its old level no longer applies.
    fn retarget(&mut self, i: usize) {
        self.faders[i].active_output = self.resolve(i);
        self.faders[i].last_applied = -1;
        if self.faders[i].cfg.is_virtual {
            self.render(i, true);
        } else {
            self.arm_pickup(i);
        }
        self.publish();
    }

    fn stop_record(&mut self) {
        let Some(record) = &self.record else { return };
        let Some(i) = self.index_of(&record.id) else {
            return;
        };
        let f = &self.faders[i];
        let prior = (f.cfg.cal.max - f.cfg.cal.min).abs();
        let full_scale = source::raw_maximum(f.key());
        let samples = record.samples.clone();
        let analysis = capture::analyze(&samples, prior, full_scale);
        let range = samples
            .iter()
            .min()
            .copied()
            .zip(samples.iter().max().copied());
        if let Some(record) = &mut self.record {
            record.result = Some(analysis);
            record.range = range;
        }
    }

    fn apply_record(&mut self) {
        let Some(record) = self.record.take() else {
            return;
        };
        let (Some(analysis), Some((min, max)), Some(i)) =
            (record.result, record.range, self.index_of(&record.id))
        else {
            return;
        };
        let f = &mut self.faders[i];
        f.cfg.cal.min = min;
        f.cfg.cal.max = max;
        if let Some(mute) = analysis.suggested_mute_raw {
            f.cfg.cal.mute_raw = mute;
        }
        f.curve = f.cfg.cal.build_curve();
        f.last_applied = -1;
        self.render(i, true);
        self.publish();
    }

    fn used_bindings(&self, except: Option<&str>) -> HashSet<String> {
        self.faders
            .iter()
            .filter(|f| !f.cfg.is_virtual && Some(f.cfg.id.as_str()) != except)
            .map(|f| source::binding_key(f.key(), f.cfg.axis_index as usize))
            .collect()
    }

    fn capture_candidate(&self) -> Option<(usize, i32, i32)> {
        let capture = self.capture.as_ref()?;
        let used = self.used_bindings(capture.editing.as_deref());
        let best = capture
            .ranges
            .iter()
            .filter(|(axis, _)| !used.contains(&source::binding_key(&capture.key, **axis)))
            .max_by_key(|(axis, (min, max))| (*max - *min, std::cmp::Reverse(**axis)))?;
        let (axis, (min, max)) = (*best.0, *best.1);
        (max - min >= source::travel_threshold(&capture.key)).then_some((axis, min, max))
    }

    fn commit_capture(&mut self) {
        let Some((axis, min, max)) = self.capture_candidate() else {
            return;
        };
        let Some(capture) = self.capture.take() else {
            return;
        };
        let name = self.sources.get(&capture.key).map(|(name, _)| name.clone());
        if let Some(i) = capture.editing.as_deref().and_then(|id| self.index_of(id)) {
            let f = &mut self.faders[i];
            f.cfg.source_device_key = Some(capture.key.clone());
            f.cfg.source_device_name = name;
            f.cfg.axis_index = axis as i32;
            f.cfg.cal.min = min;
            f.cfg.cal.max = max;
            f.curve = f.cfg.cal.build_curve();
            f.smoothed = -1.0;
            f.last_raw = None;
            f.last_applied = -1;
            self.arm_pickup(i);
            self.publish();
        } else if self.faders.len() < MAX_FADERS {
            let mut cfg = FaderConfig {
                id: settings::new_fader_id(),
                source_device_key: Some(capture.key.clone()),
                source_device_name: name,
                axis_index: axis as i32,
                label: format!(
                    "Fader {}",
                    self.faders.iter().filter(|f| !f.cfg.is_virtual).count() + 1
                ),
                ..Default::default()
            };
            cfg.cal.min = min;
            cfg.cal.max = max;
            self.faders.push(Fader::new(cfg));
        }
    }

    // ---- view -------------------------------------------------------------

    fn output_name(&self, id: &str, i: usize) -> String {
        self.outputs
            .iter()
            .find(|o| o.id == id)
            .map(|o| o.name.clone())
            .or_else(|| {
                self.faders[i]
                    .cfg
                    .outputs
                    .iter()
                    .find(|o| o.id == id)
                    .map(|o| o.name.clone())
            })
            .unwrap_or_else(|| id.to_owned())
    }

    fn target_status(&self, i: usize) -> String {
        let f = &self.faders[i];
        match f.cfg.target {
            TargetKind::Output => match self.resolve(i) {
                Some(id) => format!("Controlling output {}", self.output_name(&id, i)),
                None => "No configured output is currently connected".to_owned(),
            },
            TargetKind::App => {
                let name = f
                    .cfg
                    .app_key
                    .as_ref()
                    .and_then(|k| self.settings.known_apps.get(k).cloned())
                    .unwrap_or_else(|| "Selected app".to_owned());
                if self.live_app_targets(i).is_empty() {
                    format!("{name} is not currently running")
                } else {
                    format!("Controlling {name}")
                }
            }
            TargetKind::Category => {
                let (name, keys) = if f.cfg.category_name.as_deref() == Some(UNASSIGNED_CATEGORY) {
                    (UNASSIGNED_DISPLAY.to_owned(), self.unassigned_known())
                } else {
                    (
                        f.cfg
                            .category_name
                            .clone()
                            .unwrap_or_else(|| "Selected category".to_owned()),
                        self.category(f.cfg.category_name.as_deref())
                            .map(|c| c.app_keys.clone())
                            .unwrap_or_default(),
                    )
                };
                let active = keys.iter().filter(|k| self.apps.contains_key(*k)).count();
                format!("{name}: {active} of {} apps active", keys.len())
            }
        }
    }

    fn source_text(&self, i: usize) -> String {
        let f = &self.faders[i];
        if f.cfg.is_virtual {
            return "Virtual".to_owned();
        }
        let name = f
            .cfg
            .source_device_name
            .as_deref()
            .unwrap_or("Unknown device");
        let control = source::control_name(f.key(), f.cfg.axis_index as usize);
        if self.connected(i) {
            format!("{name} · {control}")
        } else {
            format!("{name} · {control} · Offline")
        }
    }

    fn capture_view(&self) -> Option<CaptureView> {
        let capture = self.capture.as_ref()?;
        let connected = self.source_connected(&capture.key);
        let candidate = self.capture_candidate();
        let detected = candidate.map(|(axis, _, _)| source::control_name(&capture.key, axis));
        let message = match candidate {
            Some((axis, min, max)) => format!(
                "Detected {} — range {min}–{max} {}. Sweep it fully, then capture.",
                source::control_name(&capture.key, axis),
                source::units(&capture.key)
            ),
            None if !connected => {
                "This source is offline. Connect it or choose another device.".to_owned()
            }
            None => "Move one fader fully from bottom to top.".to_owned(),
        };
        Some(CaptureView {
            key: capture.key.clone(),
            editing: capture.editing.clone(),
            connected,
            detected,
            message,
        })
    }

    pub fn view(&self) -> View {
        let faders = (0..self.faders.len())
            .map(|i| {
                let f = &self.faders[i];
                FaderView {
                    id: f.cfg.id.clone(),
                    label: f.cfg.label.clone(),
                    is_virtual: f.cfg.is_virtual,
                    source: self.source_text(i),
                    connected: self.connected(i),
                    live: self.drivable(i),
                    value: f.display,
                    syncing: f.pickup.is_some(),
                    pickup_target: f.pickup.as_ref().map(|p| p.target),
                    pickup_position: f.pickup_position,
                    muted: f.cfg.muted,
                    max: self.cap(i),
                    step: f.cfg.step,
                    target: f.cfg.target,
                    app_key: f.cfg.app_key.clone(),
                    category_name: f.cfg.category_name.clone(),
                    active_output: self.resolve(i),
                    ranked_outputs: f.cfg.outputs.clone(),
                    status: self.target_status(i),
                    raw: f.last_raw,
                    units: source::units(f.key()),
                    cal_min: f.cfg.cal.min,
                    cal_max: f.cfg.cal.max,
                    taper: f.cfg.cal.taper,
                    inverted: f.cfg.cal.inverted,
                    mute_raw: f.cfg.cal.mute_raw,
                    raw_maximum: source::raw_maximum(f.key()),
                }
            })
            .collect();
        let mut apps: Vec<AppView> = self
            .settings
            .known_apps
            .iter()
            .map(|(key, name)| AppView {
                key: key.clone(),
                name: name.clone(),
                live: self.apps.contains_key(key),
            })
            .collect();
        apps.sort_by_key(|a| (!a.live, a.name.to_lowercase()));

        let mut sources: BTreeMap<String, SourceView> = BTreeMap::new();
        for (key, (name, connected)) in &self.sources {
            let picker = source::picker_device_key(key);
            let entry = sources.entry(picker.clone()).or_insert_with(|| SourceView {
                key: picker,
                name: name.clone(),
                connected: false,
                is_midi: source::is_midi(key),
            });
            entry.connected |= *connected;
        }
        let mut sources: Vec<SourceView> = sources.into_values().collect();
        sources.sort_by_key(|s| (!s.connected, s.name.to_lowercase()));

        let record = self.record.as_ref().map(|r| RecordView {
            id: r.id.clone(),
            samples: r.samples.len(),
            finished: r.result.is_some(),
            good: r.result.as_ref().is_some_and(|a| a.good),
            message: r
                .result
                .as_ref()
                .map(|a| a.message.clone())
                .unwrap_or_default(),
        });

        View {
            faders,
            outputs: self.outputs.clone(),
            apps,
            categories: self.settings.categories.clone(),
            sources,
            capture: self.capture_view(),
            record,
            soft_takeover: self.settings.soft_takeover,
            close_behavior: self.settings.close_behavior,
            audio_ready: self.audio_ready,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::Calibration;
    use crate::source::{MidiMode, midi_key};

    const HID: &str = "hid:1d50:615e:test";

    fn output(id: &str, volume: f32) -> OutputInfo {
        OutputInfo {
            id: id.into(),
            name: id.to_uppercase(),
            volume,
        }
    }

    fn app(key: &str, volume: f32) -> AppInfo {
        AppInfo {
            key: key.into(),
            name: key.into(),
            volume,
        }
    }

    fn pref(id: &str) -> OutputPref {
        OutputPref {
            id: id.into(),
            name: id.into(),
        }
    }

    fn straight() -> Calibration {
        Calibration {
            min: 0,
            max: 1000,
            taper: Taper::Straight,
            ..Default::default()
        }
    }

    fn physical(id: &str, key: &str, axis: i32) -> FaderConfig {
        FaderConfig {
            id: id.into(),
            source_device_key: Some(key.into()),
            source_device_name: Some("Device".into()),
            axis_index: axis,
            label: id.into(),
            cal: straight(),
            outputs: vec![pref("sink")],
            ..Default::default()
        }
    }

    fn virtual_fader(id: &str) -> FaderConfig {
        FaderConfig {
            id: id.into(),
            is_virtual: true,
            axis_index: -1,
            label: id.into(),
            outputs: vec![pref("sink")],
            ..Default::default()
        }
    }

    fn engine(layout: Vec<FaderConfig>) -> Engine {
        Engine::new(Settings {
            layout,
            soft_takeover: false,
            ..Default::default()
        })
    }

    /// Runs ticks long enough for the write limiter to release everything.
    fn settle(e: &mut Engine, now: &mut u64) {
        for _ in 0..40 {
            *now += 20;
            e.tick(*now);
        }
    }

    fn last_desired(e: &mut Engine) -> Option<DesiredVolumes> {
        e.take_commands().into_iter().rev().find_map(|c| match c {
            AudioCommand::Desired(d) => Some(d),
            _ => None,
        })
    }

    fn view<'a>(v: &'a View, id: &str) -> &'a FaderView {
        v.faders.iter().find(|f| f.id == id).unwrap()
    }

    #[test]
    fn hid_fader_drives_its_output() {
        let mut e = engine(vec![physical("a", HID, 0)]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.2)], vec![], 0);
        e.sample(HID, 0, 500, 100);
        let mut now = 100;
        settle(&mut e, &mut now);
        assert_eq!(last_desired(&mut e).unwrap().endpoints["sink"], 0.5);
    }

    #[test]
    fn noise_inside_the_band_cannot_flip_the_level() {
        let mut e = engine(vec![physical("a", HID, 0)]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.2)], vec![], 0);
        e.sample(HID, 0, 500, 0);
        for raw in [503, 497, 502, 498, 504] {
            e.sample(HID, 0, raw, 1);
        }
        assert_eq!(view(&e.view(), "a").value, 50);
    }

    #[test]
    fn a_large_jump_snaps_without_easing() {
        let mut e = engine(vec![physical("a", HID, 0)]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.2)], vec![], 0);
        e.sample(HID, 0, 900, 0);
        e.sample(HID, 0, 100, 1);
        assert_eq!(view(&e.view(), "a").value, 10);
    }

    #[test]
    fn output_cap_scales_the_throw() {
        let mut e = engine(vec![physical("a", HID, 0)]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.2)], vec![], 0);
        e.command(
            Command::SetMax {
                id: "a".into(),
                max: 60,
            },
            0,
        );
        e.sample(HID, 0, 500, 0);
        assert_eq!(view(&e.view(), "a").value, 30);
        assert_eq!(e.snapshot_settings().device_max["sink"], 60);
    }

    #[test]
    fn mute_zone_forces_zero_on_the_same_report() {
        let mut fader = physical("a", HID, 0);
        fader.cal.mute_raw = 100;
        let mut e = engine(vec![fader]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.2)], vec![], 0);
        e.sample(HID, 0, 500, 0);
        e.sample(HID, 0, 95, 1);
        assert_eq!(view(&e.view(), "a").value, 0);
        // Inside the exit band the latch holds.
        e.sample(HID, 0, 110, 2);
        assert_eq!(view(&e.view(), "a").value, 0);
        // Past it the fader returns, still smoothed because the move is small.
        e.sample(HID, 0, 130, 3);
        assert_eq!(view(&e.view(), "a").value, 10);
    }

    #[test]
    fn soft_takeover_waits_for_the_fader_to_cross() {
        let mut settings = Settings {
            layout: vec![physical("a", HID, 0)],
            ..Default::default()
        };
        settings.soft_takeover = true;
        let mut e = Engine::new(settings);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;

        e.sample(HID, 0, 100, 0);
        settle(&mut e, &mut now);
        let v = e.view();
        assert_eq!(view(&v, "a").pickup_target, Some(50));
        assert_eq!(view(&v, "a").pickup_position, Some(10));
        assert_eq!(view(&v, "a").value, 50);
        assert!(last_desired(&mut e).is_none_or(|d| d.endpoints.is_empty()));

        e.sample(HID, 0, 800, now);
        settle(&mut e, &mut now);
        assert_eq!(view(&e.view(), "a").pickup_target, None);
        assert_eq!(last_desired(&mut e).unwrap().endpoints["sink"], 0.8);
    }

    #[test]
    fn takeover_target_is_limited_by_the_cap() {
        let mut settings = Settings {
            layout: vec![physical("a", HID, 0)],
            ..Default::default()
        };
        settings.device_max.insert("sink".into(), 40);
        let mut e = Engine::new(settings);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.9)], vec![], 0);
        assert_eq!(view(&e.view(), "a").pickup_target, Some(40));
    }

    #[test]
    fn external_change_rearms_a_physical_owner() {
        let mut settings = Settings {
            layout: vec![physical("a", HID, 0)],
            ..Default::default()
        };
        settings.soft_takeover = true;
        let mut e = Engine::new(settings);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;
        e.sample(HID, 0, 500, 0);
        settle(&mut e, &mut now);
        e.take_commands();

        e.set_audio(vec![output("sink", 0.2)], vec![], now);
        e.external_change("sink", true, 0.2, now);
        let commands = e.take_commands();
        assert!(commands.contains(&AudioCommand::ResolveExternal {
            key: "sink".into(),
            is_endpoint: true,
            continue_driving: false
        }));
        assert!(commands.contains(&AudioCommand::Desired(DesiredVolumes::default())));
        assert_eq!(view(&e.view(), "a").pickup_target, Some(20));
    }

    #[test]
    fn external_change_moves_a_virtual_owner() {
        let mut e = engine(vec![virtual_fader("v")]);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        e.command(
            Command::SetVirtual {
                id: "v".into(),
                value: 70,
                committed: true,
            },
            0,
        );
        e.take_commands();
        e.external_change("sink", true, 0.33, 0);
        let commands = e.take_commands();
        assert!(commands.iter().any(|c| matches!(
            c,
            AudioCommand::ResolveExternal {
                continue_driving: true,
                ..
            }
        )));
        assert_eq!(view(&e.view(), "v").value, 33);
    }

    #[test]
    fn hotkeys_ramp_and_clamp() {
        let mut e = engine(vec![virtual_fader("v")]);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;
        settle(&mut e, &mut now);
        e.hotkey("v", HotkeyAction::Up, now);
        assert!(e.needs_fast_tick());
        settle(&mut e, &mut now);
        assert_eq!(view(&e.view(), "v").value, 55);
        assert!(!e.needs_fast_tick());
        for _ in 0..30 {
            e.hotkey("v", HotkeyAction::Up, now);
        }
        settle(&mut e, &mut now);
        assert_eq!(view(&e.view(), "v").value, 100);
    }

    #[test]
    fn mute_hotkey_restores_the_level() {
        let mut e = engine(vec![virtual_fader("v")]);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;
        e.hotkey("v", HotkeyAction::Mute, now);
        settle(&mut e, &mut now);
        assert_eq!(view(&e.view(), "v").value, 0);
        assert!(view(&e.view(), "v").muted);
        e.hotkey("v", HotkeyAction::Mute, now);
        settle(&mut e, &mut now);
        assert_eq!(view(&e.view(), "v").value, 50);
    }

    #[test]
    fn output_failover_follows_hotplug() {
        let mut fader = virtual_fader("v");
        fader.outputs = vec![pref("a"), pref("b")];
        let mut e = engine(vec![fader]);
        e.set_audio(vec![output("a", 0.1), output("b", 0.1)], vec![], 0);
        let mut now = 0;
        settle(&mut e, &mut now);
        assert!(last_desired(&mut e).unwrap().endpoints.contains_key("a"));
        e.set_audio(vec![output("b", 0.1)], vec![], now);
        settle(&mut e, &mut now);
        let desired = last_desired(&mut e).unwrap();
        assert!(desired.endpoints.contains_key("b") && !desired.endpoints.contains_key("a"));
    }

    #[test]
    fn newly_launched_app_gets_the_fader_level() {
        let mut fader = virtual_fader("v");
        fader.target = TargetKind::App;
        fader.app_key = Some("bin:game".into());
        fader.value = 30;
        let mut e = engine(vec![fader]);
        e.set_audio(vec![output("sink", 0.1)], vec![], 0);
        let mut now = 0;
        settle(&mut e, &mut now);
        e.set_audio(vec![output("sink", 0.1)], vec![app("bin:game", 1.0)], now);
        settle(&mut e, &mut now);
        assert_eq!(last_desired(&mut e).unwrap().apps["bin:game"], 0.3);
    }

    #[test]
    fn everything_else_skips_categorised_and_directly_targeted_apps() {
        let mut everything = virtual_fader("rest");
        everything.target = TargetKind::Category;
        everything.category_name = Some(UNASSIGNED_CATEGORY.into());
        everything.value = 40;
        let mut direct = virtual_fader("direct");
        direct.target = TargetKind::App;
        direct.app_key = Some("bin:discord".into());
        direct.value = 90;
        let mut settings = Settings {
            layout: vec![everything, direct],
            soft_takeover: false,
            ..Default::default()
        };
        settings.categories = vec![Category {
            name: "Games".into(),
            app_keys: vec!["bin:game".into()],
        }];
        let mut e = Engine::new(settings);
        e.set_audio(
            vec![],
            vec![
                app("bin:game", 1.0),
                app("bin:discord", 1.0),
                app("bin:firefox", 1.0),
            ],
            0,
        );
        let mut now = 0;
        settle(&mut e, &mut now);
        let desired = last_desired(&mut e).unwrap();
        assert_eq!(desired.apps["bin:firefox"], 0.4);
        assert_eq!(desired.apps["bin:discord"], 0.9);
        assert!(!desired.apps.contains_key("bin:game"));
    }

    #[test]
    fn most_recently_moved_fader_wins_an_overlap() {
        let mut e = engine(vec![virtual_fader("a"), virtual_fader("b")]);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;
        e.command(
            Command::SetVirtual {
                id: "b".into(),
                value: 20,
                committed: true,
            },
            now,
        );
        settle(&mut e, &mut now);
        e.command(
            Command::SetVirtual {
                id: "a".into(),
                value: 80,
                committed: true,
            },
            now,
        );
        settle(&mut e, &mut now);
        assert_eq!(last_desired(&mut e).unwrap().endpoints["sink"], 0.8);
    }

    #[test]
    fn midi_first_report_is_a_baseline_and_heartbeats_keep_it_live() {
        let key = midi_key("zodipact", MidiMode::Qmk);
        let mut e = engine(vec![physical("m", &key, 16)]);
        e.set_source(&key, "ZodiPact", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        assert!(!view(&e.view(), "m").connected);
        e.sample(&key, 16, 500, 10);
        assert!(view(&e.view(), "m").connected);
        assert_eq!(view(&e.view(), "m").value, 50);
    }

    #[test]
    fn qmk_silence_goes_offline_and_recovers_with_pickup() {
        let key = midi_key("zodipact", MidiMode::Qmk);
        let mut settings = Settings {
            layout: vec![physical("m", &key, 16)],
            ..Default::default()
        };
        settings.soft_takeover = false;
        let mut e = Engine::new(settings);
        e.set_source(&key, "ZodiPact", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        e.sample(&key, 16, 500, 0);
        e.tick(1);
        e.tick(QMK_TIMEOUT_MS + 2000);
        let v = e.view();
        assert!(!view(&v, "m").connected);
        assert_eq!(
            view(&v, "m").value,
            50,
            "keeps its last level, never invents zero"
        );
        e.sample(&key, 16, 600, QMK_TIMEOUT_MS + 3000);
        assert!(view(&e.view(), "m").connected);
    }

    #[test]
    fn generic_midi_has_no_inactivity_timeout() {
        let key = midi_key("generic", MidiMode::Cc14);
        let mut e = engine(vec![physical("m", &key, 1)]);
        e.set_source(&key, "Generic", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        e.sample(&key, 1, 500, 0);
        e.tick(1);
        e.tick(60_000);
        assert!(view(&e.view(), "m").connected);
    }

    #[test]
    fn unplugging_drops_the_target_from_desired() {
        let mut e = engine(vec![physical("a", HID, 0)]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;
        e.sample(HID, 0, 500, 0);
        settle(&mut e, &mut now);
        e.take_commands();
        e.set_source(HID, "Dongle", false);
        assert_eq!(last_desired(&mut e), Some(DesiredVolumes::default()));
        assert!(view(&e.view(), "a").source.ends_with("Offline"));
    }

    #[test]
    fn capture_finds_the_swept_control() {
        let key = midi_key("zodipact", MidiMode::Cc14);
        let mut e = engine(vec![]);
        e.set_source(&key, "ZodiPact", true);
        e.command(
            Command::StartCapture {
                key: key.clone(),
                editing: None,
            },
            0,
        );
        assert_eq!(e.wanted_sources(), vec![key.clone()]);
        e.sample(&key, 3, 8000, 0);
        e.sample(&key, 17, 100, 0);
        e.sample(&key, 17, 13000, 1);
        let view_now = e.view();
        assert_eq!(
            view_now.capture.unwrap().detected.unwrap(),
            "Ch 1 · CC 17/49"
        );
        e.command(Command::CommitCapture, 2);
        let s = e.snapshot_settings();
        assert_eq!(s.layout.len(), 1);
        assert_eq!(
            (
                s.layout[0].axis_index,
                s.layout[0].cal.min,
                s.layout[0].cal.max
            ),
            (17, 100, 13000)
        );
        assert!(e.view().capture.is_none());
    }

    #[test]
    fn capture_needs_real_travel_and_skips_used_controls() {
        let key = midi_key("zodipact", MidiMode::Cc14);
        let mut e = engine(vec![physical("a", &key, 17)]);
        e.set_source(&key, "ZodiPact", true);
        e.command(
            Command::StartCapture {
                key: key.clone(),
                editing: None,
            },
            0,
        );
        e.sample(&key, 17, 0, 0);
        e.sample(&key, 17, 13000, 0);
        e.sample(&key, 18, 5000, 0);
        e.sample(&key, 18, 5200, 0);
        assert!(e.view().capture.unwrap().detected.is_none());
        e.command(Command::CommitCapture, 1);
        assert_eq!(e.snapshot_settings().layout.len(), 1);
    }

    #[test]
    fn rebinding_keeps_the_target_settings() {
        let key = midi_key("zodipact", MidiMode::Cc14);
        let mut fader = physical("a", HID, 0);
        fader.target = TargetKind::App;
        fader.app_key = Some("bin:game".into());
        let mut e = engine(vec![fader]);
        e.set_source(&key, "ZodiPact", true);
        e.command(
            Command::StartCapture {
                key: key.clone(),
                editing: Some("a".into()),
            },
            0,
        );
        e.sample(&key, 5, 0, 0);
        e.sample(&key, 5, 9000, 0);
        e.command(Command::CommitCapture, 1);
        let s = e.snapshot_settings();
        assert_eq!(s.layout.len(), 1);
        assert_eq!(s.layout[0].source_device_key.as_deref(), Some(key.as_str()));
        assert_eq!(s.layout[0].app_key.as_deref(), Some("bin:game"));
    }

    #[test]
    fn recording_shows_movement_without_driving_volume() {
        let mut e = engine(vec![physical("a", HID, 0)]);
        e.set_source(HID, "Dongle", true);
        e.set_audio(vec![output("sink", 0.5)], vec![], 0);
        let mut now = 0;
        e.sample(HID, 0, 500, now);
        settle(&mut e, &mut now);
        e.take_commands();

        e.command(Command::StartRecord { id: "a".into() }, now);
        for raw in (10..=1000).step_by(30) {
            e.sample(HID, 0, raw, now);
        }
        for _ in 0..8 {
            e.sample(HID, 0, 10, now);
        }
        settle(&mut e, &mut now);
        assert!(last_desired(&mut e).is_none());
        assert_eq!(view(&e.view(), "a").value, 1);

        e.command(Command::StopRecord, now);
        let record = e.view().record.unwrap();
        assert!(record.finished && record.good, "{}", record.message);
        e.command(Command::ApplyRecord, now);
        let cal = &e.snapshot_settings().layout[0].cal;
        assert_eq!((cal.min, cal.max), (10, 1000));
        assert!(e.view().record.is_none());
    }

    #[test]
    fn commands_parse_from_ui_json() {
        let cmd: Command =
            serde_json::from_str(r#"{"cmd":"setVirtual","id":"a","value":5,"committed":false}"#)
                .unwrap();
        assert_eq!(
            cmd,
            Command::SetVirtual {
                id: "a".into(),
                value: 5,
                committed: false
            }
        );
        let cmd: Command =
            serde_json::from_str(r#"{"cmd":"setCalibration","id":"a","taper":"audioReversed","inverted":true,"muteRaw":9}"#).unwrap();
        assert_eq!(
            cmd,
            Command::SetCalibration {
                id: "a".into(),
                taper: Taper::AudioReversed,
                inverted: true,
                mute_raw: 9
            }
        );
    }

    #[test]
    fn edits_save_after_a_debounce() {
        let mut e = engine(vec![]);
        e.command(Command::AddVirtual, 1000);
        assert!(e.take_save(1000 + SAVE_DEBOUNCE_MS - 1).is_none());
        let saved = e.take_save(1000 + SAVE_DEBOUNCE_MS).unwrap();
        assert_eq!(saved.layout.len(), 1);
        assert!(e.take_save(10_000).is_none());
    }
}
