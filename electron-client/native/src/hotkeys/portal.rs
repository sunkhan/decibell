//! Wayland global shortcuts through `org.freedesktop.portal.GlobalShortcuts`
//! (KDE Plasma, GNOME 48+, Hyprland). The desktop owns the actual key
//! grab: we offer each binding with a `preferred_trigger`, the desktop
//! confirms (KDE shows a dialog for ids it hasn't seen) and reports what
//! it assigned, then sends `Activated` / `Deactivated` per binding id —
//! press *and* release, so push-to-talk works.
//!
//! Runs on its own thread with a current-thread runtime and its own D-Bus
//! connection, started on the first non-empty binding set (no bindings,
//! no session, no dialog). A desktop without the portal falls back to
//! XInput2 through XWayland (`x11.rs`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::mpsc;
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::{dispatch, keys, update_status, Action, Binding};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
/// Must name an installed `<id>.desktop`; every package installs
/// `decibell.desktop`. A dev checkout without it just isn't registered.
const APP_ID: &str = "decibell";
/// How long the desktop's confirmation dialog may stay open.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

enum Cmd {
    Bind(Vec<Binding>),
    OpenSettings,
}

static TX: Mutex<Option<mpsc::UnboundedSender<Cmd>>> = Mutex::new(None);
/// Set once we learn the desktop has no GlobalShortcuts portal.
static FALLBACK: AtomicBool = AtomicBool::new(false);

pub fn apply(bindings: &[Binding]) {
    if FALLBACK.load(Ordering::Acquire) {
        super::x11::apply(bindings, true);
        return;
    }
    let mut tx = TX.lock().unwrap_or_else(|e| e.into_inner());
    if tx.is_none() {
        if bindings.is_empty() {
            return;
        }
        let (sender, rx) = mpsc::unbounded_channel();
        let spawned = std::thread::Builder::new()
            .name("hotkeys-portal".into())
            .spawn(move || match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt.block_on(run(rx)),
                Err(e) => set_error(format!("runtime: {}", e)),
            });
        if let Err(e) = spawned {
            set_error(format!("thread: {}", e));
            return;
        }
        *tx = Some(sender);
    }
    if let Some(sender) = tx.as_ref() {
        let _ = sender.send(Cmd::Bind(bindings.to_vec()));
    }
}

pub fn open_settings() -> Result<(), String> {
    let tx = TX.lock().unwrap_or_else(|e| e.into_inner());
    match tx.as_ref() {
        Some(sender) if sender.send(Cmd::OpenSettings).is_ok() => Ok(()),
        _ => Err("No keybinds are registered with the desktop yet".into()),
    }
}

fn set_error(detail: String) {
    log::warn!("[hotkeys] portal: {}", detail);
    update_status(|s| {
        s.state = "error";
        s.detail = Some(detail);
    });
}

async fn run(mut rx: mpsc::UnboundedReceiver<Cmd>) {
    let portal = match Portal::connect().await {
        Ok(p) => p,
        Err(e) => {
            log::info!("[hotkeys] no GlobalShortcuts portal ({}); trying XWayland", e);
            FALLBACK.store(true, Ordering::Release);
            *TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
            // Whatever was asked for last goes to the fallback.
            let mut latest = None;
            while let Ok(cmd) = rx.try_recv() {
                if let Cmd::Bind(b) = cmd {
                    latest = Some(b);
                }
            }
            update_status(|s| {
                s.backend = "xwayland";
                s.mouse = true;
                s.focused_fallback = true;
                s.can_configure = false;
                s.triggers.clear();
            });
            super::x11::apply(&latest.unwrap_or_default(), true);
            return;
        }
    };
    let result = portal.serve(&mut rx).await;
    *TX.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // Whatever was held through the portal can't be released by it now.
    super::release_all_bindings();
    if let Err(e) = result {
        set_error(format!("portal connection lost: {}", e));
    }
}

struct Portal {
    conn: zbus::Connection,
    proxy: zbus::Proxy<'static>,
    version: u32,
    session: Option<OwnedObjectPath>,
    ids: HashMap<String, Action>,
}

