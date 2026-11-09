//! The QObject that QML talks to.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    unsafe extern "C++" {
        include!("app_main.h");
        #[cxx_name = "drifttrim_run_app"]
        fn run_app() -> i32;
    }

    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, state_json, cxx_name = "stateJson")]
        #[qproperty(bool, start_hidden, cxx_name = "startHidden")]
        type Backend = super::BackendRust;

        #[qinvokable]
        fn command(self: &Backend, json: &QString);

        #[qsignal]
        fn show_requested(self: Pin<&mut Backend>);

        #[qsignal]
        fn quit_requested(self: Pin<&mut Backend>);
    }

    impl cxx_qt::Threading for Backend {}
    impl cxx_qt::Initialize for Backend {}
}

pub use qobject::run_app;

use core::pin::Pin;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender};

use cxx_qt::{CxxQtThread, Threading};
use cxx_qt_lib::QString;

use crate::runtime::{self, Event, UiSink};
use qobject::Backend;

pub struct BackendRust {
    state_json: QString,
    start_hidden: bool,
}

impl Default for BackendRust {
    fn default() -> Self {
        Self {
            state_json: QString::default(),
            start_hidden: START_HIDDEN.get().copied().unwrap_or(false),
        }
    }
}

pub static START_HIDDEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
/// The event channel made in `main`, claimed once when QML creates the Backend.
pub static PENDING: Mutex<Option<(Sender<Event>, Receiver<Event>)>> = Mutex::new(None);

struct QtSink(CxxQtThread<Backend>);

impl UiSink for QtSink {
    fn state(&self, json: String) {
        let _ = self.0.queue(move |backend: Pin<&mut Backend>| {
            backend.set_state_json(QString::from(json.as_str()));
        });
    }

    fn show(&self) {
        let _ = self
            .0
            .queue(|backend: Pin<&mut Backend>| backend.show_requested());
    }

    fn quit(&self) {
        let _ = self
            .0
            .queue(|backend: Pin<&mut Backend>| backend.quit_requested());
    }
}

impl cxx_qt::Initialize for Backend {
    fn initialize(self: Pin<&mut Self>) {
        if let Some((events, rx)) = PENDING.lock().unwrap().take() {
            runtime::start(events, rx, Box::new(QtSink(self.qt_thread())));
        }
    }
}

impl Backend {
    pub fn command(&self, json: &QString) {
        runtime::dispatch(&json.to_string());
    }
}
