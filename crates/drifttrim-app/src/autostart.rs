//! Start at login through the XDG autostart directory, which Plasma honours.

use std::fs;
use std::io;
use std::path::PathBuf;

const FILE_NAME: &str = "org.drifttrim.DriftTrim.desktop";

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("autostart").join(FILE_NAME))
}

pub fn enabled() -> bool {
    path().is_some_and(|p| p.exists())
}

pub fn set(enable: bool) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::other("no home directory"))?;
    if !enable {
        return match fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?;
    let contents = format!(
        "[Desktop Entry]\nType=Application\nName=DriftTrim\nComment=Ride app and device volume with faders\n\
         Exec=\"{}\" --minimized\nIcon=org.drifttrim.DriftTrim\nX-KDE-autostart-phase=2\n",
        exe.display()
    );
    fs::create_dir_all(path.parent().expect("autostart path has a parent"))?;
    fs::write(path, contents)
}
