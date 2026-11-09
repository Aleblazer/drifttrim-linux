//! System tray icon through the StatusNotifierItem protocol Plasma uses.

use std::sync::mpsc::Sender;

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::StandardItem;

use crate::runtime::Event;

struct DriftTray {
    events: Sender<Event>,
    tooltip: String,
}

impl ksni::Tray for DriftTray {
    fn id(&self) -> String {
        "org.drifttrim.DriftTrim".into()
    }

    fn title(&self) -> String {
        "DriftTrim".into()
    }

    fn icon_name(&self) -> String {
        "audio-volume-high-symbolic".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "DriftTrim".into(),
            description: self.tooltip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(Event::ShowWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open".into(),
                icon_name: "window-new".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(Event::ShowWindow);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(Event::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct TrayHandle(Handle<DriftTray>);

impl TrayHandle {
    pub fn set_tooltip(&self, tooltip: String) {
        self.0.update(move |tray| tray.tooltip = tooltip);
    }
}

pub fn start(events: Sender<Event>) -> Option<TrayHandle> {
    match (DriftTray {
        events,
        tooltip: String::new(),
    })
    .spawn()
    {
        Ok(handle) => Some(TrayHandle(handle)),
        Err(err) => {
            eprintln!("drifttrim: tray icon unavailable: {err}");
            None
        }
    }
}
