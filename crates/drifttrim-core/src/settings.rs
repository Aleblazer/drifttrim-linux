//! Persistent settings: the fader layout, categories and app preferences.
//!
//! Stored as JSON under `$XDG_CONFIG_HOME/drifttrim/`. Everything read from
//! disk passes through [`normalize`], so a hand-edited or damaged file cannot
//! put impossible values into the running app.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::calibration::Calibration;
use crate::source;

pub const CURRENT_SCHEMA: u32 = 1;
/// Category name meaning every running app not otherwise assigned.
pub const UNASSIGNED_CATEGORY: &str = "#unassigned";

const MAX_SETTINGS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LAYOUT_FADERS: usize = 128;
const MAX_CATEGORIES: usize = 128;
const MAX_KNOWN_APPS: usize = 4096;
const MAX_OUTPUT_PREFS: usize = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetKind {
    #[default]
    Output,
    App,
    Category,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CloseBehavior {
    #[default]
    Ask,
    Tray,
    Exit,
}

/// A ranked output. The name is kept so an unplugged device still reads well.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputPref {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Category {
    pub name: String,
    pub app_keys: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FaderConfig {
    pub id: String,
    pub source_device_key: Option<String>,
    pub source_device_name: Option<String>,
    pub axis_index: i32,
    pub label: String,
    pub cal: Calibration,
    pub outputs: Vec<OutputPref>,
    pub override_id: Option<String>,
    pub target: TargetKind,
    pub app_key: Option<String>,
    pub category_name: Option<String>,
    pub max: i32,
    pub is_virtual: bool,
    pub value: i32,
    pub step: i32,
    pub muted: bool,
    pub pre_mute: i32,
}

impl Default for FaderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            source_device_key: None,
            source_device_name: None,
            axis_index: 0,
            label: String::new(),
            cal: Calibration::default(),
            outputs: Vec::new(),
            override_id: None,
            target: TargetKind::Output,
            app_key: None,
            category_name: None,
            max: 100,
            is_virtual: false,
            value: 50,
            step: 5,
            muted: false,
            pre_mute: 50,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub schema_version: u32,
    pub layout: Vec<FaderConfig>,
    /// Remembered Max % cap for each output device.
    pub device_max: BTreeMap<String, i32>,
    pub known_apps: BTreeMap<String, String>,
    pub app_seen: BTreeMap<String, i64>,
    pub categories: Vec<Category>,
    pub close_behavior: CloseBehavior,
    pub soft_takeover: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA,
            layout: Vec::new(),
            device_max: BTreeMap::new(),
            known_apps: BTreeMap::new(),
            app_seen: BTreeMap::new(),
            categories: Vec::new(),
            close_behavior: CloseBehavior::Ask,
            soft_takeover: true,
        }
    }
}

fn clean(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn clean_opt(value: Option<&str>, max_chars: usize) -> Option<String> {
    value.map(|v| clean(v, max_chars)).filter(|v| !v.is_empty())
}

fn normalize_calibration(cal: &mut Calibration) {
    let limit = u16::MAX as i32;
    cal.min = cal.min.clamp(0, limit);
    cal.max = cal.max.clamp(0, limit);
    cal.mute_raw = cal.mute_raw.clamp(0, limit);
}

fn normalize_outputs(outputs: Vec<OutputPref>) -> Vec<OutputPref> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for output in outputs {
        if normalized.len() >= MAX_OUTPUT_PREFS {
            break;
        }
        let id = clean(&output.id, 1024);
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        let name = clean(&output.name, 256);
        let name = if name.is_empty() { id.clone() } else { name };
        normalized.push(OutputPref { id, name });
    }
    normalized
}

fn normalize_fader(mut fader: FaderConfig) -> FaderConfig {
    fader.id = clean(&fader.id, 64);
    fader.source_device_key =
        clean_opt(fader.source_device_key.as_deref(), 512).map(|key| source::canonical_key(&key));
    fader.source_device_name = clean_opt(fader.source_device_name.as_deref(), 160);
    fader.label = clean(&fader.label, 80);
    normalize_calibration(&mut fader.cal);
    fader.outputs = normalize_outputs(std::mem::take(&mut fader.outputs));
    fader.override_id = clean_opt(fader.override_id.as_deref(), 1024);
    fader.app_key = clean_opt(fader.app_key.as_deref(), 1024);
    fader.category_name = clean_opt(fader.category_name.as_deref(), 80);
    fader.max = fader.max.clamp(1, 100);
    fader.value = fader.value.clamp(0, 100);
    fader.pre_mute = fader.pre_mute.clamp(0, 100);
    fader.step = fader.step.clamp(1, 100);
    if (fader.target == TargetKind::App && fader.app_key.is_none())
        || (fader.target == TargetKind::Category && fader.category_name.is_none())
    {
        fader.target = TargetKind::Output;
    }
    if fader.is_virtual {
        fader.source_device_key = None;
        fader.source_device_name = None;
        fader.axis_index = -1;
    } else {
        let axes = source::axes(fader.source_device_key.as_deref().unwrap_or("")) as i32;
        fader.axis_index = fader.axis_index.clamp(0, axes - 1);
    }
    if fader.label.is_empty() {
        fader.label = if fader.is_virtual {
            "Virtual fader".to_owned()
        } else {
            format!("Fader {}", fader.axis_index + 1)
        };
    }
    fader
}

