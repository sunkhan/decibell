//! `content-copy-image`: "Copy image" (`decibell_chat_content::actions::copy_image`) puts the picture
//! on the system clipboard as PNG, on Wayland and on X11 (XWayland), and another process reads the
//! same pixels back: `wl-paste` on Wayland (and a `wl-paste --watch` reading as soon as the selection
//! changes, as a clipboard manager does), `tools/x11_clipboard.py` (python-xlib, INCR included) on X11;
//! both after the copying window lost the focus. The pictures are the content fixture's: a 1600×900
//! PNG, an animated GIF (its first frame becomes the PNG), and a link picture of noise (2.8 MB as
//! PNG: served in `INCR` pieces on X11, in many writes on Wayland).
//!
//! **The owner's clipboard is real**: what it holds is read before the first copy and put back after
//! the last, whatever happened (text through Klipper when it runs, so no process is left serving it;
//! else `wl-copy`), and read back to check.
//!
//! The parent runs in any window; each copy happens in a child (`--screen content-attachments
//! --selftest content-copy-image-child`, its window drawing the fixture), one per session.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_chat_content::actions::{Target, copy_image};
use decibell_chat_content::pictures::Source;
use decibell_harness::{SelfTest, TestCx};
use decibell_model::ServerId;
use gpui::ClipboardEntry;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "content-copy-image",
            about: "Copy image → the Wayland and X11 clipboards as PNG; wl-paste / an X client read the same pixels; the owner's clipboard is put back",
            run: |cx| Box::pin(parent(cx)),
        },
        SelfTest {
            name: "content-copy-image-child",
            about: "(run by content-copy-image, on --screen content-attachments) copy the fixture's pictures",
            run: |cx| Box::pin(child(cx)),
        },
    ]
}

/// The fixture's pictures copied: attachment id, name, mime; id 0 is a link picture of noise
/// (`NOISE_URL`), large enough for X11's `INCR` transfer (over 1 MiB as PNG).
const PICTURES: [(i64, &str, &str); 3] = [(10, "sunset.jpg", "image/jpeg"), (60, "loop.gif", "image/gif"), (0, "noise.png", "image/png")];
const NOISE_URL: &str = "https://pictures.example.invalid/noise.png";

