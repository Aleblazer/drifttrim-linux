//! ZMK vendor-HID faders through hidraw, rescanned on udev hotplug events.

use std::collections::{HashMap, HashSet};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread;

use hidapi::{DeviceInfo, HidApi};

use crate::runtime::Event;

const ZMK_VID: u16 = 0x1D50;
const ZMK_PID: u16 = 0x615E;
const VENDOR_PAGE: u16 = 0xFF00;
const FADER_USAGE: u16 = 0x0001;
const FADER_REPORT: u8 = 2;
const MAX_AXES: usize = 8;
/// Retry cadence while a device exists that could not be opened.
const RETRY_MS: i32 = 5000;

/// Decodes one report: id 2, then up to eight signed 16-bit little-endian axes.
fn parse_report(report: &[u8]) -> Vec<i32> {
    if report.len() < 3 || report[0] != FADER_REPORT {
        return Vec::new();
    }
    report[1..]
        .as_chunks::<2>()
        .0
        .iter()
        .take(MAX_AXES)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]) as i32)
        .collect()
}

/// True if the descriptor has 16-bit input report 2 inside a fader collection.
/// Other devices, such as Steam controllers, also use the vendor usage.
fn has_fader_report(descriptor: &[u8]) -> bool {
    let (mut index, mut page, mut usage, mut depth, mut in_fader) = (0, 0u32, 0u32, 0i32, false);
    let (mut report, mut bits) = (0u32, 0u32);
    while index < descriptor.len() {
        let prefix = descriptor[index];
        if prefix == 0xFE {
            // Long item: a length byte, a tag byte, then the data.
            let Some(length) = descriptor.get(index + 1) else {
                break;
            };
            index += 3 + *length as usize;
            continue;
        }
        let size = if prefix & 3 == 3 {
            4
        } else {
            (prefix & 3) as usize
        };
        let Some(bytes) = descriptor.get(index + 1..index + 1 + size) else {
            break;
        };
        let data = bytes
            .iter()
            .rev()
            .fold(0u32, |value, byte| (value << 8) | *byte as u32);
        match prefix & 0xFC {
            0x04 => page = data,
            0x08 => usage = data,
            0xA0 => {
                if depth == 0 {
                    in_fader = page == VENDOR_PAGE as u32 && usage == FADER_USAGE as u32;
                }
                depth += 1;
                usage = 0;
            }
            0xC0 => depth -= 1,
            0x84 => report = data,
            0x74 => bits = data,
            0x80 if in_fader && report == FADER_REPORT as u32 && bits == 16 => return true,
            _ => {}
        }
        index += 1 + size;
    }
    false
}

/// Reads the descriptor from sysfs, which needs no access to the device itself.
fn read_descriptor(info: &DeviceInfo) -> Option<Vec<u8>> {
    let path = info.path().to_str().ok()?;
    let node = path.rsplit('/').next()?;
    std::fs::read(format!("/sys/class/hidraw/{node}/device/report_descriptor")).ok()
}

fn is_fader(info: &DeviceInfo) -> bool {
    if info.usage_page() != VENDOR_PAGE {
        return false;
    }
    let zmk_ids = info.vendor_id() == ZMK_VID && info.product_id() == ZMK_PID;
    if info.usage() != FADER_USAGE && !zmk_ids {
        return false;
    }
    read_descriptor(info).map_or(zmk_ids, |descriptor| has_fader_report(&descriptor))
}

fn key_for(info: &DeviceInfo) -> String {
    let unit = info
        .serial_number()
        .filter(|s| !s.is_empty())
        .or(info.product_string())
        .unwrap_or("unknown");
    format!(
        "hid:{:04x}:{:04x}:{unit}",
        info.vendor_id(),
        info.product_id()
    )
}

pub fn start(events: Sender<Event>) {
    let _ = thread::Builder::new().name("hid".into()).spawn(move || {
        if let Err(err) = run(events) {
            eprintln!("drifttrim: HID input stopped: {err}");
        }
    });
}