impl Portal {
    async fn connect() -> Result<Self, String> {
        let conn = zbus::Connection::session().await.map_err(|e| format!("D-Bus session: {}", e))?;
        // Host apps name themselves before any other portal call on the
        // connection (xdg-desktop-portal >= 1.19); older portals and a
        // missing desktop file fail here harmlessly.
        if let Err(e) = conn
            .call_method(
                Some(DEST),
                PATH,
                Some("org.freedesktop.host.portal.Registry"),
                "Register",
                &(APP_ID, HashMap::<&str, Value>::new()),
            )
            .await
        {
            log::info!("[hotkeys] portal app id not registered: {}", e);
        }
        let proxy = proxy(&conn, PATH, IFACE).await?;
        let version: u32 = proxy
            .get_property("version")
            .await
            .map_err(|e| format!("GlobalShortcuts: {}", e))?;
        update_status(|s| {
            s.backend = "portal";
            s.mouse = false;
            s.focused_fallback = false;
            s.can_configure = version >= 2;
        });
        Ok(Self { conn, proxy, version, session: None, ids: HashMap::new() })
    }

    async fn serve(mut self, rx: &mut mpsc::UnboundedReceiver<Cmd>) -> Result<(), String> {
        let err = |e: zbus::Error| e.to_string();
        let mut activated = self.proxy.receive_signal("Activated").await.map_err(err)?;
        let mut deactivated = self.proxy.receive_signal("Deactivated").await.map_err(err)?;
        let mut changed = self.proxy.receive_signal("ShortcutsChanged").await.map_err(err)?;
        loop {
            tokio::select! {
                cmd = rx.recv() => {
                    let Some(cmd) = cmd else { return Ok(()) };
                    // Coalesce a burst of edits into the newest set.
                    let (mut bind, mut open) = (None, false);
                    for c in std::iter::once(cmd).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                        match c {
                            Cmd::Bind(b) => bind = Some(b),
                            Cmd::OpenSettings => open = true,
                        }
                    }
                    if let Some(b) = bind {
                        if let Err(e) = self.bind(b).await {
                            set_error(e);
                        }
                    }
                    if open {
                        if let Err(e) = self.open_settings().await {
                            set_error(e);
                        }
                    }
                }
                msg = activated.next() => {
                    let msg = msg.ok_or("Activated stream ended")?;
                    self.on_activation(&msg, true);
                }
                msg = deactivated.next() => {
                    let msg = msg.ok_or("Deactivated stream ended")?;
                    self.on_activation(&msg, false);
                }
                msg = changed.next() => {
                    let msg = msg.ok_or("ShortcutsChanged stream ended")?;
                    self.on_changed(&msg);
                }
            }
        }
    }

    fn on_activation(&self, msg: &zbus::Message, pressed: bool) {
        let Ok((session, id, _ts, _opts)) = msg
            .body()
            .deserialize::<(OwnedObjectPath, String, u64, HashMap<String, OwnedValue>)>()
        else {
            return;
        };
        if self.session.as_ref() != Some(&session) {
            return;
        }
        if let Some(action) = self.ids.get(&id) {
            dispatch(&id, *action, pressed);
        }
    }

    fn on_changed(&self, msg: &zbus::Message) {
        let Some((session, triggers)) = changed_triggers(msg) else { return };
        if self.session.as_ref() != Some(&session) {
            return;
        }
        update_status(|s| s.triggers = triggers);
    }

    async fn bind(&mut self, bindings: Vec<Binding>) -> Result<(), String> {
        if let Some(old) = self.session.take() {
            let _ = self
                .conn
                .call_method(Some(DEST), &old, Some("org.freedesktop.portal.Session"), "Close", &())
                .await;
        }
        self.ids.clear();
        // The portal binds keys only; mouse bindings stay unregistered.
        let bindings: Vec<Binding> =
            bindings.into_iter().filter(|b| !b.keys.iter().any(|k| keys::is_mouse(k))).collect();
        if bindings.is_empty() {
            update_status(|s| {
                s.state = "idle";
                s.detail = None;
                s.triggers.clear();
            });
            return Ok(());
        }
        update_status(|s| {
            s.state = "starting";
            s.detail = None;
        });

        let session_token = token();
        let (code, results) = self
            .request("CreateSession", |t| {
                (HashMap::from([
                    ("handle_token", Value::from(t)),
                    ("session_handle_token", Value::from(session_token.clone())),
                ]),)
            })
            .await?;
        if code != 0 {
            return Err(format!("CreateSession refused ({})", code));
        }
        let session = results
            .get("session_handle")
            .and_then(|v| object_path(v))
            .ok_or("CreateSession returned no session")?;
        self.session = Some(session.clone());

        let shortcuts: Vec<(String, HashMap<&str, Value>)> = bindings
            .iter()
            .map(|b| {
                let mut opts = HashMap::from([("description", Value::from(b.action.label()))]);
                if let Some(t) = keys::portal_trigger(&b.keys) {
                    opts.insert("preferred_trigger", Value::from(t));
                }
                (b.id.clone(), opts)
            })
            .collect();
        let (code, results) = self
            .request("BindShortcuts", |t| {
                (session.clone(), shortcuts, "", HashMap::from([("handle_token", Value::from(t))]))
            })
            .await?;
        match code {
            0 => {
                self.ids = bindings.iter().map(|b| (b.id.clone(), b.action)).collect();
                let triggers = results.get("shortcuts").map(|v| parse_triggers(v)).unwrap_or_default();
                update_status(|s| {
                    s.state = "active";
                    s.detail = None;
                    s.triggers = triggers;
                });
                Ok(())
            }
            1 => Err("The desktop's shortcut dialog was cancelled".into()),
            n => Err(format!("BindShortcuts failed ({})", n)),
        }
    }

    async fn open_settings(&self) -> Result<(), String> {
        if self.version < 2 {
            return Err("This desktop can't open its shortcut settings from apps".into());
        }
        let session = self.session.as_ref().ok_or("No keybinds are registered with the desktop yet")?;
        self.proxy
            .call_method("ConfigureShortcuts", &(session, "", HashMap::<&str, Value>::new()))
            .await
            .map(|_| ())
            .map_err(|e| format!("ConfigureShortcuts: {}", e))
    }

    /// A portal call that answers through a Request object's `Response`
    /// signal. Subscribes before calling so the answer can't be missed.
    async fn request<B>(
        &self,
        method: &str,
        body: impl FnOnce(String) -> B,
    ) -> Result<(u32, HashMap<String, OwnedValue>), String>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let handle = token();
        let sender = self
            .conn
            .unique_name()
            .ok_or("no D-Bus unique name")?
            .trim_start_matches(':')
            .replace('.', "_");
        let path = format!("{}/request/{}/{}", PATH, sender, handle);
        let request = proxy(&self.conn, &path, "org.freedesktop.portal.Request").await?;
        let mut responses = request
            .receive_signal("Response")
            .await
            .map_err(|e| format!("{} subscribe: {}", method, e))?;
        self.proxy
            .call_method(method, &body(handle))
            .await
            .map_err(|e| format!("{}: {}", method, e))?;
        let msg = tokio::time::timeout(REQUEST_TIMEOUT, responses.next())
            .await
            .map_err(|_| format!("{}: no answer from the desktop", method))?
            .ok_or_else(|| format!("{}: request closed", method))?;
        msg.body()
            .deserialize::<(u32, HashMap<String, OwnedValue>)>()
            .map_err(|e| format!("{} response: {}", method, e))
    }
}

