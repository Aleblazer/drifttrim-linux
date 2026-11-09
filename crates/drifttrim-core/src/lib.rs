//! Platform-independent fader logic, ported from the Windows DriftTrim app.
//!
//! Nothing in this crate touches Qt, PipeWire, ALSA or hidraw, so every rule
//! here can be covered by plain `cargo test`.

pub mod calibration;
pub mod capture;
pub mod engine;
pub mod expected;
pub mod identity;
pub mod logic;
pub mod midi;
pub mod settings;
pub mod source;
