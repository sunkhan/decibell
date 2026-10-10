//! X11 (and XWayland on Wayland desktops without the GlobalShortcuts
//! portal): passive grabs on exactly the bound combos.
//!
//! The X server tells us about a key or mouse button only when it completes
//! a bound combo — there is no stream of other input. The grabs are
//! synchronous: on a grabbed press the server freezes input until we drop
//! the grab, which we do at once, so anything typed in that instant goes to
//! the focused app afterwards and never to us. The grab already matched
//! the exact modifiers, so a press action fires straight from that event.
//!
//! A hold (push-to-talk / push-to-mute) also needs its release, which X
//! only reports to whoever has the key — so we poll every 10 ms while it's
//! held, asking as little as the binding allows:
//! - a mouse button, or a modifier key on its own: the pointer query, which
//!   carries the mouse buttons and the Ctrl/Shift/Alt/Super state only;
//! - any other key: `XQueryKeymap`. X has no single-key question, so this
//!   answer lists every key down at that moment; only the bound key's bit
//!   is read. It is asked only while such a hold is down.
//!
//! The trade-off of grabbing: the bound press itself doesn't reach the
//! focused app (as with the desktop's own shortcuts on Wayland and macOS),
//! and a combo another app already grabs can't be bound (`Status::failed`).
//! Through XWayland only X11 apps' input is visible — keys typed into
//! native Wayland windows (Decibell included) never arrive, which is why
//! the `xwayland` backend asks the renderer for its focused fallback.

use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use x11rb::connection::Connection;
use x11rb::protocol::xinput::ConnectionExt as _;
use x11rb::protocol::xproto::{
    ButtonIndex, ConnectionExt as _, EventMask, GrabMode, Keycode, ModMask, Window,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::{dispatch, keys, update_status, Action, Binding};

const POLL_INTERVAL_MS: i32 = 10;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Bumped by every `apply`; the thread regrabs when it changes and exits
/// when the bindings are gone.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Write end of the thread's wake-up pipe (-1 while no thread).
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

pub fn apply(bindings: &[Binding], xwayland: bool) {
    let backend = if xwayland { "xwayland" } else { "x11" };
    GENERATION.fetch_add(1, Ordering::AcqRel);
    if bindings.is_empty() {
        update_status(|s| {
            s.backend = backend;
            s.state = "idle";
            s.detail = None;
            s.failed.clear();
        });
        wake();
        return;
    }
    if RUNNING.swap(true, Ordering::AcqRel) {
        wake();
        return;
    }
    update_status(|s| {
        s.backend = backend;
        s.state = "starting";
        s.detail = None;
    });
    let spawned = std::thread::Builder::new()
        .name("hotkeys-x11".into())
        .spawn(move || {
            let result = listen(backend);
            WAKE_FD.store(-1, Ordering::Release);
            RUNNING.store(false, Ordering::Release);
            super::release_all_bindings();
            if let Err(detail) = result {
                log::warn!("[hotkeys] x11: {}", detail);
                update_status(|s| {
                    s.state = "error";
                    s.detail = Some(detail);
                });
            }
        });
    if let Err(e) = spawned {
        RUNNING.store(false, Ordering::Release);
        update_status(|s| {
            s.state = "error";
            s.detail = Some(format!("thread: {}", e));
        });
    }
}

fn wake() {
    let fd = WAKE_FD.load(Ordering::Acquire);
    if fd >= 0 {
        unsafe { libc::write(fd, [1u8].as_ptr().cast(), 1) };
    }
}

/// Where a watched key's state is read from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Key(Keycode),
    Button(u8),
}

fn source_of(key: &str) -> Option<Source> {
    match key {
        keys::MOUSE_MIDDLE => Some(Source::Button(2)),
        keys::MOUSE_BACK => Some(Source::Button(8)),
        keys::MOUSE_FORWARD => Some(Source::Button(9)),
        _ => keys::evdev_code(key).and_then(|c| u8::try_from(c + 8).ok()).map(Source::Key),
    }
}

