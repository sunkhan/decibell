use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use futures::future::LocalBoxFuture;
use gpui::{
    AnyWindowHandle, App, AsyncApp, KeyCode, KeyDownEvent, KeyUpEvent, Keystroke, ModifierKey, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point,
    Window,
};

/// A scripted check run by `decibell --selftest <name>|all` inside the real app: real platform,
/// real window, real frame loop. (GPUI's `TestAppContext` draws synchronously with a fake
/// platform, so it cannot show that a screen stops drawing; that is what these are for.
/// Logic that needs no window belongs in `#[gpui::test]` / plain unit tests instead.)
///
/// ```ignore
/// pub fn selftests() -> Vec<SelfTest> {
///     vec![SelfTest { name: "boot", about: "…", run: |cx| Box::pin(boot(cx)) }]
/// }
/// async fn boot(cx: TestCx) -> anyhow::Result<()> { … }
/// ```
pub struct SelfTest {
    /// Lower-case, dash-separated, unique across the workspace.
    pub name: &'static str,
    /// One line for `--selftest list`.
    pub about: &'static str,
    pub run: fn(TestCx) -> LocalBoxFuture<'static, Result<()>>,
}

/// A self-test that could not measure what it is for, through no fault of the code under test:
/// another window took the desktop's focus during an idle measurement, the real pointer rested on
/// the window, … Return it (`Err(inconclusive("why").into())`) instead of failing; the run reports
/// it as INCONCLUSIVE with the reason, apart from passes and failures. Never return it for
/// something the code did, and never pass a test that didn't measure.
#[derive(Debug, Clone)]
pub struct Inconclusive(pub String);

impl std::fmt::Display for Inconclusive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "inconclusive: {}", self.0)
    }
}

impl std::error::Error for Inconclusive {}

/// See [`Inconclusive`].
pub fn inconclusive(reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Inconclusive(reason.to_string()))
}

/// Frame counters of the test window (`Window::frames_drawn` / `frames_presented`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frames {
    pub drawn: u64,
    pub presented: u64,
}

impl Frames {
    /// Frames since `earlier`.
    pub fn since(self, earlier: Frames) -> Frames {
        Frames {
            drawn: self.drawn - earlier.drawn,
            presented: self.presented - earlier.presented,
        }
    }
}

/// What a self-test drives the app through: the app, the main window, and helpers to wait,
/// send input and read the frame counters.
#[derive(Clone)]
pub struct TestCx {
    cx: AsyncApp,
    window: AnyWindowHandle,
}

impl TestCx {
    pub fn new(cx: AsyncApp, window: AnyWindowHandle) -> Self {
        Self { cx, window }
    }

    pub fn app(&self) -> &AsyncApp {
        &self.cx
    }

    pub fn window(&self) -> AnyWindowHandle {
        self.window
    }

    /// Runs `f` with the window, then lets GPUI flush effects (a `notify` inside schedules
    /// its frame). Fails if the window was closed.
    pub fn update<R>(&self, f: impl FnOnce(&mut Window, &mut App) -> R) -> Result<R> {
        self.cx.update(|cx| {
            self.window
                .update(cx, |_, window, cx| f(window, cx))
                .context("the test window is closed")
        })
    }

    pub fn frames(&self) -> Result<Frames> {
        self.update(|window, _| Frames {
            drawn: window.frames_drawn(),
            presented: window.frames_presented(),
        })
    }

    /// One timer: the main thread sleeps until it fires.
    pub async fn sleep(&self, duration: Duration) {
        self.cx.background_executor().timer(duration).await;
    }

    /// Waits until the window has drawn nothing for 250 ms and returns the counters: presented
    /// nothing either for 250 ms, or only presented (no draw) for 1.5 s since the last draw, as a
    /// present-only animation does (the active server tile's glow breathes while the window has
    /// the desktop's focus, a speaking ring: they re-present a retained scene and never draw).
    /// GPUI's own presents after a burst of input stop within 1 s. Fails after 10 s of drawing.
    pub async fn settle(&self) -> Result<Frames> {
        let started = Instant::now();
        let mut last = self.frames()?;
        let (mut last_draw, mut last_present) = (Instant::now(), Instant::now());
        loop {
            self.sleep(Duration::from_millis(50)).await;
            let now = self.frames()?;
            if now.drawn != last.drawn {
                last_draw = Instant::now();
            }
            if now.presented != last.presented {
                last_present = Instant::now();
            }
            last = now;
            let quiet = |since: Instant, ms: u64| since.elapsed() >= Duration::from_millis(ms);
            if quiet(last_draw, 250) && (quiet(last_present, 250) || quiet(last_draw, 1500)) {
                return Ok(now);
            }
            if started.elapsed() > Duration::from_secs(10) {
                bail!("the window was still drawing after 10 s ({now:?})");
            }
        }
    }

    /// Sends a keystroke (`"ctrl-t"`, `"escape"`) to the window as the platform would.
    /// Returns whether something handled it.
    pub fn keystroke(&self, keystroke: &'static str) -> Result<bool> {
        let keystroke = Keystroke::parse(keystroke)?;
        self.update(|window, cx| window.dispatch_keystroke(keystroke, cx))
    }

    /// Types `text` one character at a time, each as the key press the platform sends for
    /// it: key listeners first, then the focused field's input handler.
    pub fn type_text(&self, text: &str) -> Result<()> {
        let mut buffer = [0; 4];
        for character in text.chars() {
            let typed = character.encode_utf8(&mut buffer);
            let keystroke = Keystroke {
                modifiers: Modifiers::default(),
                key: if character == ' ' { "space".into() } else { typed.to_lowercase() },
                key_char: Some(typed.to_string()),
            };
            self.update(|window, cx| window.dispatch_keystroke(keystroke, cx))?;
        }
        Ok(())
    }