pub fn new_fader_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Enforces limits and repairs anything inconsistent.
pub fn normalize(mut settings: Settings) -> Settings {
    let mut layout = Vec::new();
    let mut ids = HashSet::new();
    let mut bindings = HashSet::new();
    for fader in std::mem::take(&mut settings.layout) {
        if layout.len() >= MAX_LAYOUT_FADERS {
            break;
        }
        let mut fader = normalize_fader(fader);
        if !fader.is_virtual {
            // A physical fader needs a source, and each control is used once.
            let Some(key) = fader.source_device_key.as_deref() else {
                continue;
            };
            if !bindings.insert(source::binding_key(key, fader.axis_index as usize)) {
                continue;
            }
        }
        if fader.id.is_empty() || !ids.insert(fader.id.to_lowercase()) {
            loop {
                fader.id = new_fader_id();
                if ids.insert(fader.id.clone()) {
                    break;
                }
            }
        }
        layout.push(fader);
    }
    settings.layout = layout;
    settings.schema_version = CURRENT_SCHEMA;

    let mut device_max = BTreeMap::new();
    for (key, cap) in std::mem::take(&mut settings.device_max) {
        if device_max.len() >= MAX_OUTPUT_PREFS * 4 {
            break;
        }
        let key = clean(&key, 1024);
        if !key.is_empty() {
            device_max.insert(key, cap.clamp(1, 100));
        }
    }
    settings.device_max = device_max;

    let mut known_apps = BTreeMap::new();
    for (key, name) in std::mem::take(&mut settings.known_apps) {
        if known_apps.len() >= MAX_KNOWN_APPS {
            break;
        }
        let key = clean(&key, 1024);
        if key.is_empty() {
            continue;
        }
        let name = clean(&name, 256);
        let name = if name.is_empty() { key.clone() } else { name };
        known_apps.insert(key, name);
    }
    settings.known_apps = known_apps;
    let known = &settings.known_apps;
    settings
        .app_seen
        .retain(|key, seen| known.contains_key(key) && *seen >= 0);

    let mut categories = Vec::new();
    let mut names = HashSet::new();
    for mut category in std::mem::take(&mut settings.categories) {
        if categories.len() >= MAX_CATEGORIES {
            break;
        }
        let name = clean(&category.name, 80);
        if name.is_empty()
            || name.eq_ignore_ascii_case(UNASSIGNED_CATEGORY)
            || !names.insert(name.to_lowercase())
        {
            continue;
        }
        category.name = name;
        let mut seen = HashSet::new();
        category.app_keys = category
            .app_keys
            .iter()
            .map(|key| clean(key, 1024))
            .filter(|key| !key.is_empty() && seen.insert(key.clone()))
            .take(MAX_KNOWN_APPS)
            .collect();
        categories.push(category);
    }
    settings.categories = categories;
    settings
}

/// `$XDG_CONFIG_HOME/drifttrim/settings.json`, falling back to `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("drifttrim").join("settings.json"))
}

#[derive(Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// Set when a damaged file was moved aside and defaults were used.
    pub recovered_to: Option<PathBuf>,
}

/// Loads settings. A missing file gives defaults; an unreadable one is kept
/// beside the original so the user's layout is never silently destroyed.
pub fn load(path: &Path) -> io::Result<Loaded> {
    let bytes = match read_limited(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(Loaded {
                settings: Settings::default(),
                recovered_to: None,
            });
        }
        Err(err) if err.kind() == io::ErrorKind::InvalidData => return recover(path),
        Err(err) => return Err(err),
    };
    match serde_json::from_slice::<Settings>(&bytes) {
        Ok(settings) => Ok(Loaded {
            settings: normalize(settings),
            recovered_to: None,
        }),
        Err(_) => recover(path),
    }
}

fn read_limited(path: &Path) -> io::Result<Vec<u8>> {
    if fs::metadata(path)?.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "settings file is too large",
        ));
    }
    fs::read(path)
}

fn recover(path: &Path) -> io::Result<Loaded> {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".corrupt");
    let backup = PathBuf::from(backup);
    fs::rename(path, &backup)?;
    Ok(Loaded {
        settings: Settings::default(),
        recovered_to: Some(backup),
    })
}

