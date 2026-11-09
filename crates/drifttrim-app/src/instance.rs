//! One running copy per session. A second launch raises the existing window.

use std::sync::mpsc::Sender;

use zbus::blocking::{Connection, connection};
use zbus::fdo::RequestNameFlags;

use crate::runtime::Event;

const BUS_NAME: &str = "org.drifttrim.DriftTrim";

struct Service {
    events: Sender<Event>,
}

#[zbus::interface(name = "org.drifttrim.DriftTrim")]
impl Service {
    fn show(&self) {
        let _ = self.events.send(Event::ShowWindow);
    }
}

pub enum Claim {
    /// We own the name. Keep the connection alive for the process lifetime.
    Primary(Option<Connection>),
    AlreadyRunning,
}

pub fn claim(events: Sender<Event>) -> Claim {
    let built = connection::Builder::session()
        .and_then(|b| b.serve_at("/", Service { events }))
        .and_then(|b| b.build());
    let connection = match built {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("drifttrim: single-instance check unavailable: {err}");
            return Claim::Primary(None);
        }
    };
    // Without DoNotQueue the bus would silently queue a second copy for the name.
    match connection.request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(_) => Claim::Primary(Some(connection)),
        Err(zbus::Error::NameTaken) => {
            if let Ok(connection) = Connection::session() {
                let _ = connection.call_method(Some(BUS_NAME), "/", Some(BUS_NAME), "Show", &());
            }
            Claim::AlreadyRunning
        }
        Err(err) => {
            eprintln!("drifttrim: single-instance check unavailable: {err}");
            Claim::Primary(None)
        }
    }
}