/// A bound combo as grabbed, and how its release is told apart.
struct Grabbed {
    id: String,
    action: Action,
    source: Source,
    /// Modifiers the rest of the combo requires (exact for press actions
    /// and modified holds).
    required: u16,
    /// A hold with no modifiers: grabbed under any modifiers.
    any: bool,
    /// Modifier mask of the main key itself, when it is a modifier
    /// (modifier-only combos): its release shows in the pointer query.
    main_modifier: u16,
}

#[cfg(test)]
pub(super) static KEYMAP_QUERIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Which ModN bits Alt, Super and NumLock live on (from the server's
/// modifier mapping; the usual Mod1 / Mod4 / Mod2 if not found).
struct Masks {
    alt: u16,
    meta: u16,
    num_lock: u16,
}

impl Masks {
    fn read(conn: &RustConnection) -> Result<Self, String> {
        let reply = conn
            .get_modifier_mapping()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("GetModifierMapping: {}", e))?;
        let per = (reply.keycodes.len() / 8).max(1);
        let kc = |key: &str| keys::evdev_code(key).map(|c| (c + 8) as u8);
        let mask_of = |codes: &[Option<u8>]| -> u16 {
            let mut mask = 0;
            for (i, chunk) in reply.keycodes.chunks(per).enumerate().take(8) {
                if chunk.iter().any(|k| *k != 0 && codes.contains(&Some(*k))) {
                    mask |= 1 << i;
                }
            }
            mask
        };
        let alt = mask_of(&[kc("AltLeft"), kc("AltRight")]);
        let meta = mask_of(&[kc("MetaLeft"), kc("MetaRight")]);
        let num_lock = mask_of(&[kc("NumLock")]);
        Ok(Masks {
            alt: if alt != 0 { alt } else { u16::from(ModMask::M1) },
            meta: if meta != 0 { meta } else { u16::from(ModMask::M4) },
            num_lock,
        })
    }

    /// The modifier bits a combo can require (locks excluded).
    fn relevant(&self) -> u16 {
        u16::from(ModMask::CONTROL) | u16::from(ModMask::SHIFT) | self.alt | self.meta
    }

    fn of(&self, m: keys::Modifier) -> u16 {
        match m {
            keys::Modifier::Control => u16::from(ModMask::CONTROL),
            keys::Modifier::Shift => u16::from(ModMask::SHIFT),
            keys::Modifier::Alt => self.alt,
            keys::Modifier::Meta => self.meta,
        }
    }

    /// Caps Lock / Num Lock state must not stop a combo from matching.
    fn lock_variants(&self) -> Vec<u16> {
        let lock = u16::from(ModMask::LOCK);
        let mut v = vec![0, lock];
        if self.num_lock != 0 {
            v.extend([self.num_lock, lock | self.num_lock]);
        }
        v
    }
}

/// The grab a binding needs: its main key (the non-modifier, or the last
/// key of a modifier-only combo), the modifiers the rest require, and the
/// main key's own modifier bit when it is one.
fn grab_spec(b: &Binding, masks: &Masks) -> Option<(Source, u16, u16)> {
    let parts: Vec<&'static str> = b.keys.iter().map(|k| keys::intern(k)).collect::<Option<_>>()?;
    let main_idx = parts
        .iter()
        .rposition(|k| keys::generic_modifier(k).is_none() && keys::sided_modifier(k).is_none())
        .or_else(|| parts.len().checked_sub(1))?;
    let main = parts[main_idx];
    if keys::generic_modifier(main).is_some() {
        return None;
    }
    let mut required = 0;
    for (i, k) in parts.iter().enumerate() {
        if i != main_idx {
            if let Some(m) = keys::generic_modifier(k).or_else(|| keys::sided_modifier(k)) {
                required |= masks.of(m);
            }
        }
    }
    let main_modifier = keys::sided_modifier(main).map_or(0, |m| masks.of(m));
    Some((source_of(main)?, required, main_modifier))
}

