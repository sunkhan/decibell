use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result};
use decibell_harness::perf;
#[cfg(feature = "dev")]
use decibell_harness::{TestCx, run_selftests};
#[cfg(feature = "dev")]
use decibell_theme::ThemeId;
use decibell_ui::Assets;
#[cfg(feature = "dev")]
use decibell_ui::{ActiveTheme as _, Theme};
use gpui::{App, AppContext as _};
#[cfg(feature = "dev")]
use gpui::{KeyBinding, actions};

use crate::args::{Args, usage};
use crate::window;
#[cfg(feature = "dev")]
use crate::registry;

#[cfg(feature = "dev")]
actions!(decibell, [NextTheme, PreviousTheme]);

/// What the window shows.
enum Mode {
    /// The app: the model, the core, the sign-in page or the main layout.
    App,
    /// A registered screen (fixture state, no core): `--screen <name>`, and the self-tests'
    /// window (the default screen). Development builds only.
    #[cfg(feature = "dev")]
    Screen(String),
}

/// Runs the app until its last window closes (or its self-tests finish).
pub fn run(args: Args) -> ExitCode {
    // A panic that unwinds the UI thread to here ends the process with a report (see the guard).
    let _abort_on_unwind = crate::crash::AbortOnUnwind;
    if args.help {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("decibell {}", crate::VERSION);
        return ExitCode::SUCCESS;
    }
    #[cfg(feature = "dev")]
    if args.screen.as_deref() == Some("list") {
        let screens = registry::screens();
        print!(
            "{}",
            registry::listing(screens.iter().map(|s| (s.name, s.about)))
        );
        return ExitCode::SUCCESS;
    }
    #[cfg(feature = "dev")]
    if args.selftest.as_deref() == Some("list") {
        let tests = registry::selftests();
        print!(
            "{}",
            registry::listing(tests.iter().map(|t| (t.name, t.about)))
        );
        return ExitCode::SUCCESS;
    }
    crate::logging::init();
    #[cfg(feature = "dev")]
    if args.layout_labels {
        return crate::hotkeys::print_layout_labels();
    }
    // The installer's Start menu shortcut (Windows), written by this binary for its AppUserModelID.
    if let Some(link) = &args.create_shortcut {
        return crate::updater::create_shortcut(link);
    }
    if args.perf {
        perf::enable_report();
    }
    // The taskbar's identity (Windows): the same id as the Start menu shortcut's, before any window.
    #[cfg(windows)]
    decibell_release::win::set_app_user_model_id(decibell_release::APP_USER_MODEL_ID);
    // The app itself (not a screen, a self-test's window or the benchmark): its profile, and one
    // instance per profile (a second start hands its links / hotkey to the first and exits before
    // any window or GPUI starts).
    let app = if is_app(&args) {
        match claim_profile(&args) {
            Ok(Some(app)) => Some(app),
            Ok(None) => return ExitCode::SUCCESS,
            Err(message) => {
                eprintln!("decibell: {message:#}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    // A downloaded update is applied by this (old) version before anything starts: the new one
    // runs on trial, and on failure this one carries on (decibell_release::apply).
    let app = match app {
        Some(app) => match apply_staged_update(app, &args) {
            Ok(app) => Some(app),
            Err(code) => return code,
        },
        None => None,
    };
    if let Some(app) = &app {
        decibell_release::apply::clean_up_after_update();
        // Crash reports (packaged ship builds with a DSN and the Privacy toggle on).
        crate::crash::start(&app.profile);
    }
    // `--login` exists in development builds only, and only for the local test stack.
    let login = match &args.login {
        None => None,
        Some(account) => match dev_login(account) {
            Ok(password) => Some((account.clone(), password)),
            Err(message) => {
                eprintln!("decibell: --login refused: {message:#}");
                return ExitCode::from(2);
            }
        },
    };

    // Before the first window: the renderer's GPU device is then created able to decode video
    // (two idle queues; nothing else exists until a stream is displayed).
    decibell_video::VideoDevice::install();
    // The core's received frames come in pooled buffers sized for the hub (docs/video-design.md
    // §0.10); before the first stream is watched.
    decibell_video::enable_frame_pool();

    let exit_code = Arc::new(AtomicU8::new(0));
    gpui_platform::application().with_assets(Assets).run({
        let exit_code = exit_code.clone();
        move |cx| {
            if let Err(error) = start(&args, login, app, exit_code.clone(), cx) {
                // Nothing is running yet, and `cx.quit()` from here would be lost: the
                // platform's event loop only starts after this callback.
                eprintln!("decibell: {error:#}");
                std::process::exit(1);
            }
        }
    });
    // The event loop is gone. Stream playback first: its decoder threads free their decoders
    // through the GPU driver, which must not be running its exit handlers yet.
    decibell_video::shutdown(Duration::from_secs(2));
    // Then the core and its runtime, bounded (a call's goodbye, the connections' close).
    decibell_model::core::shutdown(Duration::from_secs(3));
    perf::report_frames();
    // Settings → About's Restart: this version starts again to apply the update (and exits now,
    // giving up the profile).
    crate::updater::relaunch_if_requested();
    ExitCode::from(exit_code.load(Ordering::Relaxed))
}

/// Applies a staged update at start (self-update installs only). `Err`: this process is done
/// (the new version runs, or the profile was handed over).
fn apply_staged_update(app: AppStart, args: &Args) -> Result<AppStart, ExitCode> {
    if args.update_trial.is_some() {
        return Ok(app);
    }
    let Some(staged) = crate::updater::staged_to_apply(&app.profile) else { return Ok(app) };
    let dir = crate::updater::update_dir(&app.profile);
    // The profile's lock and endpoint go with it: the new version must become the instance.
    drop(app);
    let outcome = decibell_release::apply::apply(&dir, &staged, &crate::updater::trial_args());
    if outcome == decibell_release::apply::Outcome::Started {
        return Err(ExitCode::SUCCESS);
    }
    match claim_profile(args) {
        Ok(Some(app)) => Ok(app),
        Ok(None) => Err(ExitCode::SUCCESS),
        Err(message) => {
            eprintln!("decibell: {message:#}");
            Err(ExitCode::from(2))
        }
    }
}

/// What the app (as opposed to a screen) runs with: its profile, what its command line asked for,
/// and the profile's instance lock when it holds it.
pub(crate) struct AppStart {
    pub(crate) profile: std::path::PathBuf,
    launch: decibell_platform::Launch,
    instance: Option<decibell_platform::Instance>,
}

fn is_app(args: &Args) -> bool {
    args.screen.is_none() && args.selftest.is_none() && args.bench_scroll.is_none()
}

/// The profile (refused if a development build would use the shipped one), then the instance:
/// `Ok(None)` when this start's work was handed to the running instance (or there was nothing to
/// do: `--hotkey` with no instance, as Electron's main process exits).
fn claim_profile(args: &Args) -> Result<Option<AppStart>> {
    let profile = decibell_model::core::default_user_data_dir()?;
    decibell_model::core::check_profile(&profile)?;
    if args.apply_update {
        // Restart started us while the old window was closing: wait for it to let go of the
        // profile, then become its instance (not hand over to it).
        crate::updater::wait_for_profile(&profile);
    }
    let launch = decibell_platform::Launch::new(&args.links, args.hotkey.as_deref());
    let instance = match decibell_platform::instance::start(&profile, &launch) {
        Ok(decibell_platform::Start::First(instance)) => Some(instance),
        Ok(decibell_platform::Start::Forwarded { took }) => {
            log::info!("[instance] handed to the running instance in {} ms", took.as_millis());
            #[cfg(feature = "dev-local")]
            println!("decibell: platform forwarded in {} ms", took.as_millis());
            return Ok(None);
        }
        Err(e) => {
            // Without the lock the app still runs (as Electron would not): better two windows than
            // none.
            log::warn!("[instance] no single-instance lock: {e:#}");
            None
        }
    };
    if launch.hotkey.is_some() && instance.is_some() {
        eprintln!("decibell: --hotkey: Decibell isn't running (nothing to send the keybind to)");
        return Ok(None);
    }
    Ok(Some(AppStart { profile, launch, instance }))
}

#[cfg(feature = "dev-local")]
fn dev_login(account: &str) -> Result<String> {
    crate::dev::validate_login(account)
}

#[cfg(not(feature = "dev-local"))]
fn dev_login(_: &str) -> Result<String> {
    anyhow::bail!("this build has no development sign-in (feature dev-local)")
}

fn start(
    args: &Args,
    login: Option<(String, String)>,
    app: Option<AppStart>,
    exit_code: Arc<AtomicU8>,
    cx: &mut App,
) -> Result<()> {
    decibell_ui::init(cx, args.theme.unwrap_or_default())?;
    // Key labels in the keyboard layout's terms (Settings → Keybinds).
    crate::hotkeys::follow_keyboard_layout(cx);
    // Quitting with a stream on screen: its decoder threads leave before the windows and the
    // GPU device do (and before the driver's exit handlers: `run` checks again after the loop).
    cx.on_app_quit(|_| {
        decibell_video::shutdown(Duration::from_secs(1));
        async {}
    })
    .detach();
    // GPUI patch 0014's way back: every cached view under a dirty one renders, as at the pin.
    if std::env::var("DECIBELL_VIEW_REPLAY").is_ok_and(|value| value == "0") {
        gpui::set_nested_view_replay(false);
    }
    #[cfg(feature = "dev")]
    bind_theme_keys(cx);

    // `--check` runs one of the shell's checks in the signed-in app; `--selftest` runs in the
    // default screen's window (the shell's own tests start the app in child processes).
    #[cfg(feature = "dev")]
    let mode = match (&args.screen, &args.selftest) {
        (Some(name), _) => Mode::Screen(name.clone()),
        // The scroll benchmark opens its own window, as a self-test does.
        (None, None) if args.bench_scroll.is_some() => Mode::Screen(registry::DEFAULT_SCREEN.into()),
        (None, Some(_)) => Mode::Screen(registry::DEFAULT_SCREEN.into()),
        (None, None) => Mode::App,
    };
    #[cfg(not(feature = "dev"))]
    let mode = Mode::App;
    let window = match (mode, app) {
        (Mode::App, app) => {
            let app = app.context("the app's profile was not claimed")?;
            let model = crate::app::init_model(args.theme, Some(app.profile.clone()), cx);
            // `--check`: a self-test's signed-in child; its sounds never reach the real output.
            let cues = crate::audio::CueOutput::from_env(args.check.is_some());
            crate::app::start_core(&model, cues, cx);
            // The names a crash report must never carry (the account, the communities).
            crate::crash::watch_names(&model, cx);
            #[cfg(feature = "dev")]
            crate::crash::panic_in_app_for_test(cx);
            #[cfg(feature = "dev-local")]
            {
                crate::dev::print_markers(cx);
                if let Some((account, password)) = login {
                    crate::dev::login_when_booted(account, password, cx);
                }
            }
            #[cfg(not(feature = "dev-local"))]
            let _ = login;
            let window = window::open_main(|_, cx| cx.new(decibell_shell::Shell::new).into(), Some(&app.profile), cx)?;
            // The decode capabilities are probed once the first frame is up (crate::video).
            window.update(cx, |_, window, _| crate::video::probe_after_first_frame(window))?;
            // The updater: Settings → About's state and buttons, the first check after the first
            // frame; a version started on trial says it started.
            crate::updater::install(&model, &app.profile, args.update_trial.clone(), window.into(), cx);
            // The desktop around it: the first start's links, later starts' requests, attention,
            // taskbar progress, the quit guard, the window's state, sleep / lock.
            let overlay = decibell_platform::install(window.into(), app.launch, app.instance, Some(app.profile), cx);
            window.update(cx, |root, _, cx| root.set_overlay(overlay, cx))?;
            #[cfg(feature = "dev-local")]
            crate::dev::serve_control(window.into(), cx);
            window
        }
        #[cfg(feature = "dev")]
        (Mode::Screen(name), _) => {
            // Screens draw from fixtures: the model without a core (they fill it themselves).
            crate::app::init_model(args.theme, None, cx);
            let screen = registry::screen(&name)?;
            window::open_main(screen.build, None, cx)?
        }
    };

    #[cfg(feature = "dev")]
    {
        let tests = match (&args.selftest, &args.check) {
            (_, Some(check)) => Some((registry::checks(), check.clone())),
            (Some(selection), None) => Some((registry::selftests(), selection.clone())),
            (None, None) => None,
        };
        if let Some(seconds) = args.bench_scroll {
            let speeds = args.bench_speed.map_or(vec![1000., 3000.], |speed| vec![speed]);
            cx.spawn(async move |cx| {
                match decibell_chat::bench::run(cx.clone(), seconds, &speeds, None).await {
                    Ok(report) => println!("{report:#}"),
                    Err(error) => {
                        eprintln!("decibell: {error:#}");
                        exit_code.store(1, Ordering::Relaxed);
                    }
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
            return Ok(());
        }
        if let Some((tests, selection)) = tests {
            cx.spawn(async move |cx| {
                let test_cx = TestCx::new(cx.clone(), window.into());
                if !run_selftests(tests, &selection, test_cx).await {
                    exit_code.store(1, Ordering::Relaxed);
                }
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    }
    #[cfg(not(feature = "dev"))]
    let _ = (window, exit_code);
    Ok(())
}

/// F8 / Shift-F8 step through the themes: development builds only (the `themes` self-test's
/// keyboard path). Not in the shipped build: the step isn't saved (Settings → Appearance and the
/// next start would disagree with the screen) and a bound F8 couldn't be recorded as a keybind.
#[cfg(feature = "dev")]
fn bind_theme_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f8", NextTheme, None),
        KeyBinding::new("shift-f8", PreviousTheme, None),
    ]);
    cx.on_action(|_: &NextTheme, cx| step_theme(1, cx));
    cx.on_action(|_: &PreviousTheme, cx| step_theme(ThemeId::ALL.len() - 1, cx));
}

#[cfg(feature = "dev")]
fn step_theme(by: usize, cx: &mut App) {
    let next = ThemeId::ALL[(cx.theme().id.index() + by) % ThemeId::ALL.len()];
    Theme::set(cx, next);
}
