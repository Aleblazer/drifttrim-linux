//! Stable identity for an audio-playing app across restarts and streams.

/// Runtimes whose binary name says nothing about which app is running.
const GENERIC_BINARIES: &[&str] = &[
    "electron",
    "python",
    "python3",
    "java",
    "node",
    "wine",
    "wine64",
    "wine-preloader",
    "wine64-preloader",
    "bwrap",
    "gjs",
    "sh",
    "bash",
];

/// Library or engine names that apps report instead of their own.
const ENGINE_NAMES: &[&str] = &[
    "webrtc voiceengine",
    "alsa plug-in",
    "playstream",
    "libcanberra",
];

fn is_generic_binary(binary: &str) -> bool {
    let binary = binary.to_lowercase();
    GENERIC_BINARIES.contains(&binary.as_str()) || binary.starts_with("python3.")
}

fn is_engine_name(name: &str) -> bool {
    let name = name.to_lowercase();
    ENGINE_NAMES.iter().any(|engine| name.starts_with(engine))
}

fn usable(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Builds the durable key used to save an app target. Sandbox ids win, then a
/// meaningful binary name, then the name the app reports.
pub fn app_key(
    flatpak_id: Option<&str>,
    binary: Option<&str>,
    name: Option<&str>,
) -> Option<String> {
    if let Some(id) = usable(flatpak_id) {
        return Some(format!("flatpak:{}", id.to_lowercase()));
    }
    let binary = usable(binary);
    if let Some(binary) = binary.filter(|b| !is_generic_binary(b)) {
        return Some(format!("bin:{}", binary.to_lowercase()));
    }
    if let Some(name) = usable(name) {
        return Some(format!("name:{}", name.to_lowercase()));
    }
    binary.map(|b| format!("bin:{}", b.to_lowercase()))
}

/// Picks the friendliest label. Discord, for one, reports its voice engine as
/// the application name, so the binary reads better there.
pub fn app_display_name(binary: Option<&str>, name: Option<&str>) -> String {
    let binary = usable(binary);
    match usable(name) {
        Some(name) if !is_engine_name(name) => name.to_owned(),
        fallback => binary.or(fallback).unwrap_or("Unknown app").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatpak_id_beats_everything() {
        assert_eq!(
            app_key(Some("com.Spotify.Client"), Some("spotify"), Some("Spotify")),
            Some("flatpak:com.spotify.client".to_owned())
        );
    }

    #[test]
    fn binary_is_used_when_meaningful() {
        assert_eq!(
            app_key(None, Some("Discord"), Some("WEBRTC VoiceEngine")),
            Some("bin:discord".to_owned())
        );
    }

    #[test]
    fn generic_runtimes_fall_back_to_the_app_name() {
        assert_eq!(
            app_key(None, Some("electron"), Some("Slack")),
            Some("name:slack".to_owned())
        );
        assert_eq!(
            app_key(None, Some("python3.14"), Some("mpd")),
            Some("name:mpd".to_owned())
        );
    }

    #[test]
    fn generic_binary_is_better_than_nothing() {
        assert_eq!(
            app_key(None, Some("java"), None),
            Some("bin:java".to_owned())
        );
        assert_eq!(app_key(None, None, Some("  ")), None);
    }

    #[test]
    fn engine_names_are_replaced_for_display() {
        assert_eq!(
            app_display_name(Some("Discord"), Some("WEBRTC VoiceEngine")),
            "Discord"
        );
        assert_eq!(
            app_display_name(Some("firefox"), Some("Firefox")),
            "Firefox"
        );
        assert_eq!(
            app_display_name(None, Some("ALSA plug-in [mpv]")),
            "ALSA plug-in [mpv]"
        );
        assert_eq!(app_display_name(None, None), "Unknown app");
    }
}