/// 1000 × 700 px of noise as PNG (about 2.8 MB): what a screenshot of real content weighs.
fn noise_png() -> Vec<u8> {
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    let img = image::RgbaImage::from_fn(1000, 700, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let b = state.to_le_bytes();
        image::Rgba([b[0], b[1], b[2], 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).expect("PNG");
    out
}

fn run(tool: &str, args: &[&str], input: Option<&[u8]>) -> Result<(bool, Vec<u8>)> {
    let mut command = Command::new(tool);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = command.spawn().with_context(|| format!("starting {tool}"))?;
    if let Some(input) = input {
        use std::io::Write as _;
        child.stdin.take().context("stdin")?.write_all(input)?;
    }
    let out = child.wait_with_output()?;
    Ok((out.status.success(), out.stdout))
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok()
}

/// What the owner's clipboard held: a type and its bytes, or nothing.
struct Kept(Option<(String, Vec<u8>)>);

impl Kept {
    fn read() -> Result<Kept> {
        let (_, types) = run("wl-paste", &["--list-types"], None)?;
        let types: Vec<String> = String::from_utf8_lossy(&types).lines().map(str::to_string).collect();
        let pick = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "image/png"]
            .iter()
            .find(|t| types.iter().any(|x| x == *t))
            .map(|t| t.to_string())
            .or_else(|| types.first().cloned());
        Ok(Kept(match pick {
            Some(kind) => Some((kind.clone(), run("wl-paste", &["--no-newline", "--type", &kind], None)?.1)),
            None => None,
        }))
    }

    fn describe(&self) -> String {
        match &self.0 {
            Some((kind, bytes)) => format!("{kind}, {} bytes", bytes.len()),
            None => "empty".into(),
        }
    }

    /// Puts it back and reads it again; the answer says how.
    fn restore(&self) -> Result<String> {
        let how = match &self.0 {
            Some((kind, bytes)) if kind.starts_with("text/") || kind == "UTF8_STRING" => {
                let text = String::from_utf8_lossy(bytes).to_string();
                // Klipper serves what it sets: no process stays behind for it.
                let klipper = have("qdbus6")
                    && run("qdbus6", &["org.kde.klipper", "/klipper", "org.kde.klipper.klipper.setClipboardContents", &text], None)
                        .is_ok_and(|(ok, _)| ok);
                if !klipper {
                    run("wl-copy", &["--type", kind], Some(bytes))?;
                }
                if klipper { "Klipper" } else { "wl-copy" }
            }
            Some((kind, bytes)) => {
                run("wl-copy", &["--type", kind], Some(bytes))?;
                "wl-copy"
            }
            None => {
                run("wl-copy", &["--clear"], None)?;
                "wl-copy --clear"
            }
        };
        // Read back (the new owner answers within a moment).
        let started = Instant::now();
        loop {
            let now = Kept::read()?;
            let same = match (&self.0, &now.0) {
                (Some((_, a)), Some((_, b))) => a == b,
                (None, None) => true,
                (None, Some((_, b))) => b.is_empty(),
                _ => false,
            };
            if same {
                return Ok(how.to_string());
            }
            if started.elapsed() > Duration::from_secs(3) {
                bail!("the clipboard reads {} after putting back {}", now.describe(), self.describe());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn pixels(png_or_any: &[u8]) -> Result<image::RgbaImage> {
    Ok(image::load_from_memory(png_or_any)?.to_rgba8())
}

/// Writes `x11_clipboard.py` next to the test's files.
fn x11_reader(dir: &Path) -> Result<PathBuf> {
    let path = dir.join("x11_clipboard.py");
    std::fs::write(&path, include_str!("../../decibell-platform/tools/x11_clipboard.py"))?;
    Ok(path)
}

async fn parent(cx: TestCx) -> Result<()> {
    if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
        cx.log("SKIP: the Linux clipboards");
        return Ok(());
    }
    let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| !d.is_empty());
    if !wayland || !have("wl-paste") || !have("wl-copy") {
        cx.log("SKIP: needs a Wayland session with wl-paste / wl-copy (the owner's clipboard is read and put back through them)");
        return Ok(());
    }
    let dir = std::env::temp_dir().join(format!("decibell-copy-image-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let kept = Kept::read()?;
    cx.log(format_args!("the clipboard held: {}", kept.describe()));
    let result = sessions(&cx, &dir).await;
    let restored = kept.restore();
    let _ = std::fs::remove_dir_all(&dir);
    result?;
    cx.log(format_args!("the clipboard is as it was ({}, through {})", kept.describe(), restored?));
    Ok(())
}

async fn sessions(cx: &TestCx, dir: &Path) -> Result<()> {
    let x11 = std::env::var("DISPLAY").is_ok_and(|d| !d.is_empty())
        && Command::new("python3").args(["-I", "-c", "import Xlib"]).status().is_ok_and(|s| s.success());
    let mut sessions = vec!["wayland"];
    if x11 {
        sessions.push("x11");
    } else {
        cx.log("no X11 reader (DISPLAY and python-xlib): the X11 half is not run");
    }
    let reader = x11_reader(dir)?;
    let winctl = std::env::current_exe()?.ancestors().map(|d| d.join("tools/perf/winctl.sh")).find(|p| p.exists());
    for session in sessions {
        let files = dir.join(session);
        std::fs::create_dir_all(&files)?;
        // A clipboard manager's view (Wayland): it reads the picture the moment the selection changes.
        let watched = files.join("watched.png");
        let mut watcher = (session == "wayland").then(|| {
            Command::new("wl-paste")
                .args(["--watch", "sh", "-c", r#"wl-paste --list-types | grep -qx image/png && wl-paste --no-newline --type image/png > "$0.part" && mv "$0.part" "$0""#])
                .arg(&watched)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        });
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--screen", "content-attachments", "--selftest", "content-copy-image-child"])
            .env("DECIBELL_COPY_TEST_DIR", &files)
            .env_remove("DECIBELL_PERF")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(files.join("child.stderr"))?));
        if session == "x11" {
            command.env("WAYLAND_DISPLAY", "");
        }
        let mut child = command.spawn().context("starting the child")?;
        let stdout = child.stdout.take().context("piped")?;
        let (tx, lines) = std::sync::mpsc::channel::<String>();
        std::thread::Builder::new().name("copy-image-child".into()).spawn(move || {
            use std::io::BufRead as _;
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
        let outcome = async {
            for (id, name, _) in PICTURES {
                let wanted = format!("copy-image: copied {id}");
                let started = Instant::now();
                loop {
                    if let Ok(line) = lines.try_recv() {
                        if line == wanted {
                            break;
                        }
                        if line.contains("FAIL") {
                            let stderr = std::fs::read_to_string(files.join("child.stderr")).unwrap_or_default();
                            let tail: Vec<&str> = stderr.lines().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect();
                            bail!("{session}: the child: {line}\n{}", tail.join("\n"));
                        }
                        continue;
                    }
                    ensure!(started.elapsed() < Duration::from_secs(60), "{session}: the child never copied {name}");
                    ensure!(child.try_wait()?.is_none(), "{session}: the child ended before copying {name}");
                    cx.sleep(Duration::from_millis(50)).await;
                }
                // Another window takes the focus: the selection must outlive it.
                if let Some(winctl) = &winctl {
                    // To the desktop of its output.
                    let _ = Command::new(winctl)
                        .args(["unfocus", &child.id().to_string()])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    cx.sleep(Duration::from_millis(300)).await;
                }
                let expected = pixels(&std::fs::read(files.join(format!("original-{id}")))?)?;
                let (types, bytes) = match session {
                    "wayland" => {
                        let types = String::from_utf8_lossy(&run("wl-paste", &["--list-types"], None)?.1).to_string();
                        (types, run("wl-paste", &["--no-newline", "--type", "image/png"], None)?.1)
                    }
                    _ => {
                        let reader = reader.display().to_string();
                        let (_, targets) = run("python3", &["-I", &reader, "targets"], None)?;
                        let (ok, bytes) = run("python3", &["-I", &reader, "image/png"], None)?;
                        ensure!(ok, "x11: the owner refused image/png");
                        (String::from_utf8_lossy(&targets).to_string(), bytes)
                    }
                };
                let offered: Vec<&str> = types.lines().collect();
                ensure!(offered.contains(&"image/png"), "{session}: {name}: offered {offered:?}");
                ensure!(!offered.iter().any(|t| t.starts_with("text/")), "{session}: {name}: a picture offered as text: {offered:?}");
                let got = pixels(&bytes).with_context(|| format!("{session}: {name}: {} bytes that aren't a picture", bytes.len()))?;
                ensure!(got.dimensions() == expected.dimensions(), "{session}: {name}: {:?}, expected {:?}", got.dimensions(), expected.dimensions());
                ensure!(got == expected, "{session}: {name}: the pixels differ");
                let mut note = String::new();
                if session == "wayland" {
                    let started = Instant::now();
                    while !watched.exists() && started.elapsed() < Duration::from_secs(3) {
                        cx.sleep(Duration::from_millis(50)).await;
                    }
                    let watched_pixels = pixels(&std::fs::read(&watched).context("the watching reader got nothing")?)?;
                    ensure!(watched_pixels == expected, "wayland: {name}: the watching reader got other pixels");
                    std::fs::remove_file(&watched)?;
                    note = ", and the reader watching the selection got the same at once".into();
                }
                cx.log(format_args!(
                    "{session}: {name} → {} bytes of image/png (offered: {}), {}×{} px identical{note}",
                    bytes.len(),
                    offered.join(" "),
                    got.width(),
                    got.height()
                ));
                std::fs::write(files.join(format!("read-{id}")), b"")?;
            }
            let started = Instant::now();
            loop {
                if let Some(status) = child.try_wait()? {
                    ensure!(status.success(), "{session}: the child failed ({status})");
                    break;
                }
                ensure!(started.elapsed() < Duration::from_secs(30), "{session}: the child didn't end");
                cx.sleep(Duration::from_millis(50)).await;
            }
            Ok(())
        }
        .await;
        // Our own processes, by pid.
        if child.try_wait()?.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(Ok(watcher)) = watcher.as_mut() {
            let _ = watcher.kill();
            let _ = watcher.wait();
        }
        outcome?;
    }
    Ok(())
}

async fn child(cx: TestCx) -> Result<()> {
    let Some(files) = std::env::var_os("DECIBELL_COPY_TEST_DIR").map(PathBuf::from) else {
        cx.log("SKIP: run by content-copy-image");
        return Ok(());
    };
    let server = ServerId::from_central(decibell_shell::fixture::DAILY_GAMING);
    for (id, name, mime) in PICTURES {
        // What the fixture serves for the original (the pipeline's own fetcher).
        let original = cx.update(|_, cx| {
            let fetcher = decibell_chat_content::fixture::fetcher(cx);
            let source = if id == 0 {
                let source = Source::Remote(NOISE_URL.into());
                let g = decibell_chat_content::fixture::Generated { w: 1000, h: 700, kind: decibell_chat_content::fixture::Kind::Still, seed: 99 };
                fetcher.remote.borrow_mut().insert(NOISE_URL.to_string(), g);
                fetcher.served.borrow_mut().insert(source.clone(), noise_png().into());
                source
            } else {
                Source::Attachment { server: server.wire().into(), id, thumb: 0 }
            };
            fetcher.answer(&source)
        })?;
        let original = original.context("held")?.map_err(|e| anyhow::anyhow!("{e}"))?;
        std::fs::write(files.join(format!("original-{id}")), &original)?;
        // A copy is a click in our window: it has the keyboard focus then (Wayland gives the
        // selection only to the focused client). KWin may not focus a window started from a shell.
        if !cx.update(|window, _| window.is_window_active())? {
            if let Some(winctl) = std::env::current_exe()?.ancestors().map(|d| d.join("tools/perf/winctl.sh")).find(|p| p.exists()) {
                let _ = Command::new(winctl).args(["activate", &std::process::id().to_string()]).stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
            let started = Instant::now();
            while !cx.update(|window, _| window.is_window_active())? {
                ensure!(started.elapsed() < Duration::from_secs(5), "{name}: the window never got the focus");
                cx.sleep(Duration::from_millis(50)).await;
            }
            cx.log(format_args!("{name}: the window took the focus to copy"));
        }
        let target = if id == 0 {
            Target { server: None, attachment_id: None, filename: name.into(), mime: Some(mime.into()), url: Some(NOISE_URL.into()), page_url: None }
        } else {
            Target { server: Some(server), attachment_id: Some(id), filename: name.into(), mime: Some(mime.into()), url: None, page_url: None }
        };
        cx.update(|_, cx| copy_image(target, cx))?;
        let started = Instant::now();
        loop {
            let ours = cx.update(|_, cx| {
                cx.read_from_clipboard()
                    .is_some_and(|item| item.entries().iter().any(|e| matches!(e, ClipboardEntry::Image(i) if i.format == gpui::ImageFormat::Png)))
            })?;
            if ours {
                break;
            }
            if started.elapsed() > Duration::from_secs(20) {
                let toasts = cx.update(|_, cx| {
                    decibell_model::Model::global(cx)
                        .toasts
                        .read(cx)
                        .toasts()
                        .iter()
                        .map(|t| format!("{}: {}", t.title, t.body.clone().unwrap_or_default()))
                        .collect::<Vec<_>>()
                })?;
                bail!("{name}: the clipboard never held the picture (toasts: {toasts:?})");
            }
            cx.sleep(Duration::from_millis(100)).await;
        }
        println!("copy-image: copied {id}");
        let read = files.join(format!("read-{id}"));
        let started = Instant::now();
        while !read.exists() {
            ensure!(started.elapsed() < Duration::from_secs(60), "{name}: the parent never read it");
            cx.sleep(Duration::from_millis(50)).await;
        }
        cx.log(format_args!("{name}: copied and read"));
    }
    Ok(())
}