/// Grab every binding's combo on the root window. Returns what was
/// grabbed and the binding ids the server refused (another client holds
/// the combo).
fn grab_all(
    conn: &RustConnection,
    root: Window,
    masks: &Masks,
    xi: bool,
    bindings: &[Binding],
) -> (Vec<Grabbed>, Vec<String>) {
    let mut grabbed = Vec::new();
    let mut failed = Vec::new();
    for b in bindings {
        let Some((source, required, main_modifier)) = grab_spec(b, masks) else {
            failed.push(b.id.clone());
            continue;
        };
        // A hold with no modifiers grabs under any modifiers, so Shift+V
        // still talks when push-to-talk is V; everything else is exact.
        let variants = if b.action.is_hold() && required == 0 {
            vec![u16::from(ModMask::ANY)]
        } else {
            masks.lock_variants().into_iter().map(|l| required | l).collect()
        };
        let mut ok = true;
        for mods in variants {
            let result = match source {
                Source::Key(kc) => conn
                    .grab_key(false, root, ModMask::from(mods), kc, GrabMode::ASYNC, GrabMode::SYNC)
                    .map_err(|e| e.to_string())
                    .and_then(|c| c.check().map_err(|e| e.to_string())),
                Source::Button(_) if !xi => Err("XInput 2 is needed for mouse buttons".into()),
                Source::Button(n) => conn
                    .grab_button(
                        false,
                        root,
                        EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE,
                        GrabMode::SYNC,
                        GrabMode::ASYNC,
                        x11rb::NONE,
                        x11rb::NONE,
                        ButtonIndex::from(n),
                        ModMask::from(mods),
                    )
                    .map_err(|e| e.to_string())
                    .and_then(|c| c.check().map_err(|e| e.to_string())),
            };
            if let Err(e) = result {
                log::info!("[hotkeys] x11 grab refused for {}: {}", b.id, e);
                ok = false;
            }
        }
        if ok {
            grabbed.push(Grabbed {
                id: b.id.clone(),
                action: b.action,
                source,
                required,
                any: b.action.is_hold() && required == 0,
                main_modifier,
            });
        } else {
            failed.push(b.id.clone());
        }
    }
    (grabbed, failed)
}

fn ungrab_all(conn: &RustConnection, root: Window) -> Result<(), String> {
    conn.ungrab_key(0u8, root, ModMask::ANY).map_err(|e| e.to_string())?;
    conn.ungrab_button(ButtonIndex::ANY, root, ModMask::ANY).map_err(|e| e.to_string())?;
    Ok(())
}

