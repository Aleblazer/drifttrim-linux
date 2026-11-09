//! Transport-independent source identity, control addressing and units.

use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MidiMode {
    Qmk,
    Cc14,
    Cc7,
}

impl MidiMode {
    pub const ALL: [MidiMode; 3] = [MidiMode::Qmk, MidiMode::Cc14, MidiMode::Cc7];

    fn tag(self) -> &'static str {
        match self {
            MidiMode::Qmk => "qmk",
            MidiMode::Cc14 => "cc14",
            MidiMode::Cc7 => "cc7",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|m| m.tag().eq_ignore_ascii_case(tag))
    }

    pub fn label(self) -> &'static str {
        match self {
            MidiMode::Qmk => "14-bit CC (QMK/VIK heartbeat)",
            MidiMode::Cc7 => "7-bit CC",
            MidiMode::Cc14 => "14-bit CC",
        }
    }

    /// Number of addressable controls: channels times controllers.
    fn width(self) -> usize {
        if self == MidiMode::Cc7 { 128 } else { 32 }
    }
}

fn hex_upper(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Key for a MIDI endpoint in one profile. The endpoint id is hashed so a
/// saved binding never depends on how the id happens to be spelled.
pub fn midi_key(endpoint_id: &str, mode: MidiMode) -> String {
    let digest = Sha256::digest(endpoint_id.to_uppercase().as_bytes());
    format!("midi:{}:{}", mode.tag(), hex_upper(&digest))
}

/// Parses a MIDI key into its mode and hash. Other keys return `None`.
fn parse_midi(key: &str) -> Option<(MidiMode, &str)> {
    let mut parts = key.split(':');
    let (scheme, mode, hash) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !scheme.eq_ignore_ascii_case("midi") {
        return None;
    }
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((MidiMode::from_tag(mode)?, hash))
}

pub fn midi_mode(key: &str) -> Option<MidiMode> {
    parse_midi(key).map(|(mode, _)| mode)
}

pub fn is_midi(key: &str) -> bool {
    midi_mode(key).is_some()
}

pub fn is_qmk(key: &str) -> bool {
    midi_mode(key) == Some(MidiMode::Qmk)
}

pub fn canonical_key(key: &str) -> String {
    match parse_midi(key) {
        Some((mode, hash)) => format!("midi:{}:{}", mode.tag(), hash.to_uppercase()),
        None => key.to_owned(),
    }
}

pub fn with_midi_mode(key: &str, mode: MidiMode) -> String {
    match parse_midi(key) {
        Some((_, hash)) => format!("midi:{}:{}", mode.tag(), hash.to_uppercase()),
        None => key.to_owned(),
    }
}

/// The device picker lists each MIDI device once, under its 14-bit key.
pub fn picker_device_key(key: &str) -> String {
    with_midi_mode(key, MidiMode::Cc14)
}

pub fn axes(key: &str) -> usize {
    midi_mode(key).map_or(8, |mode| 16 * mode.width())
}

pub fn raw_maximum(key: &str) -> i32 {
    match midi_mode(key) {
        Some(MidiMode::Cc7) => 127,
        Some(_) => 16383,
        None => 3250,
    }
}

pub fn travel_threshold(key: &str) -> i32 {
    if is_midi(key) {
        (raw_maximum(key) / 12).max(16)
    } else {
        1500
    }
}

pub fn units(key: &str) -> &'static str {
    if is_midi(key) { "counts" } else { "mV" }
}

pub fn control_name(key: &str, axis: usize) -> String {
    let Some(mode) = midi_mode(key) else {
        return format!("axis {}", axis + 1);
    };
    let width = mode.width();
    let cc = axis % width;
    let channel = axis / width + 1;
    if width == 32 {
        format!("Ch {channel} · CC {cc}/{}", cc + 32)
    } else {
        format!("Ch {channel} · CC {cc}")
    }
}

/// Identity of one physical control, used to refuse duplicate assignments. A
/// coarse and fine pair cannot be assigned again through another MIDI profile.
pub fn binding_key(key: &str, axis: usize) -> String {
    let Some((mode, hash)) = parse_midi(key) else {
        return format!("{}\n{axis}", key.trim());
    };
    let width = mode.width();
    let mut cc = axis % width;
    if (32..64).contains(&cc) {
        cc -= 32;
    }
    format!("midi:{}\n{}:{cc}", hash.to_uppercase(), axis / width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_keys_ignore_endpoint_case_and_carry_the_mode() {
        let a = midi_key("ZodiPact:MIDI 1", MidiMode::Cc14);
        assert_eq!(a, midi_key("zodipact:midi 1", MidiMode::Cc14));
        assert_ne!(a, midi_key("ZodiPact:MIDI 1", MidiMode::Qmk));
        assert_eq!(midi_mode(&a), Some(MidiMode::Cc14));
    }

    #[test]
    fn hid_keys_are_not_midi() {
        assert!(!is_midi("hid:1d50:615e:abc"));
        assert_eq!(axes("hid:1d50:615e:abc"), 8);
        assert_eq!(raw_maximum("hid:1d50:615e:abc"), 3250);
        assert_eq!(units("hid:1d50:615e:abc"), "mV");
    }

    #[test]
    fn malformed_midi_keys_are_rejected() {
        assert!(!is_midi("midi:cc14:ABC"));
        assert!(!is_midi("midi:nope:".to_string().as_str()));
        let bad_hash = format!("midi:cc14:{}", "Z".repeat(64));
        assert!(!is_midi(&bad_hash));
    }

    #[test]
    fn canonical_key_normalises_case() {
        let key = midi_key("x", MidiMode::Cc7);
        let lower = key.to_lowercase();
        assert_eq!(canonical_key(&lower), key);
    }

    #[test]
    fn axis_counts_and_ranges_per_profile() {
        let cc7 = midi_key("x", MidiMode::Cc7);
        let cc14 = midi_key("x", MidiMode::Cc14);
        assert_eq!((axes(&cc7), raw_maximum(&cc7)), (2048, 127));
        assert_eq!((axes(&cc14), raw_maximum(&cc14)), (512, 16383));
        assert_eq!(travel_threshold(&cc7), 16);
        assert_eq!(travel_threshold(&cc14), 1365);
    }

    #[test]
    fn control_names_show_channel_and_pair() {
        assert_eq!(
            control_name(&midi_key("x", MidiMode::Cc14), 32 + 16),
            "Ch 2 · CC 16/48"
        );
        assert_eq!(
            control_name(&midi_key("x", MidiMode::Cc7), 7),
            "Ch 1 · CC 7"
        );
        assert_eq!(control_name("hid:a", 1), "axis 2");
    }

    #[test]
    fn same_pair_collides_across_profiles() {
        let cc14 = midi_key("x", MidiMode::Cc14);
        let qmk = midi_key("x", MidiMode::Qmk);
        let cc7 = midi_key("x", MidiMode::Cc7);
        assert_eq!(binding_key(&cc14, 16), binding_key(&qmk, 16));
        assert_eq!(binding_key(&cc14, 16), binding_key(&cc7, 16));
        assert_eq!(binding_key(&cc14, 16), binding_key(&cc7, 48));
        assert_ne!(binding_key(&cc14, 16), binding_key(&cc14, 17));
    }
}