struct Reader {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

fn run(events: Sender<Event>) -> Result<(), Box<dyn std::error::Error>> {
    let mut api = HidApi::new()?;
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("hidraw")?
        .listen()?;
    let mut readers: HashMap<String, Reader> = HashMap::new();
    let mut named: HashMap<String, String> = HashMap::new();
    let mut warned: HashSet<String> = HashSet::new();

    loop {
        readers.retain(|_, reader| !reader.finished.load(Ordering::Relaxed));
        api.refresh_devices()?;
        let mut present: HashMap<String, &DeviceInfo> = HashMap::new();
        for info in api.device_list().filter(|info| is_fader(info)) {
            present.entry(key_for(info)).or_insert(info);
        }

        for (key, name) in &named {
            if !present.contains_key(key) {
                if let Some(reader) = readers.remove(key) {
                    reader.stop.store(true, Ordering::Relaxed);
                }
                let _ = events.send(Event::Source {
                    key: key.clone(),
                    name: name.clone(),
                    connected: false,
                });
            }
        }
        named.retain(|key, _| present.contains_key(key));

        let mut failed = false;
        for (key, info) in &present {
            if readers.contains_key(key) {
                continue;
            }
            let name = info.product_string().unwrap_or("ZMK faders").to_owned();
            named.insert(key.clone(), name.clone());
            match info.open_device(&api) {
                Ok(device) => {
                    warned.remove(key);
                    let stop = Arc::new(AtomicBool::new(false));
                    let finished = Arc::new(AtomicBool::new(false));
                    spawn_reader(
                        device,
                        key.clone(),
                        name.clone(),
                        events.clone(),
                        &stop,
                        &finished,
                    );
                    readers.insert(key.clone(), Reader { stop, finished });
                    let _ = events.send(Event::Source {
                        key: key.clone(),
                        name,
                        connected: true,
                    });
                }
                Err(err) => {
                    failed = true;
                    if warned.insert(key.clone()) {
                        eprintln!(
                            "drifttrim: cannot open {name}: {err}. Is the udev rule installed?"
                        );
                    }
                    let _ = events.send(Event::Source {
                        key: key.clone(),
                        name,
                        connected: false,
                    });
                }
            }
        }

        let mut fds = [libc::pollfd {
            fd: monitor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // A reader that ended needs a rescan too, so never wait forever with one open.
        let timeout = if failed || !readers.is_empty() {
            RETRY_MS
        } else {
            -1
        };
        // SAFETY: `fds` is a valid array of one pollfd for the duration of the call.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout) };
        for _ in monitor.iter() {}
    }
}

fn spawn_reader(
    device: hidapi::HidDevice,
    key: String,
    name: String,
    events: Sender<Event>,
    stop: &Arc<AtomicBool>,
    finished: &Arc<AtomicBool>,
) {
    let stop = Arc::clone(stop);
    let finished = Arc::clone(finished);
    let _ = thread::Builder::new()
        .name("hid-reader".into())
        .spawn(move || {
            let mut buffer = [0u8; 64];
            while !stop.load(Ordering::Relaxed) {
                match device.read_timeout(&mut buffer, 1000) {
                    Ok(0) => {}
                    Ok(count) => {
                        for (axis, raw) in parse_report(&buffer[..count]).into_iter().enumerate() {
                            let _ = events.send(Event::Sample {
                                key: key.clone(),
                                axis,
                                raw,
                            });
                        }
                    }
                    Err(_) => break,
                }
            }
            finished.store(true, Ordering::Relaxed);
            let _ = events.send(Event::Source {
                key,
                name,
                connected: false,
            });
        });
}

#[cfg(test)]
mod tests {
    use super::{has_fader_report, parse_report};

    #[test]
    fn recognises_the_zmk_fader_collection() {
        let descriptor = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x10, 0x95, 0x08, 0x81,
            0x02, 0xC0,
        ];
        assert!(has_fader_report(&descriptor));
    }

    #[test]
    fn rejects_other_vendor_collections() {
        // Start of a Steam Controller descriptor: vendor page, usage 2, report 0x42.
        let wrong_usage = [
            0x06, 0x00, 0xFF, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x10, 0x81, 0x02, 0xC0,
        ];
        assert!(!has_fader_report(&wrong_usage));
        let wrong_report = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x42, 0x75, 0x10, 0x81, 0x02, 0xC0,
        ];
        assert!(!has_fader_report(&wrong_report));
        // Report 2 exists, but as 8-bit data, as on a Steam Controller.
        let eight_bit = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x08, 0x81, 0x02, 0xC0,
        ];
        assert!(!has_fader_report(&eight_bit));
        // Report 2 as an output or outside the vendor page never counts.
        let output = [
            0x06, 0x00, 0xFF, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x10, 0x91, 0x02, 0xC0,
        ];
        assert!(!has_fader_report(&output));
        let generic = [
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x02, 0x75, 0x10, 0x81, 0x02, 0xC0,
        ];
        assert!(!has_fader_report(&generic));
    }

    #[test]
    fn survives_truncated_descriptors() {
        assert!(!has_fader_report(&[0x06, 0x00]));
        assert!(!has_fader_report(&[0xFE]));
        assert!(!has_fader_report(&[]));
    }

    #[test]
    fn decodes_little_endian_axes() {
        assert_eq!(parse_report(&[2, 0xE8, 0x03, 0x10, 0x00]), vec![1000, 16]);
    }

    #[test]
    fn keeps_signed_values() {
        assert_eq!(parse_report(&[2, 0xFF, 0xFF]), vec![-1]);
    }

    #[test]
    fn ignores_other_reports_and_short_data() {
        assert!(parse_report(&[1, 0, 0]).is_empty());
        assert!(parse_report(&[2, 0]).is_empty());
        assert!(parse_report(&[]).is_empty());
    }

    #[test]
    fn caps_at_eight_axes_and_drops_the_button_byte() {
        let mut report = vec![2u8];
        report.extend(std::iter::repeat_n([1u8, 0u8], 10).flatten());
        assert_eq!(parse_report(&report).len(), 8);
        assert_eq!(parse_report(&[2, 5, 0, 9]), vec![5]);
    }
}