/// Writes atomically, so a crash or power cut cannot leave a half-written file.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("settings path has no parent"))?;
    fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::Taper;
    use crate::source::{MidiMode, midi_key};

    fn physical(key: &str, axis: i32) -> FaderConfig {
        FaderConfig {
            source_device_key: Some(key.to_owned()),
            axis_index: axis,
            ..Default::default()
        }
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let fader = FaderConfig {
            is_virtual: true,
            max: 500,
            value: -3,
            pre_mute: 900,
            step: 0,
            ..Default::default()
        };
        let s = normalize(Settings {
            layout: vec![fader],
            ..Default::default()
        });
        let f = &s.layout[0];
        assert_eq!((f.max, f.value, f.pre_mute, f.step), (100, 0, 100, 1));
        assert_eq!(f.axis_index, -1);
        assert_eq!(f.label, "Virtual fader");
    }

    #[test]
    fn duplicate_controls_are_dropped_across_profiles() {
        let cc14 = midi_key("zodipact", MidiMode::Cc14);
        let qmk = midi_key("zodipact", MidiMode::Qmk);
        let layout = vec![physical(&cc14, 16), physical(&qmk, 16), physical(&cc14, 17)];
        let s = normalize(Settings {
            layout,
            ..Default::default()
        });
        assert_eq!(s.layout.len(), 2);
    }

    #[test]
    fn physical_fader_without_a_source_is_removed() {
        let s = normalize(Settings {
            layout: vec![FaderConfig::default()],
            ..Default::default()
        });
        assert!(s.layout.is_empty());
    }

    #[test]
    fn targets_missing_their_subject_fall_back_to_output() {
        let fader = FaderConfig {
            is_virtual: true,
            target: TargetKind::App,
            ..Default::default()
        };
        let s = normalize(Settings {
            layout: vec![fader],
            ..Default::default()
        });
        assert_eq!(s.layout[0].target, TargetKind::Output);
    }

    #[test]
    fn duplicate_or_blank_ids_are_replaced() {
        let fader = FaderConfig {
            id: "a".into(),
            is_virtual: true,
            ..Default::default()
        };
        let blank = FaderConfig {
            is_virtual: true,
            ..Default::default()
        };
        let s = normalize(Settings {
            layout: vec![fader.clone(), fader, blank],
            ..Default::default()
        });
        let ids: HashSet<_> = s.layout.iter().map(|f| f.id.clone()).collect();
        assert_eq!(ids.len(), 3);
        assert!(s.layout.iter().all(|f| !f.id.is_empty()));
    }

    #[test]
    fn layout_is_capped() {
        let fader = FaderConfig {
            is_virtual: true,
            ..Default::default()
        };
        let s = normalize(Settings {
            layout: vec![fader; 200],
            ..Default::default()
        });
        assert_eq!(s.layout.len(), MAX_LAYOUT_FADERS);
    }

    #[test]
    fn reserved_and_duplicate_categories_are_removed() {
        let category = |name: &str| Category {
            name: name.into(),
            app_keys: vec![],
        };
        let s = normalize(Settings {
            categories: vec![
                category("Games"),
                category("games"),
                category("#Unassigned"),
                category(" "),
            ],
            ..Default::default()
        });
        assert_eq!(s.categories.len(), 1);
        assert_eq!(s.categories[0].name, "Games");
    }

    #[test]
    fn app_seen_only_keeps_known_apps() {
        let s = normalize(Settings {
            known_apps: BTreeMap::from([("bin:a".to_owned(), String::new())]),
            app_seen: BTreeMap::from([("bin:a".to_owned(), 5), ("bin:b".to_owned(), 5)]),
            ..Default::default()
        });
        assert_eq!(s.known_apps["bin:a"], "bin:a");
        assert_eq!(s.app_seen.len(), 1);
    }

    #[test]
    fn output_caps_are_clamped() {
        let s = normalize(Settings {
            device_max: BTreeMap::from([
                ("a".to_owned(), 0),
                ("b".to_owned(), 900),
                (" ".to_owned(), 5),
            ]),
            ..Default::default()
        });
        assert_eq!(
            s.device_max,
            BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 100)])
        );
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load(&dir.path().join("settings.json")).unwrap();
        assert_eq!(loaded.settings, Settings::default());
        assert!(loaded.recovered_to.is_none());
    }

    #[test]
    fn save_then_load_round_trips_mixed_sources() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("settings.json");
        let mut midi = physical(&midi_key("zodipact", MidiMode::Qmk), 500);
        midi.cal.taper = Taper::Straight;
        midi.cal.inverted = true;
        midi.outputs = vec![OutputPref {
            id: "sink.a".into(),
            name: "Speakers".into(),
        }];
        let virtual_fader = FaderConfig {
            is_virtual: true,
            target: TargetKind::Category,
            category_name: Some(UNASSIGNED_CATEGORY.into()),
            ..Default::default()
        };
        let settings = normalize(Settings {
            layout: vec![midi, physical("hid:1d50:615e:x", 3), virtual_fader],
            ..Default::default()
        });
        save(&path, &settings).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.settings, settings);
        assert_eq!(loaded.settings.layout[0].axis_index, 500);
    }

    #[test]
    fn corrupt_file_is_moved_aside_not_lost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"{ not json").unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.settings, Settings::default());
        let backup = loaded.recovered_to.unwrap();
        assert_eq!(fs::read(backup).unwrap(), b"{ not json");
        assert!(!path.exists());
    }

    #[test]
    fn unknown_and_missing_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, br#"{"somethingNew":1,"softTakeover":false}"#).unwrap();
        let loaded = load(&path).unwrap();
        assert!(!loaded.settings.soft_takeover);
        assert_eq!(loaded.settings.schema_version, CURRENT_SCHEMA);
    }
}
