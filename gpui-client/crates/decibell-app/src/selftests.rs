//! The app's own self-tests: that it boots with its fonts, that it stops drawing, that a
//! theme switch costs one frame.

use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx, perf};
use decibell_theme::ThemeId;
use decibell_ui::{ActiveTheme as _, Theme, fonts};
use gpui::{Decorations, Font, FontFeatures, FontStyle, FontWeight, px};

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "boot",
            about: "the window opens, a first frame is presented, every bundled face resolves",
            run: |cx| Box::pin(boot(cx)),
        },
        SelfTest {
            name: "idle-frames",
            about: "nothing is drawn or presented, and the main thread sleeps, while idle",
            run: |cx| Box::pin(idle_frames(cx)),
        },
        SelfTest {
            name: "window-frame",
            about: "maximising drops the client-side margin and restoring brings it back",
            run: |cx| Box::pin(window_frame(cx)),
        },
        SelfTest {
            name: "themes",
            about: "each of the seven themes repaints in exactly one frame, then idles",
            run: |cx| Box::pin(themes(cx)),
        },
        crate::software_codecs::selftest(),
        crate::hotkeys::selftest(),
        SelfTest {
            name: "regional-read-crates",
            about: "(the regional self-test's child) the labels other crates draw in this process's regional formats: the audit log's day heading, the Transfers panel's time, the viewer's posted line, the invite card's member count, a search result's time",
            run: |cx| Box::pin(regional_read_crates(cx)),
        },
        SelfTest {
            name: "render-cost",
            about: "what one change costs in views rendered and main-thread time, with nested cached views replayed (GPUI patch 0014) and as GPUI was",
            run: |cx| Box::pin(crate::render_cost::run(cx)),
        },
    ]
    .into_iter()
    .chain(crate::joins::selftests())
    .collect()
}

async fn boot(cx: TestCx) -> Result<()> {
    let mut waited = Duration::ZERO;
    let first_frame = loop {
        if let Some(first_frame) = perf::first_frame() {
            break first_frame;
        }
        ensure!(
            waited < Duration::from_secs(10),
            "no frame was presented within 10 s"
        );
        cx.sleep(Duration::from_millis(20)).await;
        waited += Duration::from_millis(20);
    };
    cx.log(format_args!(
        "first frame presented {:.1} ms after start",
        first_frame.as_secs_f64() * 1e3
    ));

    let (size, scale, decorations) = cx.update(|window, _| {
        (
            window.viewport_size(),
            window.scale_factor(),
            window.window_decorations(),
        )
    })?;
    cx.log(format_args!(
        "window {}×{} at scale {scale}, {decorations:?}",
        f32::from(size.width),
        f32::from(size.height)
    ));
    ensure!(
        size.width >= px(800.) && size.height >= px(600.),
        "window smaller than its minimum"
    );

    // Every (family, weight) we ship must come back as exactly that face, from the platform's
    // text system with the system's fonts loaded beside ours. The advance of `m` at a font
    // size of one em in font units is the face's own hmtx value; it differs between all faces
    // of a proportional family. The monospace weights share it and can only be told to be
    // four different faces here (the unit tests of decibell-ui compare their outlines).
    let text_system = cx.app().update(|cx| cx.text_system().clone());
    let mut resolved = Vec::new();
    for face in fonts::FACES.iter() {
        let font = Font {
            family: face.family.into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight(face.weight as f32),
            style: FontStyle::Normal,
        };
        let id = text_system.resolve_font(&font);
        let advance = text_system
            .advance(id, px(face.units_per_em as f32), 'm')?
            .width;
        ensure!(
            (f32::from(advance) - face.advance_m as f32).abs() < 0.5,
            "{} {} resolved to another face: 'm' advances {} units, the bundled face {}",
            face.family,
            face.weight,
            f32::from(advance),
            face.advance_m
        );
        ensure!(
            !resolved.contains(&id),
            "{} {} shares a face with another weight",
            face.family,
            face.weight
        );
        resolved.push(id);
    }
    let by_metrics = fonts::FACES
        .iter()
        .filter(|face| face.advance_identifies())
        .count();
    cx.log(format_args!(
        "{} bundled faces resolve: {by_metrics} identified by their metrics, {} as distinct faces",
        resolved.len(),
        resolved.len() - by_metrics
    ));
    Ok(())
}

