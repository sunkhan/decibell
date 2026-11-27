//! `keybind-labels-layout`: key labels come from the keyboard layout (GPUI patch 0040), as the
//! renderer's come from `navigator.keyboard.getLayoutMap()`.
//!
//! - **X11, in private Xvfb servers** this test starts (its own display numbers; stopped by their
//!   pids), one per layout: each reads a private copy of the system's XKB data whose default `us`
//!   symbols are German, French or Turkish (`Xvfb -xkbdir`; `setxkbmap` doesn't take on this Xvfb,
//!   and nothing of the session's is touched), and the app's `--layout-labels` child, on that display
//!   with Wayland off, prints what it would label each writing-system key with; the labels are
//!   checked against what those layouts type.
//! - **The session's own display** (Wayland here), read only: the child prints the labels the
//!   compositor's keymap gives; each letter key has a one-character label.
//! - **Windows** (`ToUnicodeEx`) is compiled only: `docs/windows-owner-checks.md`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx};

pub fn selftest() -> SelfTest {
    SelfTest {
        name: "keybind-labels-layout",
        about: "key labels follow the keyboard layout: German, French and Turkish layouts in a private Xvfb (the German Y key reads Z, dead keys their accent), and the session's compositor keymap read only",
        run: |cx| Box::pin(run(cx)),
    }
}

/// A private X server (`-displayfd`: it picks a free display), killed by its pid when dropped.
struct Xvfb {
    child: std::process::Child,
    display: String,
}

impl Xvfb {
    fn start(log: &Path, xkb: &Path) -> Result<Xvfb> {
        use std::io::BufRead as _;
        let mut child = Command::new("Xvfb")
            .arg("-xkbdir")
            .arg(xkb)
            .args(["-displayfd", "1", "-nolisten", "tcp", "-screen", "0", "640x480x24"])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(log)?))
            .spawn()
            .context("starting Xvfb")?;
        let stdout = child.stdout.take().context("Xvfb's stdout")?;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("decibell-test-xvfb".into()).spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        })?;
        let mut xvfb = Xvfb { child, display: String::new() };
        let line = rx.recv_timeout(Duration::from_secs(15)).context("Xvfb printed no display within 15 s")?;
        let number: u32 = line.trim().parse().with_context(|| format!("Xvfb printed {line:?}"))?;
        xvfb.display = format!(":{number}");
        Ok(xvfb)
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A copy of the system's XKB data (symbolic links followed), for a server of our own.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if std::fs::metadata(&source)?.is_dir() {
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// The copy's default symbols (`us`, what the server loads) made `layout`'s.
fn default_layout(xkb: &Path, original_us: &str, layout: &str) -> Result<()> {
    let text = if layout == "us" {
        original_us.to_string()
    } else {
        format!("default partial alphanumeric_keys modifier_keys\nxkb_symbols \"basic\" {{\n    include \"{layout}(basic)\"\n}};\n")
    };
    std::fs::write(xkb.join("symbols/us"), text)?;
    Ok(())
}

/// The child's labels: `(layout name, [(code, label)])`.
fn labels(env: &[(&str, &str)], remove: &[&str]) -> Result<(String, Vec<(String, String)>)> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.arg("--layout-labels").stdout(Stdio::piped()).stderr(Stdio::null());
    for key in remove {
        command.env_remove(key);
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let started = Instant::now();
    let output = command.output().context("running the --layout-labels child")?;
    ensure!(started.elapsed() < Duration::from_secs(20), "the child took {:?}", started.elapsed());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout.lines().find_map(|l| l.strip_prefix("decibell: layout ")) else {
        bail!("the child printed no labels ({}): {stdout}", output.status);
    };
    let Some((name, rest)) = line.split_once(" | ") else { bail!("the child printed {line:?}") };
    let pairs = rest
        .split(' ')
        .filter_map(|p| p.split_once('='))
        .map(|(c, l)| (c.to_string(), l.to_string()))
        .collect();
    Ok((name.to_string(), pairs))
}