    /// A key going down or up with its **physical key** (`KeyCode`, GPUI patch 0012), as a
    /// platform reports it: `keystroke` is what the layout makes of the key (`"ş"`),
    /// `code` which key it is. Key listeners see it; no text is inserted.
    pub fn key_event(&self, keystroke: Keystroke, code: KeyCode, down: bool) -> Result<()> {
        self.input(if down {
            PlatformInput::KeyDown(KeyDownEvent {
                keystroke,
                is_held: false,
                prefer_character_input: false,
                code,
            })
        } else {
            PlatformInput::KeyUp(KeyUpEvent { keystroke, code })
        })
    }

    /// A modifier key going down or up, as Linux and Windows report it: the modifiers as
    /// they are after it, and which physical key moved.
    pub fn modifier_key(&self, code: KeyCode, pressed: bool, modifiers: Modifiers) -> Result<()> {
        self.input(PlatformInput::ModifiersChanged(ModifiersChangedEvent {
            modifiers,
            capslock: Default::default(),
            key: Some(ModifierKey { code, pressed }),
        }))
    }

    /// A left button press at `position`: `clicks` 2 is the second press of a double click.
    pub fn mouse_down(&self, position: Point<Pixels>, clicks: usize, modifiers: Modifiers) -> Result<()> {
        self.mouse_move(position)?;
        self.input(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers,
            click_count: clicks,
            first_mouse: false,
        }))
    }

    /// The pointer moving with the left button held.
    pub fn mouse_drag(&self, position: Point<Pixels>) -> Result<()> {
        self.input(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        }))
    }

    pub fn mouse_up(&self, position: Point<Pixels>) -> Result<()> {
        self.input(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }))
    }

    pub fn mouse_move(&self, position: Point<Pixels>) -> Result<()> {
        self.input(PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: None,
            modifiers: Modifiers::default(),
        }))
    }

    /// A left click: move, press, release.
    pub fn click(&self, position: Point<Pixels>) -> Result<()> {
        self.mouse_move(position)?;
        self.input(PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
            first_mouse: false,
        }))?;
        self.input(PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left,
            position,
            modifiers: Modifiers::default(),
            click_count: 1,
        }))
    }

    fn input(&self, event: PlatformInput) -> Result<()> {
        self.update(|window, cx| {
            window.dispatch_event(event, cx);
        })
    }

    /// A line in the test's log. A parent relaying a `--check` child's log through here notes a
    /// child's INCONCLUSIVE: the parent's own pass then reads INCONCLUSIVE too (see
    /// [`run_selftests`]).
    pub fn log(&self, message: impl std::fmt::Display) {
        let line = message.to_string();
        if let Some((_, reason)) = line.split_once(": INCONCLUSIVE (") {
            let reason = reason.split_once("): ").map_or(reason, |(_, r)| r).to_string();
            RELAYED_INCONCLUSIVE.with(|r| {
                let mut r = r.borrow_mut();
                if r.is_none() {
                    *r = Some(reason);
                }
            });
        }
        println!("    {line}");
    }
}

thread_local! {
    /// A child's INCONCLUSIVE relayed by the test running now ([`TestCx::log`]).
    static RELAYED_INCONCLUSIVE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Runs the tests named by `selection` (`all`, one name, or names separated by commas) in
/// order and prints a log. Returns whether all of them passed.
pub async fn run_selftests(tests: Vec<SelfTest>, selection: &str, cx: TestCx) -> bool {
    let selected: Vec<&SelfTest> = if selection == "all" {
        tests.iter().collect()
    } else {
        let mut selected = Vec::new();
        for name in selection.split(',') {
            match tests.iter().find(|test| test.name == name) {
                Some(test) => selected.push(test),
                None => {
                    println!("selftest: no test named {name:?}. Tests:");
                    for test in &tests {
                        println!("  {:<16} {}", test.name, test.about);
                    }
                    return false;
                }
            }
        }
        selected
    };

    let (mut failed, mut unmeasured) = (0, 0);
    for test in &selected {
        println!("selftest {}: {}", test.name, test.about);
        let started = Instant::now();
        RELAYED_INCONCLUSIVE.with(|r| r.borrow_mut().take());
        let result = (test.run)(cx.clone()).await;
        let ms = started.elapsed().as_millis();
        // A parent whose child said INCONCLUSIVE passed only its own part: it is inconclusive too.
        let result = match (result, RELAYED_INCONCLUSIVE.with(|r| r.borrow_mut().take())) {
            (Ok(()), Some(reason)) => Err(inconclusive(format!("the app's check: {reason}"))),
            (result, _) => result,
        };
        match result {
            Ok(()) => println!("selftest {}: PASS ({ms} ms)", test.name),
            Err(error) if error.downcast_ref::<Inconclusive>().is_some() => {
                unmeasured += 1;
                let reason = error.downcast_ref::<Inconclusive>().map(|i| i.0.clone()).unwrap_or_default();
                println!("selftest {}: INCONCLUSIVE ({ms} ms): {reason}", test.name);
            }
            Err(error) => {
                failed += 1;
                println!("selftest {}: FAIL ({ms} ms): {error:#}", test.name);
            }
        }
    }
    println!(
        "selftest: {} passed, {failed} failed, {unmeasured} inconclusive",
        selected.len() - failed - unmeasured
    );
    failed == 0
}
