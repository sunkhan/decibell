use decibell_theme::ThemeId;

pub const USAGE: &str = "\
usage: decibell [options]

  --theme <id>          nocturne (default), matinee, graphite, graphite-light, console,
                        console-light, console-split
  --screen <name>       show a registered screen instead of the app (--screen list)
  --login <account>     development builds (feature dev-local) only: sign in to the local test
                        stack as <account> (password from DECIBELL_DEV_PASSWORD; refused unless
                        DECIBELL_CENTRAL is a loopback address)
  --selftest <names>    run self-tests in the real window and exit: all, one name, or
                        names separated by commas (--selftest list)
  --bench-scroll <s>    scroll the chat fixture channel into history for <s> seconds per speed
                        (--speed <px/s>, default 1000 then 3000) and print the frame figures
                        as JSON (decibell_chat::bench)
  --perf                print the first-frame time and frame counters on stderr
                        (also DECIBELL_PERF=1)
  --dbv <a.dbv[,b.dbv]> the test streams `--screen video-test` plays; --mirror shows the
                        first one a second time in a mini-player
  -h, --help

  DECIBELL_LOG=<level>  log records on stderr: off, error (default), warn, info, debug, trace

While the window is open, F8 / Shift-F8 switch to the next / previous theme.";

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
    pub perf: bool,
    pub help: bool,
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
                    .ok_or_else(|| format!("decibell: {name} needs a value\n\n{USAGE}"))
            };
            match arg.as_str() {
                "--theme" => {
                    parsed.theme = Some(
                        value("--theme")?
                            .parse()
                            .map_err(|error| format!("decibell: {error}\n\n{USAGE}"))?,
                    )
                }
                "--screen" => parsed.screen = Some(value("--screen")?),
                "--selftest" => parsed.selftest = Some(value("--selftest")?),
                "--check" => parsed.check = Some(value("--check")?),
                "--bench-scroll" | "--speed" => {
                    let number: f64 = value(&arg)?.parse().map_err(|_| format!("decibell: {arg} needs a number\n\n{USAGE}"))?;
                    if arg == "--speed" { parsed.bench_speed = Some(number) } else { parsed.bench_scroll = Some(number) }
                }
                "--login" => parsed.login = Some(value("--login")?),
                "--perf" => parsed.perf = true,
                // Read by the video-test screen itself (decibell_video::screens).
                "--dbv" => drop(value("--dbv")?),
                "--mirror" => {}
                "-h" | "--help" => parsed.help = true,
                other => return Err(format!("decibell: unknown argument {other:?}\n\n{USAGE}")),
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
    fn rejects_what_it_does_not_know() {
        assert!(parse(&["--theme"]).unwrap_err().contains("needs a value"));
        assert!(
            parse(&["--theme", "solarized"])
                .unwrap_err()
                .contains("unknown theme")
        );
        assert!(
            parse(&["--frobnicate"])
                .unwrap_err()
                .contains("unknown argument")
        );
    }
}