fn listen(backend: &'static str) -> Result<(), String> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| format!("connect: {}", e))?;
    let root = conn.setup().roots[screen].root;
    let masks = Masks::read(&conn)?;
    // XInput 2 only to read side-button state (core masks stop at 5).
    let pointer = conn
        .xinput_xi_query_version(2, 0)
        .ok()
        .and_then(|c| c.reply().ok())
        .filter(|v| v.major_version >= 2)
        .and_then(|_| conn.xinput_xi_get_client_pointer(x11rb::NONE).ok())
        .and_then(|c| c.reply().ok())
        .map(|r| if r.set { r.deviceid } else { 2 });

    let mut pipe = [-1i32; 2];
    if unsafe { libc::pipe2(pipe.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err("wake pipe".into());
    }
    WAKE_FD.store(pipe[1], Ordering::Release);
    let result = run(&conn, root, &masks, pointer, pipe[0], backend);
    WAKE_FD.store(-1, Ordering::Release);
    unsafe {
        libc::close(pipe[0]);
        libc::close(pipe[1]);
    }
    result
}

fn run(
    conn: &RustConnection,
    root: Window,
    masks: &Masks,
    pointer: Option<u16>,
    wake_fd: i32,
    backend: &'static str,
) -> Result<(), String> {
    let mut generation = u64::MAX;
    let mut grabbed: Vec<Grabbed> = Vec::new();
    // Indices into `grabbed` of holds currently down.
    let mut held: Vec<usize> = Vec::new();
    let mut presses: Vec<(Source, u16)> = Vec::new();
    loop {
        let current = GENERATION.load(Ordering::Acquire);
        if current != generation {
            generation = current;
            ungrab_all(conn, root)?;
            conn.flush().map_err(|e| e.to_string())?;
            let bindings = super::bindings();
            if bindings.is_empty() {
                RUNNING.store(false, Ordering::Release);
                // A bind racing this exit saw RUNNING still set and didn't
                // spawn; take it over rather than leave it unwatched.
                if super::bindings().is_empty() || RUNNING.swap(true, Ordering::AcqRel) {
                    return Ok(());
                }
                generation = u64::MAX;
                continue;
            }
            super::release_all_bindings();
            held.clear();
            presses.clear();
            let failed;
            (grabbed, failed) = grab_all(conn, root, masks, pointer.is_some(), &bindings);
            update_status(|s| {
                s.backend = backend;
                s.state = "active";
                s.detail = None;
                s.failed = failed;
            });
        }

        // Grabbed presses (plus anything a reply read brought in with it).
        while let Some(event) = conn.poll_for_event().map_err(|e| e.to_string())? {
            take_press(conn, event, &mut presses)?;
        }
        if !presses.is_empty() {
            conn.flush().map_err(|e| e.to_string())?;
        }
        for (source, state) in presses.drain(..) {
            for (i, g) in grabbed.iter().enumerate() {
                let mods = state & masks.relevant();
                if g.source != source || !(g.any || mods == g.required) {
                    continue;
                }
                if !g.action.is_hold() {
                    dispatch(&g.id, g.action, true);
                } else if !held.contains(&i) {
                    // Auto-repeat re-grabs a held key; only the first counts.
                    held.push(i);
                    dispatch(&g.id, g.action, true);
                }
            }
        }
        if !held.is_empty() {
            check_releases(conn, root, pointer, &grabbed, &mut held)?;
            if let Some(event) = conn.poll_for_event().map_err(|e| e.to_string())? {
                take_press(conn, event, &mut presses)?;
                continue;
            }
        }
        conn.flush().map_err(|e| e.to_string())?;
        wait(conn.stream().as_raw_fd(), wake_fd, if held.is_empty() { -1 } else { POLL_INTERVAL_MS })?;
    }
}

/// A grabbed press: drop the grab at once (input was frozen for it).
fn take_press(conn: &RustConnection, event: Event, presses: &mut Vec<(Source, u16)>) -> Result<(), String> {
    match event {
        Event::KeyPress(e) => {
            conn.ungrab_keyboard(x11rb::CURRENT_TIME).map_err(|e| e.to_string())?;
            presses.push((Source::Key(e.detail), u16::from(e.state)));
        }
        Event::ButtonPress(e) => {
            conn.ungrab_pointer(x11rb::CURRENT_TIME).map_err(|e| e.to_string())?;
            presses.push((Source::Button(e.detail), u16::from(e.state)));
        }
        _ => {}
    }
    Ok(())
}

/// Release every held combo whose main key, button or a required modifier
/// is up. Asks the pointer query (buttons + modifier state) always, and
/// the keymap only while a hold on an ordinary key is down.
fn check_releases(
    conn: &RustConnection,
    root: Window,
    pointer: Option<u16>,
    grabbed: &[Grabbed],
    held: &mut Vec<usize>,
) -> Result<(), String> {
    let (mods, buttons): (u16, Vec<u32>) = match pointer {
        Some(device) => {
            let r = conn
                .xinput_xi_query_pointer(root, device)
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| format!("XIQueryPointer: {}", e))?;
            ((r.mods.effective & 0xff) as u16, r.buttons)
        }
        None => {
            // Core query: modifiers in the low byte, buttons 1–5 above.
            let mask = u16::from(
                conn.query_pointer(root)
                    .map_err(|e| e.to_string())?
                    .reply()
                    .map_err(|e| format!("QueryPointer: {}", e))?
                    .mask,
            );
            let buttons = (1..=5u32).filter(|b| mask & (1 << (7 + b)) != 0).fold(0u32, |w, b| w | 1 << b);
            (mask & 0xff, vec![buttons])
        }
    };
    let ordinary_key = |g: &Grabbed| matches!(g.source, Source::Key(_)) && g.main_modifier == 0;
    let keymap = if held.iter().any(|&i| ordinary_key(&grabbed[i])) {
        #[cfg(test)]
        KEYMAP_QUERIES.fetch_add(1, Ordering::Relaxed);
        Some(
            conn.query_keymap()
                .map_err(|e| e.to_string())?
                .reply()
                .map_err(|e| format!("QueryKeymap: {}", e))?
                .keys,
        )
    } else {
        None
    };
    held.retain(|&i| {
        let g = &grabbed[i];
        let main_down = match g.source {
            Source::Button(n) => buttons.get(n as usize / 32).is_some_and(|w| w & (1 << (n % 32)) != 0),
            Source::Key(_) if g.main_modifier != 0 => mods & g.main_modifier != 0,
            // Only this key's bit is read.
            Source::Key(kc) => keymap.is_some_and(|k| k[kc as usize / 8] & (1 << (kc % 8)) != 0),
        };
        let still = main_down && mods & g.required == g.required;
        if !still {
            dispatch(&g.id, g.action, false);
        }
        still
    });
    Ok(())
}