async fn idle_frames(cx: TestCx) -> Result<()> {
    let seconds = std::env::var("DECIBELL_SELFTEST_IDLE_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    // The pointer crossing the window (hover styles) or the compositor reconfiguring it
    // draws frames that are not idle frames; a real leak shows up in every attempt.
    let mut last = String::new();
    for attempt in 1..=3 {
        let before = cx.settle().await?;
        let wakeups_before = perf::thread_wakeups();
        cx.sleep(Duration::from_secs(seconds)).await;
        let wakeups = perf::thread_wakeups()
            .zip(wakeups_before)
            .map(|(after, before)| after - before);
        let frames = cx.frames()?.since(before);

        last = format!(
            "{} drawn, {} presented, {} main-thread wakeups in {seconds} s",
            frames.drawn,
            frames.presented,
            wakeups.map_or("?".into(), |count| count.to_string())
        );
        cx.log(format_args!("attempt {attempt}: {last}"));
        // The timer ending the wait is one wakeup; allow two a second for the session
        // (D-Bus signals, the compositor's pings).
        let quiet = wakeups.is_none_or(|count| count <= 1 + 2 * seconds);
        if frames.drawn == 0 && frames.presented == 0 && quiet {
            return Ok(());
        }
    }
    bail!("the window is not idle: {last}")
}

async fn window_frame(cx: TestCx) -> Result<()> {
    let state = |cx: &TestCx| {
        cx.update(|window, _| {
            (
                window.window_decorations(),
                window.is_maximized(),
                window.viewport_size(),
            )
        })
    };
    let (decorations, maximized, floating_size) = state(&cx)?;
    let Decorations::Client { tiling } = decorations else {
        cx.log("server-side decorations: the window has no frame of ours to check");
        return Ok(());
    };
    ensure!(
        !maximized && !tiling.is_tiled(),
        "the window did not start floating ({decorations:?})"
    );

    cx.update(|window, _| window.zoom_window())?;
    cx.sleep(Duration::from_millis(300)).await;
    cx.settle().await?;
    let (decorations, maximized, maximized_size) = state(&cx)?;
    cx.log(format_args!(
        "maximised: {}×{}, {decorations:?}",
        f32::from(maximized_size.width),
        f32::from(maximized_size.height)
    ));
    // Restore before judging, so a failure does not leave the window maximised.
    cx.update(|window, _| window.zoom_window())?;
    cx.sleep(Duration::from_millis(300)).await;
    let restored = cx.settle().await;
    ensure!(maximized, "the window did not maximise");
    ensure!(
        matches!(decorations, Decorations::Client { tiling } if tiling.top && tiling.bottom && tiling.left && tiling.right),
        "a maximised window still has floating edges: {decorations:?}"
    );
    restored?;
    let (_, maximized, size) = state(&cx)?;
    ensure!(
        !maximized && size == floating_size,
        "the window did not return to its floating size"
    );
    Ok(())
}

async fn themes(cx: TestCx) -> Result<()> {
    let original = cx.update(|_, cx| cx.theme().id)?;
    let start = original.index();
    // Every other theme in turn, ending back on the one we started with.
    for step in 1..=ThemeId::ALL.len() {
        let id = ThemeId::ALL[(start + step) % ThemeId::ALL.len()];
        let before = cx.settle().await?;
        cx.update(|_, cx| Theme::set(cx, id))?;
        let after = cx.settle().await?;
        let frames = after.since(before);
        ensure!(
            cx.update(|_, cx| cx.theme().id)? == id,
            "the theme did not change to {id}"
        );
        ensure!(
            frames.drawn == 1 && frames.presented == 1,
            "switching to {id} drew {} and presented {} frames, expected 1 and 1",
            frames.drawn,
            frames.presented
        );
    }
    cx.log("7 switches, one frame each");

    // The keyboard path: F8 is bound to the next theme.
    let before = cx.settle().await?;
    ensure!(cx.keystroke("f8")?, "F8 was not handled");
    let frames = cx.settle().await?.since(before);
    let now = cx.update(|_, cx| cx.theme().id)?;
    ensure!(
        now.index() == (start + 1) % ThemeId::ALL.len(),
        "F8 did not step the theme (now {now})"
    );
    // The first key after pointer input also makes GPUI redraw once for the change of input
    // modality (focus-visible styling), before the key is dispatched.
    ensure!(
        frames.presented == 1 && frames.drawn <= 2,
        "F8 drew {} and presented {} frames, expected 1 presented",
        frames.drawn,
        frames.presented
    );
    cx.update(|_, cx| Theme::set(cx, original))?;
    cx.settle().await?;
    cx.log("F8 steps the theme in one frame");
    Ok(())
}

/// The child half of `decibell-chat`'s `regional` self-test for the crates it can't reach: one label
/// from each crate that draws a date, a time of day or a count, as that crate composes it, for a
/// moment 40 days ago (so every one takes its dated form), printed as `regional-read: <key> <secs>
/// <label>` for the parent to compare with the case's formats.
async fn regional_read_crates(cx: TestCx) -> Result<()> {
    use decibell_model::regional::{Regional, local};
    use decibell_model::stores::settings::TimeFormat;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
    let then = now - 40 * 86_400;
    let (at, today) = (local(then).context("a local time")?, local(now).context("today")?);
    cx.log(format_args!("regional formats: {}", Regional::get().origin()));
    let mut labels = vec![("audit", decibell_server_settings::server::audit::day_heading(at.date, today.date).0)];
    labels.extend(decibell_chat_content::regional_labels(then, now, 1234, TimeFormat::Auto));
    let today_day = cx.update(|_, cx| decibell_search::format::today(cx))?;
    labels.push(("search", decibell_search::format::result_stamp(then, today_day, TimeFormat::Auto)));
    for (key, label) in labels {
        println!("regional-read: {key} {then} {label}");
        cx.log(format_args!("{key}: {label:?}"));
    }
    Ok(())
}
