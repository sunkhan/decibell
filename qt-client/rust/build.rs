use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    // The Rust side's QML module: types implemented in Rust (models, the core bridge).
    CxxQtBuilder::new_qml_module(QmlModule::new("Decibell.Core"))
        .qt_module("Qml")
        .files(["src/app_info.rs", "src/models.rs"])
        .build();
}
