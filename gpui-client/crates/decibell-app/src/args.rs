use decibell_theme::ThemeId;

/// The options every build takes.
const USAGE_COMMON: &str = "\
usage: decibell [options]

  --theme <id>          nocturne (default), matinee, graphite, graphite-light, console,
                        console-light, console-split
  --perf                print the first-frame time and frame counters on stderr
                        (also DECIBELL_PERF=1)
  --hotkey=<action>     run a keybind action in the running Decibell and exit (for desktops
                        without a shortcut service: bind this command in the window manager):
                        toggle_mute, toggle_deafen, leave_voice, answer_call, decline_call,
                        push_to_talk[:down|:up], push_to_mute[:down|:up]
  decibell://invite/<code>
                        open the invite (in the running Decibell, if there is one)
  --version             print the version and exit
  -h, --help

  DECIBELL_LOG=<level>  log records on stderr: off, error (default), warn, info, debug, trace";

/// What development builds add (feature `dev`; never the shipped build).
#[cfg(feature = "dev")]
const USAGE_DEV: &str = "

Development build:
  --screen <name>       show a registered screen instead of the app (--screen list)
  --login <account>     development builds (feature dev-local) only: sign in to the local test
                        stack as <account> (password from DECIBELL_DEV_PASSWORD; refused unless
                        DECIBELL_CENTRAL is a loopback address)
  --selftest <names>    run self-tests in the real window and exit: all, one name, or
                        names separated by commas (--selftest list)
  --bench-scroll <s>    scroll the chat fixture channel into history for <s> seconds per speed
                        (--speed <px/s>, default 1000 then 3000) and print the frame figures
                        as JSON (decibell_chat::bench)
  --dbv <a.dbv[,b.dbv]> the test streams `--screen video-test` plays; --mirror shows the
                        first one a second time in a mini-player
  --layout-labels       print each writing-system key's label in the display's keyboard layout
                        and exit (no window)

While the window is open, F8 / Shift-F8 switch to the next / previous theme (development builds
only; not saved).";

/// The help text.
pub fn usage() -> String {
    #[cfg(feature = "dev")]
    return format!("{USAGE_COMMON}{USAGE_DEV}");
    #[cfg(not(feature = "dev"))]
    USAGE_COMMON.to_string()
}

#[derive(Debug, Default, PartialEq)]
pub struct Args {
    /// `--theme`: wins over the saved appearance for this run.
    pub theme: Option<ThemeId>,
    pub screen: Option<String>,
    pub selftest: Option<String>,
    /// `--check <name>`: one of the shell's checks, run in the signed-in app (a shell self-test
    /// starts the app this way in a child process).
    pub check: Option<String>,
    /// `--bench-scroll <seconds>` and its `--speed <px/s>` (the chat list's scroll benchmark).
    pub bench_scroll: Option<f64>,
    pub bench_speed: Option<f64>,
    /// `--login <account>` (feature `dev-local`).
    pub login: Option<String>,
    /// `decibell://…` arguments (the desktop's URL handler passes the link as one), unvalidated:
    /// `decibell_platform::Launch` checks them as Electron's main process did.
    pub links: Vec<String>,
    /// `--hotkey=<action>` / `--hotkey <action>`, unvalidated.
    pub hotkey: Option<String>,
    pub perf: bool,
    pub help: bool,
    /// `--version`.
    pub version: bool,
    /// `--update-trial=<pipe>` (hidden): started by the previous version to apply an update; tell it
    /// through the pipe once the first frame is on screen (`decibell_release::apply`).
    pub update_trial: Option<String>,
    /// `--apply-update` (hidden): Restart started this process to apply the downloaded update; it
    /// waits for the profile instead of handing over to the instance that is quitting.
    pub apply_update: bool,
    /// `--create-shortcut <path>` (hidden, Windows): the installer's Start menu shortcut, with the
    /// app's AppUserModelID (`decibell_release::win::create_shortcut`).
    pub create_shortcut: Option<String>,
    /// `--layout-labels` (development builds): print every writing-system key's label in the
    /// display's keyboard layout and exit (`keybind-labels-layout`).
    pub layout_labels: bool,
    /// Arguments this build doesn't know: ignored, as Chromium ignores switches it doesn't know
    /// (a launcher's `--no-sandbox`, an Electron-era desktop entry), each said once on stderr.
    pub ignored: Vec<String>,
}

