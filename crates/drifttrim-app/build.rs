use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    println!("cargo:rerun-if-changed=cpp");
    CxxQtBuilder::new_qml_module(QmlModule::new("org.drifttrim.app").qml_files([
        "qml/Main.qml",
        "qml/FaderCard.qml",
        "qml/CaptureDialog.qml",
        "qml/CalibrateDialog.qml",
        "qml/OutputsDialog.qml",
        "qml/CategoriesPage.qml",
        "qml/OptionsPage.qml",
    ]))
    .qt_module("Widgets")
    .qt_module("QuickControls2")
    .include_dir("cpp")
    .cpp_file("cpp/app_main.cpp")
    .files(["src/bridge.rs"])
    .build();
}
