use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use anyhow::Result;
use decibell_harness::{TestCx, perf, run_selftests};
use decibell_theme::ThemeId;
use decibell_ui::{ActiveTheme as _, Assets, Theme};
use gpui::{App, KeyBinding, actions};

use crate::args::{Args, USAGE};
use crate::{registry, window};

actions!(decibell, [NextTheme, PreviousTheme]);

/// Runs the app until its last window closes (or its self-tests finish).
pub fn run(args: Args) -> ExitCode {
    if args.help {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if args.screen.as_deref() == Some("list") {
        let screens = registry::screens();
        print!(
            "{}",
            registry::listing(screens.iter().map(|s| (s.name, s.about)))
        );
        return ExitCode::SUCCESS;
    }
    if args.selftest.as_deref() == Some("list") {
        let tests = registry::selftests();
        print!(
            "{}",
            registry::listing(tests.iter().map(|t| (t.name, t.about)))
        );
        return ExitCode::SUCCESS;
    }
    crate::logging::init();
    if args.perf {
        perf::enable_report();
    }

    let exit_code = Arc::new(AtomicU8::new(0));
    gpui_platform::application().with_assets(Assets).run({
        let exit_code = exit_code.clone();
        move |cx| {
            if let Err(error) = start(&args, exit_code.clone(), cx) {
                // Nothing is running yet, and `cx.quit()` from here would be lost: the
                // platform's event loop only starts after this callback.
                eprintln!("decibell: {error:#}");
                std::process::exit(1);
            }
        }
    });
    perf::report_frames();
    ExitCode::from(exit_code.load(Ordering::Relaxed))
}

fn start(args: &Args, exit_code: Arc<AtomicU8>, cx: &mut App) -> Result<()> {
    decibell_ui::init(cx, args.theme)?;
    bind_theme_keys(cx);

    let screen = registry::screen(args.screen.as_deref().unwrap_or(registry::DEFAULT_SCREEN))?;
    let window = window::open_main(&screen, cx)?;

    if let Some(selection) = args.selftest.clone() {
        cx.spawn(async move |cx| {
            let test_cx = TestCx::new(cx.clone(), window.into());
            if !run_selftests(registry::selftests(), &selection, test_cx).await {
                exit_code.store(1, Ordering::Relaxed);
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    }
    Ok(())
}

/// F8 / Shift-F8 step through the themes. A development aid until Settings → Appearance
/// exists; the self-tests use it as their keyboard path.
fn bind_theme_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f8", NextTheme, None),
        KeyBinding::new("shift-f8", PreviousTheme, None),
    ]);
    cx.on_action(|_: &NextTheme, cx| step_theme(1, cx));
    cx.on_action(|_: &PreviousTheme, cx| step_theme(ThemeId::ALL.len() - 1, cx));
}

fn step_theme(by: usize, cx: &mut App) {
    let next = ThemeId::ALL[(cx.theme().id.index() + by) % ThemeId::ALL.len()];
    Theme::set(cx, next);
}