fn wait(x_fd: i32, wake_fd: i32, timeout_ms: i32) -> Result<(), String> {
    let mut fds = [
        libc::pollfd { fd: x_fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: wake_fd, events: libc::POLLIN, revents: 0 },
    ];
    let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
    if r < 0 {
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(format!("poll: {}", err));
        }
    }
    if fds[1].revents & libc::POLLIN != 0 {
        let mut buf = [0u8; 64];
        while unsafe { libc::read(wake_fd, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::Action;
    use super::*;

    fn masks() -> Masks {
        Masks { alt: 8, meta: 64, num_lock: 16 }
    }

    fn binding(action: Action, keys: &[&str]) -> Binding {
        Binding { id: "b".into(), action, keys: keys.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn grab_specs() {
        let m = masks();
        let control = u16::from(ModMask::CONTROL);
        let shift = u16::from(ModMask::SHIFT);
        // KeyM = evdev 50 → keycode 58.
        assert!(grab_spec(&binding(Action::ToggleMute, &["Control", "Shift", "KeyM"]), &m)
            == Some((Source::Key(58), control | shift, 0)));
        assert!(grab_spec(&binding(Action::PushToTalk, &["Meta", "Mouse4"]), &m) == Some((Source::Button(8), 64, 0)));
        // Modifier-only: the last key is the grab, the rest its modifiers,
        // and its own modifier bit tells the release.
        assert!(grab_spec(&binding(Action::PushToTalk, &["ControlRight"]), &m) == Some((Source::Key(105), 0, control)));
        assert!(
            grab_spec(&binding(Action::PushToTalk, &["ControlLeft", "AltLeft"]), &m)
                == Some((Source::Key(64), control, 8))
        );
        assert!(grab_spec(&binding(Action::ToggleMute, &["Nope"]), &m).is_none());
    }

    #[test]
    fn lock_variants_cover_caps_and_num_lock() {
        assert_eq!(masks().lock_variants(), vec![0, 2, 16, 18]);
        assert_eq!(Masks { alt: 8, meta: 64, num_lock: 0 }.lock_variants(), vec![0, 2]);
    }
}
