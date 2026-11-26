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

    /// Waits until the window has drawn and presented nothing for 250 ms and returns the
    /// counters. Fails after 10 s of continuous drawing.
    pub async fn settle(&self) -> Result<Frames> {
        let started = Instant::now();
        let mut last = self.frames()?;
        let mut quiet_since = Instant::now();
        loop {
            self.sleep(Duration::from_millis(50)).await;
            let now = self.frames()?;
            if now != last {
                last = now;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= Duration::from_millis(250) {
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

    /// A line in the test's log.
    pub fn log(&self, message: impl std::fmt::Display) {
        println!("    {message}");
    }
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

    let mut failed = 0;
    for test in &selected {
        println!("selftest {}: {}", test.name, test.about);
        let started = Instant::now();
        let result = (test.run)(cx.clone()).await;
        let ms = started.elapsed().as_millis();
        match result {
            Ok(()) => println!("selftest {}: PASS ({ms} ms)", test.name),
            Err(error) => {
                failed += 1;
                println!("selftest {}: FAIL ({ms} ms): {error:#}", test.name);
            }
        }
    }
    println!(
        "selftest: {} passed, {failed} failed",
        selected.len() - failed
    );
    failed == 0
}
