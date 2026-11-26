//! The idle source behind auto-away (Electron: `powerMonitor.getSystemIdleTime` / `getSystemIdleState`
//! in `idle.ts`). The core reads Wayland's `ext-idle-notify` and GNOME's IdleMonitor; on Windows the
//! app reads `GetLastInputInfo`, which knows how long the session has been idle, so `AutoAway` arms
//! its one timer for exactly when the threshold can be reached. Lock and sleep come from
//! [`crate::session`]. X11 and macOS: the core's answer ("unsupported": Away only on lock / sleep).

use std::rc::Rc;

use decibell_model::live::presence::IdleSource;

/// The source the app gives `AutoAway` on this platform.
pub fn source() -> Rc<dyn IdleSource> {
    #[cfg(windows)]
    let source: Rc<dyn IdleSource> = Rc::new(last_input::LastInput);
    #[cfg(not(windows))]
    let source: Rc<dyn IdleSource> = Rc::new(decibell_model::actions::presence::CoreIdleSource);
    source
}

#[cfg(windows)]
mod last_input {
    use std::time::Duration;

    use decibell_model::live::presence::{IdleReading, IdleSource};
    use futures::future::LocalBoxFuture;

    /// The session's last keyboard / mouse input (`GetLastInputInfo`): a cheap call, made when
    /// `AutoAway` asks (once per threshold while active, every second while Away, as Electron).
    pub struct LastInput;

    impl IdleSource for LastInput {
        fn read(&self, threshold: Duration) -> LocalBoxFuture<'static, IdleReading> {
            let reading = match crate::win32::idle_for() {
                Some(idle) if idle >= threshold => IdleReading::Idle,
                Some(idle) => IdleReading::Active { idle_for: Some(idle) },
                None => IdleReading::Unsupported,
            };
            Box::pin(async move { reading })
        }

        fn pushes_resume(&self) -> bool {
            false
        }
    }
}