impl Args {
    /// Parses the arguments after the program name. The error is a message for stderr.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
        let mut parsed = Args {
            perf: std::env::var_os("DECIBELL_PERF").is_some_and(|value| value != "0"),
            ..Args::default()
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .ok_or_else(|| format!("decibell: {name} needs a value\n\n{}", usage()))
            };
            match arg.as_str() {
                "--theme" => {
                    parsed.theme = Some(
                        value("--theme")?
                            .parse()
                            .map_err(|error| format!("decibell: {error}\n\n{}", usage()))?,
                    )
                }
                #[cfg(feature = "dev")]
                "--screen" => parsed.screen = Some(value("--screen")?),
                #[cfg(feature = "dev")]
                "--selftest" => parsed.selftest = Some(value("--selftest")?),
                #[cfg(feature = "dev")]
                "--check" => parsed.check = Some(value("--check")?),
                #[cfg(feature = "dev")]
                "--bench-scroll" | "--speed" => {
                    let number: f64 = value(&arg)?.parse().map_err(|_| format!("decibell: {arg} needs a number\n\n{}", usage()))?;
                    if arg == "--speed" { parsed.bench_speed = Some(number) } else { parsed.bench_scroll = Some(number) }
                }
                #[cfg(feature = "dev")]
                "--login" => parsed.login = Some(value("--login")?),
                "--hotkey" => parsed.hotkey = Some(value("--hotkey")?),
                hotkey if hotkey.starts_with("--hotkey=") => parsed.hotkey = Some(hotkey["--hotkey=".len()..].to_string()),
                link if decibell_platform::launch::is_link_argument(link) => parsed.links.push(link.to_string()),
                "--perf" => parsed.perf = true,
                // Read by the video-test screen itself (decibell_video::screens).
                #[cfg(feature = "dev")]
                "--dbv" => drop(value("--dbv")?),
                #[cfg(feature = "dev")]
                "--mirror" => {}
                #[cfg(feature = "dev")]
                "--layout-labels" => parsed.layout_labels = true,
                "-h" | "--help" => parsed.help = true,
                "--version" => parsed.version = true,
                trial if trial.starts_with("--update-trial=") => parsed.update_trial = Some(trial["--update-trial=".len()..].to_string()),
                "--apply-update" => parsed.apply_update = true,
                "--create-shortcut" => parsed.create_shortcut = Some(value("--create-shortcut")?),
                other => parsed.ignored.push(other.to_string()),
            }
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        Args::parse(args.iter().map(|arg| arg.to_string()))
    }

    #[cfg(feature = "dev")]
    #[test]
    fn parses_options() {
        let args = parse(&[
            "--theme",
            "console-split",
            "--selftest",
            "boot,themes",
            "--perf",
        ])
        .unwrap();
        assert_eq!(args.theme, Some(ThemeId::ConsoleSplit));
        assert_eq!(args.selftest.as_deref(), Some("boot,themes"));
        assert!(args.perf && !args.help && args.screen.is_none());
    }

    #[test]
    fn takes_links_and_hotkeys() {
        let args = parse(&["decibell://invite/ABCD234567", "--hotkey=push_to_talk:up"]).unwrap();
        assert_eq!(args.links, vec!["decibell://invite/ABCD234567".to_string()]);
        assert_eq!(args.hotkey.as_deref(), Some("push_to_talk:up"));
        assert_eq!(parse(&["--hotkey", "toggle_mute"]).unwrap().hotkey.as_deref(), Some("toggle_mute"));
        assert!(parse(&["--hotkey"]).unwrap_err().contains("needs a value"));
        // The app starts as usual from its URL handler, whatever the link.
        assert_eq!(parse(&["DECIBELL://x y"]).unwrap().links.len(), 1);
    }

    #[test]
    fn takes_the_release_arguments() {
        let args = parse(&["--version", "--update-trial=7", "--apply-update", "decibell://invite/ABCD234567"]).unwrap();
        assert!(args.version && args.apply_update);
        assert_eq!(args.update_trial.as_deref(), Some("7"));
        assert_eq!(args.links.len(), 1);
        assert_eq!(parse(&["--create-shortcut", "C:\\x.lnk"]).unwrap().create_shortcut.as_deref(), Some("C:\\x.lnk"));
    }

    /// The shipped build has no screens, self-tests, benchmark or development sign-in: their
    /// arguments are unknown there, and ignored like any other.
    #[cfg(not(feature = "dev"))]
    #[test]
    fn the_ship_build_ignores_development_arguments() {
        for arg in ["--screen", "--selftest", "--check", "--bench-scroll", "--login", "--dbv", "--mirror"] {
            let args = parse(&[arg, "x"]).unwrap();
            assert_eq!(args.ignored, vec![arg.to_string(), "x".to_string()], "{arg}");
            assert!(args.screen.is_none() && args.selftest.is_none() && args.login.is_none());
        }
        assert!(!usage().contains("--selftest"));
    }

    #[test]
    fn rejects_what_it_does_not_know() {
        assert!(parse(&["--theme"]).unwrap_err().contains("needs a value"));
        assert!(
            parse(&["--theme", "solarized"])
                .unwrap_err()
                .contains("unknown theme")
        );
        // Unknown switches and words are ignored (Chromium's rule), the rest still applies.
        let args = parse(&["--no-sandbox", "--enable-features=X", "stray", "--perf"]).unwrap();
        assert_eq!(args.ignored, ["--no-sandbox", "--enable-features=X", "stray"]);
        assert!(args.perf);
    }
}
