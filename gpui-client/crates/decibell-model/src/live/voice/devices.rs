//! Audio hardware lists (`stores/audioDevicesStore.ts`, `hooks/useAudioDeviceSync.ts`)
//! and the stream-audio app roster (`stores/streamAudioAppsStore.ts`).
//!
//! **Devices**: one list for every picker. A view that shows it holds a watch
//! ([`AudioDevices::watch`] while it is on screen, [`AudioDevices::unwatch`] when it
//! goes): the first watch lists the devices again and switches the core's device
//! notifications on (`api::audio::watch_devices`: the platform's own, never a poll; the
//! core's listener thread exists only meanwhile), the last switches them off. Each
//! notification (`audio_devices_changed`, Electron's `devicechange`) is
//! [`AudioDevices::hotplug`]: a burst collapses into one refresh 400 ms after the last
//! report. The timer exists only while one is pending. Like the renderer, a change never
//! re-pushes the selection to a running voice engine (each re-push was an audible pop); a
//! "Default" selection follows the OS default inside the core.
//!
//! **Stream-audio apps**: the Go Live picker refreshes while it is open (the renderer
//! polls every 2 s while mounted: that timer is the picker view's, with the open picker
//! as its reason); a refresh in flight is reused and an unchanged list notifies nothing.

use std::rc::Rc;
use std::time::Duration;

use futures::future::LocalBoxFuture;
use gpui::{Context, SharedString, Task};

use crate::error::UiError;

pub const HOTPLUG_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioDevice {
    /// What the core takes (a PipeWire / Pulse node name on Linux).
    pub name: SharedString,
    pub label: SharedString,
}

pub type DeviceList = (Vec<AudioDevice>, Vec<AudioDevice>);
pub type DeviceLister = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<DeviceList, UiError>>>;
/// Switches the platform's device notifications on or off. Answers whether there is a
/// platform to ask (the core runs): `false` and nothing was switched.
pub type DeviceWatching = Rc<dyn Fn(bool) -> bool>;

pub struct AudioDevices {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
    pub loading: bool,
    pub loaded: bool,
    lister: DeviceLister,
    in_flight: Option<Task<()>>,
    debounce: Option<Task<()>>,
    watching: Option<DeviceWatching>,
    /// Views showing the lists now.
    watchers: usize,
    /// The notifications were switched on (to switch them off exactly once).
    notified: bool,
}

impl AudioDevices {
    pub fn new(lister: DeviceLister) -> Self {
        AudioDevices {
            inputs: Vec::new(),
            outputs: Vec::new(),
            loading: false,
            loaded: false,
            lister,
            in_flight: None,
            debounce: None,
            watching: None,
            watchers: 0,
            notified: false,
        }
    }

    /// With the switch for the platform's device notifications (the model's: the core's).
    pub fn with_watching(mut self, watching: DeviceWatching) -> Self {
        self.watching = Some(watching);
        self
    }

    /// A view that shows the lists appeared (a picker opened, Settings → Audio). The first
    /// one switches the notifications on and lists again: changes while nobody watched
    /// were not reported.
    pub fn watch(&mut self, cx: &mut Context<Self>) {
        self.watchers += 1;
        if self.watchers > 1 {
            return;
        }
        self.notified = self.watching.as_ref().is_some_and(|w| w(true));
        if self.notified {
            self.refresh(cx);
        }
    }

    /// That view went. The last one switches the notifications off; a refresh still
    /// waiting out its debounce is dropped with them.
    pub fn unwatch(&mut self) {
        if self.watchers == 0 {
            return;
        }
        self.watchers -= 1;
        if self.watchers > 0 {
            return;
        }
        self.debounce = None;
        if std::mem::take(&mut self.notified)
            && let Some(w) = &self.watching
        {
            w(false);
        }
    }

    pub fn watchers(&self) -> usize {
        self.watchers
    }

    /// Re-enumerate. A refresh already in flight is reused.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.in_flight.is_some() {
            return;
        }
        self.loading = true;
        cx.notify();
        let list = (self.lister)();
        self.in_flight = Some(cx.spawn(async move |this, cx| {
            let result = list.await;
            let _ = this.update(cx, |this, cx| {
                this.in_flight = None;
                this.loading = false;
                match result {
                    Ok((inputs, outputs)) => {
                        this.inputs = inputs;
                        this.outputs = outputs;
                        this.loaded = true;
                    }
                    Err(e) => log::error!("[audioDevices] refresh failed: {e}"),
                }
                cx.notify();
            });
        }));
    }

    /// First use (a picker opened): load once.
    pub fn ensure_loaded(&mut self, cx: &mut Context<Self>) {
        if !self.loaded {
            self.refresh(cx);
        }
    }

    /// The OS reported a device change (`audio_devices_changed`, while watched).
    pub fn hotplug(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HOTPLUG_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this.debounce = None;
                this.refresh(cx);
            });
        }));
    }

    pub fn hotplug_pending(&self) -> bool {
        self.debounce.is_some()
    }
}

/// An app with an audio output (`StreamAudioApp`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamAudioApp {
    /// Normalised identity (lowercase program name).
    pub id: SharedString,
    pub name: SharedString,
    /// Producing sound right now.
    pub active: bool,
    /// Owns the window being shared (Windows).
    pub owns_window_source: bool,
}

pub type AppLister =
    Rc<dyn Fn(Option<String>) -> LocalBoxFuture<'static, Result<(bool, Vec<StreamAudioApp>), UiError>>>;

pub struct StreamAudioApps {
    pub apps: Vec<StreamAudioApp>,
    /// Per-app stream audio exists here (false on macOS).
    pub supported: bool,
    lister: AppLister,
    in_flight: Option<Task<()>>,
}