fn label_of<'a>(pairs: &'a [(String, String)], code: &str) -> &'a str {
    pairs.iter().find(|(c, _)| c == code).map_or("", |(_, l)| l.as_str())
}

async fn run(cx: TestCx) -> Result<()> {
    if !cfg!(target_os = "linux") {
        cx.log("SKIP: the X11 / Wayland layouts are Linux's (Windows: docs/windows-owner-checks.md)");
        return Ok(());
    }
    if !Command::new("which").arg("Xvfb").stdout(Stdio::null()).status().is_ok_and(|s| s.success()) {
        cx.log("SKIP: no Xvfb (the private X server the layouts are set in)");
        return Ok(());
    }
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    let dir = cache.join(format!("decibell-dev/selftest-tmp/layout-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let _cleanup = Cleanup(dir.clone());
    // (layout, [(code, label it must have)]): what xkeyboard-config's layouts type there.
    let cases: [(&str, &[(&str, &str)]); 4] = [
        ("us", &[("KeyY", "Y"), ("KeyZ", "Z"), ("Minus", "-"), ("Quote", "'"), ("Digit1", "1")]),
        (
            "de",
            &[
                ("KeyY", "Z"),
                ("KeyZ", "Y"),
                ("Semicolon", "Ö"),
                ("Quote", "Ä"),
                ("BracketLeft", "Ü"),
                // ß upper-cases to SS, as JavaScript's toUpperCase does.
                ("Minus", "SS"),
                // Dead keys: their accent.
                ("Equal", "\u{b4}"),
                ("Backquote", "^"),
                ("Digit1", "1"),
            ],
        ),
        ("fr", &[("KeyQ", "A"), ("KeyA", "Q"), ("KeyW", "Z"), ("KeyZ", "W"), ("Digit1", "&"), ("Digit2", "É"), ("Semicolon", "M")]),
        ("tr", &[("KeyI", "I"), ("Quote", "I"), ("Semicolon", "Ş"), ("BracketLeft", "Ğ"), ("Comma", "Ö"), ("Period", "Ç")]),
    ];
    let xkb = dir.join("xkb");
    copy_tree(Path::new("/usr/share/X11/xkb"), &xkb).context("copying /usr/share/X11/xkb")?;
    let original_us = std::fs::read_to_string(xkb.join("symbols/us"))?;
    for (layout, expected) in cases {
        default_layout(&xkb, &original_us, layout)?;
        let xvfb = Xvfb::start(&dir.join(format!("xvfb-{layout}.log")), &xkb)?;
        let (name, pairs) = labels(&[("DISPLAY", xvfb.display.as_str()), ("WAYLAND_DISPLAY", "")], &[])?;
        drop(xvfb);
        let wrong: Vec<String> = expected
            .iter()
            .filter(|(code, want)| label_of(&pairs, code) != *want)
            .map(|(code, want)| format!("{code}: {:?}, expected {want:?}", label_of(&pairs, code)))
            .collect();
        let shown: Vec<String> = expected.iter().map(|(code, _)| format!("{code}={}", label_of(&pairs, code))).collect();
        cx.log(format_args!("X11 {layout} ({name}): {}", shown.join(" ")));
        ensure!(wrong.is_empty(), "X11 {layout}: {}", wrong.join("; "));
    }
    // The session's own display, read only (its layout is the owner's: whatever it is, every
    // letter key gets a one-character label from the compositor's keymap).
    let (name, pairs) = labels(&[], &[])?;
    let letters: Vec<&(String, String)> = pairs.iter().filter(|(c, _)| c.starts_with("Key")).collect();
    cx.log(format_args!(
        "the session's display ({}): {name}: {}",
        std::env::var("WAYLAND_DISPLAY").map_or("X11".into(), |w| format!("Wayland {w}")),
        letters.iter().map(|(c, l)| format!("{c}={l}")).collect::<Vec<_>>().join(" ")
    ));
    ensure!(letters.len() == 26, "the session's keymap labelled {} of 26 letter keys", letters.len());
    ensure!(letters.iter().all(|(_, l)| l.chars().count() == 1), "a letter key without a one-character label");
    Ok(())
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
