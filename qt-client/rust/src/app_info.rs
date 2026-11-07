//! Build facts for the UI (version, build profile): the smallest bridge, proving the QML ↔ Rust path.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(QString, version, READ, CONSTANT)]
        #[qproperty(QString, profile, READ, CONSTANT)]
        type AppInfo = super::AppInfoRust;
    }
}

use cxx_qt_lib::QString;

pub struct AppInfoRust {
    version: QString,
    profile: QString,
}

impl Default for AppInfoRust {
    fn default() -> Self {
        Self {
            version: QString::from(env!("CARGO_PKG_VERSION")),
            profile: QString::from(if cfg!(debug_assertions) { "debug" } else { "release" }),
        }
    }
}