impl StreamAudioApps {
    pub fn new(lister: AppLister) -> Self {
        StreamAudioApps { apps: Vec::new(), supported: true, lister, in_flight: None }
    }

    pub fn refresh(&mut self, source_id: Option<String>, cx: &mut Context<Self>) {
        if self.in_flight.is_some() {
            return;
        }
        let list = (self.lister)(source_id);
        self.in_flight = Some(cx.spawn(async move |this, cx| {
            let result = list.await;
            let _ = this.update(cx, |this, cx| {
                this.in_flight = None;
                match result {
                    Ok((supported, apps)) => {
                        if this.supported != supported || this.apps != apps {
                            this.supported = supported;
                            this.apps = apps;
                            cx.notify();
                        }
                    }
                    Err(e) => log::error!("[streamAudioApps] refresh failed: {e}"),
                }
            });
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn device(name: &str) -> AudioDevice {
        AudioDevice { name: name.to_string().into(), label: name.to_string().into() }
    }

    #[gpui::test]
    fn a_hotplug_burst_is_one_refresh(cx: &mut TestAppContext) {
        let calls = Rc::new(Cell::new(0));
        let devices = cx.new(|_| {
            let calls = calls.clone();
            AudioDevices::new(Rc::new(move || {
                calls.set(calls.get() + 1);
                Box::pin(async { Ok((vec![device("mic")], vec![device("speakers")])) })
            }))
        });
        for _ in 0..5 {
            devices.update(cx, |d, cx| d.hotplug(cx));
            cx.executor().advance_clock(Duration::from_millis(100));
            cx.run_until_parked();
        }
        assert_eq!(calls.get(), 0, "still inside the debounce");
        cx.executor().advance_clock(HOTPLUG_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(calls.get(), 1);
        devices.read_with(cx, |d, _| {
            assert_eq!(d.inputs, vec![device("mic")]);
            assert!(!d.loading && d.loaded && !d.hotplug_pending());
        });
        // A second refresh while one is in flight is the same one.
        devices.update(cx, |d, cx| {
            d.refresh(cx);
            d.refresh(cx);
        });
        cx.run_until_parked();
        assert_eq!(calls.get(), 2);
    }

    #[gpui::test]
    fn the_notifications_run_exactly_while_a_view_watches(cx: &mut TestAppContext) {
        let lists = Rc::new(Cell::new(0));
        let switched: Rc<std::cell::RefCell<Vec<bool>>> = Rc::default();
        let devices = cx.new(|_| {
            let (lists, switched) = (lists.clone(), switched.clone());
            AudioDevices::new(Rc::new(move || {
                lists.set(lists.get() + 1);
                Box::pin(async { Ok((vec![device("mic")], vec![device("speakers")])) })
            }))
            .with_watching(Rc::new(move |on| {
                switched.borrow_mut().push(on);
                true
            }))
        });
        // The first watcher switches them on and lists; a second changes nothing.
        devices.update(cx, |d, cx| d.watch(cx));
        devices.update(cx, |d, cx| d.watch(cx));
        cx.run_until_parked();
        assert_eq!((lists.get(), switched.borrow().clone()), (1, vec![true]));
        // A report while watched: one refresh after the debounce.
        devices.update(cx, |d, cx| d.hotplug(cx));
        cx.executor().advance_clock(HOTPLUG_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!(lists.get(), 2);
        // The last watcher going switches them off and drops a pending refresh.
        devices.update(cx, |d, cx| {
            d.hotplug(cx);
            d.unwatch();
            assert_eq!(d.watchers(), 1);
            d.unwatch();
            assert!(!d.hotplug_pending());
            d.unwatch();
        });
        cx.executor().advance_clock(HOTPLUG_DEBOUNCE);
        cx.run_until_parked();
        assert_eq!((lists.get(), switched.borrow().clone()), (2, vec![true, false]));
        // Watching again lists again (what changed meanwhile was not reported).
        devices.update(cx, |d, cx| d.watch(cx));
        cx.run_until_parked();
        assert_eq!((lists.get(), switched.borrow().clone()), (3, vec![true, false, true]));
    }

    #[gpui::test]
    fn without_a_platform_watching_switches_nothing(cx: &mut TestAppContext) {
        let lists = Rc::new(Cell::new(0));
        let offs = Rc::new(Cell::new(0));
        let devices = cx.new(|_| {
            let (lists, offs) = (lists.clone(), offs.clone());
            AudioDevices::new(Rc::new(move || {
                lists.set(lists.get() + 1);
                Box::pin(async { Ok((Vec::new(), Vec::new())) })
            }))
            // No core (a screen drawn from fixtures): nothing to switch, nothing to list.
            .with_watching(Rc::new(move |on| {
                if !on {
                    offs.set(offs.get() + 1);
                }
                false
            }))
        });
        devices.update(cx, |d, cx| {
            d.watch(cx);
            d.unwatch();
        });
        cx.run_until_parked();
        assert_eq!((lists.get(), offs.get()), (0, 0));
    }

    #[gpui::test]
    fn an_unchanged_app_list_notifies_nothing(cx: &mut TestAppContext) {
        let apps = cx.new(|_| {
            StreamAudioApps::new(Rc::new(|_| {
                Box::pin(async {
                    Ok((true, vec![StreamAudioApp { id: "firefox".into(), name: "Firefox".into(), active: true, owns_window_source: false }]))
                })
            }))
        });
        let notifies = Rc::new(Cell::new(0));
        let _sub = cx.update(|cx| {
            let notifies = notifies.clone();
            cx.observe(&apps, move |_, _| notifies.set(notifies.get() + 1))
        });
        for _ in 0..3 {
            apps.update(cx, |a, cx| a.refresh(None, cx));
            cx.run_until_parked();
        }
        assert_eq!(notifies.get(), 1);
    }
}