async fn proxy(conn: &zbus::Connection, path: &str, iface: &'static str) -> Result<zbus::Proxy<'static>, String> {
    let path = ObjectPath::try_from(path.to_string()).map_err(|e| e.to_string())?;
    zbus::proxy::Builder::new(conn)
        .destination(DEST)
        .and_then(|b| b.path(path))
        .and_then(|b| b.interface(iface))
        .map_err(|e| e.to_string())?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .map_err(|e| format!("{} proxy: {}", iface, e))
}

fn token() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!("decibell_hk_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

/// Strip variant wrappers (`a{sv}` values arrive boxed).
fn unwrap_variant<'a, 'b>(v: &'b Value<'a>) -> &'b Value<'a> {
    match v {
        Value::Value(inner) => unwrap_variant(inner),
        other => other,
    }
}

fn object_path(v: &Value<'_>) -> Option<OwnedObjectPath> {
    match unwrap_variant(v) {
        Value::ObjectPath(p) => Some(p.clone().into()),
        // The spec says `s`; some backends send `o`. Take either.
        Value::Str(s) => ObjectPath::try_from(s.as_str()).ok().map(|p| p.into()),
        _ => None,
    }
}

/// `ShortcutsChanged(o session, a(sa{sv}) shortcuts)` → session + triggers.
/// The body must be read with its real signature; a `Value` stand-in for
/// the array would type as `ov` and never match.
fn changed_triggers(msg: &zbus::Message) -> Option<(OwnedObjectPath, HashMap<String, String>)> {
    let (session, shortcuts) = msg
        .body()
        .deserialize::<(OwnedObjectPath, Vec<(String, HashMap<String, OwnedValue>)>)>()
        .ok()?;
    let triggers = shortcuts
        .into_iter()
        .filter_map(|(id, opts)| {
            let t: &str = opts.get("trigger_description")?.downcast_ref().ok()?;
            (!t.is_empty()).then(|| (id, t.to_string()))
        })
        .collect();
    Some((session, triggers))
}

/// `a(sa{sv})` (inside a vardict) → binding id → `trigger_description`.
fn parse_triggers(v: &Value<'_>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Value::Array(items) = unwrap_variant(v) else { return out };
    for item in items.iter() {
        let Value::Structure(st) = unwrap_variant(item) else { continue };
        let (Some(Value::Str(id)), Some(Value::Dict(opts))) = (st.fields().first(), st.fields().get(1)) else {
            continue;
        };
        for (k, val) in opts.iter() {
            if let (Value::Str(k), Value::Str(t)) = (unwrap_variant(k), unwrap_variant(val)) {
                if k.as_str() == "trigger_description" && !t.is_empty() {
                    out.insert(id.to_string(), t.to_string());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shortcut_list() -> Vec<(String, HashMap<String, Value<'static>>)> {
        vec![
            (
                "b1".to_string(),
                HashMap::from([
                    ("description".to_string(), Value::from("Toggle mute")),
                    ("trigger_description".to_string(), Value::from("Ctrl+Shift+M")),
                ]),
            ),
            ("b2".to_string(), HashMap::from([("trigger_description".to_string(), Value::from(""))])),
        ]
    }

    /// Both trigger paths, through a real D-Bus message encode/decode.
    #[test]
    fn triggers_parse_from_signal_and_response() {
        let path = ObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_2/t").unwrap();
        let changed = zbus::Message::signal(PATH, IFACE, "ShortcutsChanged")
            .unwrap()
            .build(&(path.clone(), shortcut_list()))
            .unwrap();
        let (session, triggers) = changed_triggers(&changed).expect("ShortcutsChanged body");
        assert_eq!(session.as_str(), path.as_str());
        assert_eq!(triggers, HashMap::from([("b1".to_string(), "Ctrl+Shift+M".to_string())]));

        let results = HashMap::from([("shortcuts".to_string(), Value::from(shortcut_list()))]);
        let response = zbus::Message::signal(PATH, "org.freedesktop.portal.Request", "Response")
            .unwrap()
            .build(&(0u32, results))
            .unwrap();
        let (code, results) =
            response.body().deserialize::<(u32, HashMap<String, OwnedValue>)>().unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            parse_triggers(results.get("shortcuts").unwrap()),
            HashMap::from([("b1".to_string(), "Ctrl+Shift+M".to_string())])
        );
    }

    #[test]
    fn session_handle_accepts_string_or_object_path() {
        let p = "/org/freedesktop/portal/desktop/session/1_2/t";
        assert_eq!(object_path(&Value::from(p)).unwrap().as_str(), p);
        let op = ObjectPath::try_from(p).unwrap();
        assert_eq!(object_path(&Value::from(op)).unwrap().as_str(), p);
        assert!(object_path(&Value::from(7u32)).is_none());
    }

    /// Request/Response plumbing against the session's real portal, without
    /// binding anything (no dialog, nothing persisted by the desktop).
    #[test]
    #[ignore = "talks to the desktop session's portal"]
    fn live_create_session_and_list() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let p = Portal::connect().await.expect("connect");
            println!("GlobalShortcuts version {}", p.version);
            let session_token = token();
            let (code, results) = p
                .request("CreateSession", |t| {
                    (HashMap::from([
                        ("handle_token", Value::from(t)),
                        ("session_handle_token", Value::from(session_token.clone())),
                    ]),)
                })
                .await
                .expect("CreateSession");
            assert_eq!(code, 0);
            let session = results.get("session_handle").and_then(|v| object_path(v)).expect("session handle");
            println!("session {}", session.as_str());
            let (code, results) = p
                .request("ListShortcuts", |t| (session.clone(), HashMap::from([("handle_token", Value::from(t))])))
                .await
                .expect("ListShortcuts");
            println!("ListShortcuts code {} -> {:?}", code, results.get("shortcuts").map(|v| parse_triggers(v)));
        });
    }
}
