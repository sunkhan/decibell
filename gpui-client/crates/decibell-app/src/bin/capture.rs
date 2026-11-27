//! `decibell-capture`: renders registered screens to PNG files without opening a window.
//!
//! The screen is built into the same root view as in the app (title bar included) on GPUI's
//! headless test platform, with the real text system loaded with only the bundled fonts and
//! the real renderer drawing into an offscreen texture: no display server is needed, and the
//! same screen, theme, size and scale give the same picture. Linux only: GPUI has no headless
//! renderer for Direct3D.
//!
//! Built only with the `capture` feature, because it needs GPUI's `test-support` feature,
//! which must not be compiled into `decibell`:
//!
//! ```sh
//! cargo run -p decibell-app --features capture --bin decibell-capture -- \
//!     --screen gallery --all-themes --out-dir /tmp/gallery
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
usage: decibell-capture --screen <name> (--out <file.png> | --out-dir <dir>) [options]

  --screen <name>      a registered screen (--list prints them)
  --theme <id>         default nocturne
  --all-themes         one file per theme (needs --out-dir)
  --size <W>x<H>       the window's visible size in logical px, default 1280x800
  --scale <factor>     device pixels per logical px, default 1
  --out <file.png>     where to write a single capture
  --out-dir <dir>      write <dir>/<screen>-<theme>.png
  --list";

// Only the Linux build reads these.
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
struct Args {
    screen: String,
    themes: Vec<decibell_theme::ThemeId>,
    size: (f32, f32),
    scale: f32,
    out: Option<PathBuf>,
    out_dir: Option<PathBuf>,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Args>, String> {
    use decibell_theme::ThemeId;

    let (mut screen, mut theme, mut all_themes) = (None, ThemeId::default(), false);
    let (mut size, mut scale, mut out, mut out_dir) = ((1280., 800.), 1., None, None);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--screen" => screen = Some(value()?),
            "--theme" => theme = value()?.parse().map_err(|error| format!("{error}"))?,
            "--all-themes" => all_themes = true,
            "--size" => {
                let value = value()?;
                size = value
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|(w, h)| *w >= 1. && *h >= 1.)
                    .ok_or_else(|| format!("--size {value}: expected <W>x<H>"))?;
            }
            "--scale" => {
                let value = value()?;
                scale = value
                    .parse()
                    .ok()
                    .filter(|scale| (0.5..=4.).contains(scale))
                    .ok_or_else(|| format!("--scale {value}: expected a factor from 0.5 to 4"))?;
            }
            "--out" => out = Some(PathBuf::from(value()?)),
            "--out-dir" => out_dir = Some(PathBuf::from(value()?)),
            "--list" => {
                let screens = decibell_app::registry::screens();
                print!(
                    "{}",
                    decibell_app::registry::listing(screens.iter().map(|s| (s.name, s.about)))
                );
                return Ok(None);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let screen = screen.ok_or("--screen is required")?;
    if out.is_some() == out_dir.is_some() {
        return Err("give exactly one of --out and --out-dir".into());
    }
    if all_themes && out_dir.is_none() {
        return Err("--all-themes needs --out-dir".into());
    }
    let themes = if all_themes {
        ThemeId::ALL.to_vec()
    } else {
        vec![theme]
    };
    Ok(Some(Args {
        screen,
        themes,
        size,
        scale,
        out,
        out_dir,
    }))
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => return ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("decibell-capture: {message}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match capture(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("decibell-capture: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn capture(_: &Args) -> anyhow::Result<()> {
    anyhow::bail!("captures need GPUI's headless renderer, which exists on Linux only")
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn capture(args: &Args) -> anyhow::Result<()> {
    use std::sync::Arc;

    use anyhow::Context as _;
    use decibell_app::{registry, root::Root};
    use decibell_ui::{Assets, Theme};
    use gpui::{AnyWindowHandle, AppContext as _, HeadlessAppContext, px, size};

    // Dates, times and counts in en-US whatever this machine's regional settings: the captures
    // pair with the Electron renderer's, which `tools/electron-captures/run.sh` pins to en-US.
    decibell_model::regional::Regional::pin(Some(decibell_model::regional::Regional::en_us()));
    let screen = registry::screen(&args.screen)?;
    if let Some(dir) = &args.out_dir {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }

    // Only the bundled fonts: text the fonts don't cover shows as missing instead of
    // quietly borrowing a font this machine happens to have.
    let text_system = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts(
        "Decibell Inter",
    ));
    let mut cx = HeadlessAppContext::with_platform(text_system, Arc::new(Assets), || {
        gpui_platform::current_headless_renderer()
    });
    cx.update(|cx| {
        cx.set_global(decibell_ui::AssumeClientDecorations);
        decibell_ui::init(cx, args.themes[0])?;
        // What the crates hand each other through the app (the DM composer, …): the screens
        // build the rest themselves (the capture skips the app's boot). The shell's whole-window
        // screens (`main-*`) show the panes the features register, as the app does: their
        // crates' `init` (which joins them too).
        if args.screen.starts_with("main-") {
            if cx.try_global::<decibell_model::Model>().is_none() {
                decibell_model::Model::init(cx, None);
            }
            registry::init_features(cx);
        } else {
            registry::join_features(cx);
        }
        anyhow::Ok(())
    })?;

    // The app's window is client-decorated: its surface holds a margin around the visible
    // window. The capture keeps it, so the frame's shadow and corners are in the picture.
    let outer = decibell_ui::outer_window_size(size(px(args.size.0), px(args.size.1)));
    let build = screen.build;
    let window = cx.open_window(outer, move |window, cx| {
        let content = build(window, cx);
        cx.new(|cx| Root::new(content, window, cx))
    })?;
    let window: AnyWindowHandle = window.into();
    cx.simulate_window_scale_factor_change(window, args.scale);

    for theme in &args.themes {
        // Through the window: `HeadlessAppContext::update` alone does not flush effects, and
        // the capture would show the frame before the switch.
        cx.update_window(window, |_, _, cx| Theme::set(cx, *theme))?;
        cx.run_until_parked();
        let image = cx.capture_screenshot(window)?;
        let path = match (&args.out, &args.out_dir) {
            (Some(out), _) => {
                if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
                }
                out.clone()
            }
            (None, Some(dir)) => dir.join(format!("{}-{theme}.png", screen.name)),
            (None, None) => unreachable!("checked when parsing"),
        };
        image
            .save(&path)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("{} {}x{}", path.display(), image.width(), image.height());
    }
    Ok(())
}
