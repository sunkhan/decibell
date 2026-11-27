//! The administration tests that need more than one session or a throwaway target, against the
//! local stack (the conventions of `selftests.rs`: a child app per session, its own profile, the
//! throwaway account `stsettings` as every target, everything else named `st-<pid>…` and removed
//! again; a cleanup child runs whatever happened).
//!
//! - `roles-reorder` (Design Lab, carol): Alt+↑ / Alt+↓ on the focused hierarchy row, a drag, and a
//!   role's holders through the picker and the row's ✕.
//! - `moderation-live` (Design Lab, carol): a time-out and End timeout; the throwaway posts; a ban
//!   with an expiry and a one-hour purge (the post goes), listed with its expiry, lifted from Bans;
//!   the throwaway joins again and is kicked.
//! - `transfer-ownership-live` (Rustaceans, bob): bob gives the community to the throwaway through
//!   the Overview's picker and typed confirmation; the throwaway gives it back the same way. Bob is
//!   the owner again before the test ends, whatever failed (the give-back runs regardless, and the
//!   last step fails loudly if he isn't).
//! - `server-picture-live` (Design Lab, carol): a picture uploaded through the editor, a second
//!   session (the throwaway's) sees the new version arrive and then the removal.
//! - `membership-added-live` (Design Lab): the throwaway signed in on one device; a second device of
//!   it joins by invite; the first shows the community's tile and connects (`MEMBERSHIP_ADDED`).
//! - `server-settings-guard` (Daily Gaming, alice; nothing is sent): an unsaved Overview edit holds
//!   Server Settings on Esc, ✕, the backdrop and a tab switch (each shakes the save bar).

use super::*;

use decibell_model::stores::community::Role;
use decibell_picture_editor::picture::sample_rgba;
use gpui::{Modifiers, Pixels, Point, point};

/// Bob's community (unlisted, lightly used): the ownership test's.
const RUSTACEANS: i32 = 2;

pub(super) fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "roles-reorder",
            about: "(stack) Design Lab, carol: Alt+↑ / Alt+↓ on the focused role row moves it one place on the echo and the focus follows it; \
                    a drag moves it; a custom colour typed and picked in the colour picker, saved; the throwaway added to a role through Add \
                    members and removed with the row's ✕",
            run: |cx| Box::pin(with_throwaway(cx, "roles-reorder", DESIGN_LAB)),
        },
        SelfTest {
            name: "moderation-live",
            about: "(stack) Design Lab, carol on the throwaway: time out, End timeout; a ban for an hour that deletes its last hour of \
                    messages (its post goes), listed with the expiry, lifted; joins again; kicked",
            run: |cx| Box::pin(parent_moderation(cx)),
        },
        SelfTest {
            name: "transfer-ownership-live",
            about: "(stack) Rustaceans: bob transfers ownership to the throwaway through the picker and typed confirmation; it \
                    transfers back the same way; bob is the owner again",
            run: |cx| Box::pin(parent_transfer(cx)),
        },
        SelfTest {
            name: "server-picture-live",
            about: "(stack) Design Lab: carol uploads a server picture through the editor, the throwaway's session sees the new \
                    version and picture; carol removes it, the throwaway sees it go",
            run: |cx| Box::pin(parent_picture(cx)),
        },
        SelfTest {
            name: "membership-added-live",
            about: "(stack) the throwaway on two devices: the second joins Design Lab by invite, the first shows and connects the \
                    tile (MEMBERSHIP_ADDED)",
            run: |cx| Box::pin(parent_membership_added(cx)),
        },
        SelfTest {
            name: "channel-settings-more",
            about: "(stack) Design Lab, carol: a topic saved shows in the channel header; an encrypted channel's settings say topics \
                    aren't encrypted; a voice channel's bitrate saved, then Reset to the default; a member overwrite for the throwaway \
                    through the picker, the three-way switch clicked Allow → Inherit → Deny, saved and echoed",
            run: |cx| Box::pin(with_throwaway(cx, "channel-settings-more", DESIGN_LAB)),
        },
        SelfTest {
            name: "overview-meta-live",
            about: "(stack) Rustaceans (bob): the name saved and echoed, then put back; Storage's headroom stepped with ↑, saved, \
                    echoed, put back. Daily Gaming (alice): the public listing switched off and on again, each echoed",
            run: |cx| Box::pin(parent_meta(cx)),
        },
        SelfTest {
            name: "server-settings-guard",
            about: "(stack) an unsaved Overview edit (never sent) holds Server Settings: Esc, ✕, a backdrop press and a tab switch \
                    each shake the save bar and keep the window and the tab",
            run: |cx| Box::pin(parent(cx, "server-settings-guard", false)),
        },
    ]
}

pub(super) fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "roles-reorder", about: "(in the app) Alt+arrows, drag, holders", run: |cx| Box::pin(check_roles_reorder(cx)) },
        SelfTest { name: "moderation-live", about: "(in the app) time out, ban, kick", run: |cx| Box::pin(check_moderation(cx)) },
        SelfTest { name: "transfer-ownership-live", about: "(in the app) give the community away, or check it came back", run: |cx| Box::pin(check_transfer(cx)) },
        SelfTest { name: "server-picture-live", about: "(in the app) upload and remove the picture", run: |cx| Box::pin(check_picture(cx)) },
        SelfTest { name: "server-settings-guard", about: "(in the app) the leave guard", run: |cx| Box::pin(check_guard(cx)) },
        SelfTest { name: "channel-settings-more", about: "(in the app) topic, note, bitrate, member overwrite", run: |cx| Box::pin(check_channel_more(cx)) },
        SelfTest { name: "overview-meta-live", about: "(in the app) name, headroom, listing", run: |cx| Box::pin(check_meta(cx)) },
    ]
}

// ── Parents ─────────────────────────────────────────────────────────────────────────────────

fn finish(cx: &TestCx, result: Result<Child>) -> Result<()> {
    match result? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            skip_no_login(cx);
            Ok(())
        }
    }
}

/// The throwaway joins `server`; its owner runs `check` on it; the cleanup child runs after.
async fn with_throwaway(cx: TestCx, check: &'static str, server: i32) -> Result<()> {
    let Some(stack) = stack_for(server)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let result = async {
        join_throwaway(&cx, &stack, &user).await?;
        run_child(&cx, check, Some(stack.owner.as_str()), &stack, &[("DECIBELL_ST_USER", user.clone())]).await
    }
    .await;
    if !matches!(result, Ok(Child::NoDevLogin)) {
        clean_up(&cx, &stack, Some(&user)).await;
    }
    finish(&cx, result)
}

/// A state file shared by one run's children.
fn state_env(user: &str, phase: &str, file: &Path, stack: &Stack) -> Vec<(&'static str, String)> {
    vec![
        ("DECIBELL_ST_USER", user.to_string()),
        ("DECIBELL_ST_PHASE", phase.to_string()),
        ("DECIBELL_ST_STATE", file.display().to_string()),
        ("DECIBELL_ST_OWNER", stack.owner.clone()),
    ]
}

async fn throwaway_child(cx: &TestCx, stack: &Stack, user: &str, phase: &str, file: &Path) -> Result<Child> {
    run_child(cx, "settings-throwaway", None, stack, &state_env(user, phase, file, stack)).await
}

async fn parent_moderation(cx: TestCx) -> Result<()> {
    let Some(stack) = stack_for(DESIGN_LAB)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let dir = TempDir::new("settings-moderation")?;
    let file = dir.0.join("state.json");
    let owner = stack.owner.clone();
    let result = async {
        join_throwaway(&cx, &stack, &user).await?;
        if let Child::NoDevLogin = run_child(&cx, "moderation-live", Some(&owner), &stack, &state_env(&user, "timeout", &file, &stack)).await? {
            return Ok(Child::NoDevLogin);
        }
        throwaway_child(&cx, &stack, &user, "post", &file).await?;
        run_child(&cx, "moderation-live", Some(&owner), &stack, &state_env(&user, "ban", &file, &stack)).await?;
        join_throwaway(&cx, &stack, &user).await?;
        run_child(&cx, "moderation-live", Some(&owner), &stack, &state_env(&user, "kick", &file, &stack)).await
    }
    .await;
    if !matches!(result, Ok(Child::NoDevLogin)) {
        clean_up(&cx, &stack, Some(&user)).await;
    }
    finish(&cx, result)
}

async fn parent_transfer(cx: TestCx) -> Result<()> {
    let Some(stack) = stack_for(RUSTACEANS)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let dir = TempDir::new("settings-transfer")?;
    let file = dir.0.join("state.json");
    let owner = stack.owner.clone();
    let given = async {
        join_throwaway(&cx, &stack, &user).await?;
        run_child(&cx, "transfer-ownership-live", Some(&owner), &stack, &state_env(&user, "give", &file, &stack)).await
    }
    .await;
    if matches!(given, Ok(Child::NoDevLogin)) {
        skip_no_login(&cx);
        return Ok(());
    }
    // Whatever happened: the throwaway gives the community back if it holds it, and bob checks.
    let back = throwaway_child(&cx, &stack, &user, "give-back", &file).await;
    let back = match back {
        Ok(_) => Ok(()),
        // Once more: a give-back that failed half-way (the answer lost) leaves it with the throwaway.
        Err(e) => {
            cx.log(format_args!("the give-back failed ({e:#}): once more"));
            throwaway_child(&cx, &stack, &user, "give-back", &file).await.map(|_| ())
        }
    };
    let verified = run_child(&cx, "transfer-ownership-live", Some(&owner), &stack, &state_env(&user, "verify", &file, &stack)).await;
    clean_up(&cx, &stack, Some(&user)).await;
    verified.context("Rustaceans' owner after the test (bob must own it again)")?;
    back?;
    finish(&cx, given)
}

async fn parent_picture(cx: TestCx) -> Result<()> {
    let Some(stack) = stack_for(DESIGN_LAB)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let dir = TempDir::new("settings-picture")?;
    let file = dir.0.join("state.json");
    let owner = stack.owner.clone();
    let result = async {
        join_throwaway(&cx, &stack, &user).await?;
        // Side by side: the observer signs in and says so (a file), carol waits for it.
        let observer = throwaway_child(&cx, &stack, &user, "observe-picture", &file);
        let env = state_env(&user, "picture", &file, &stack);
        let uploader = run_child(&cx, "server-picture-live", Some(&owner), &stack, &env);
        let (observed, uploaded) = futures::join!(observer, uploader);
        let uploaded = uploaded?;
        observed?;
        Ok(uploaded)
    }
    .await;
    if !matches!(result, Ok(Child::NoDevLogin)) {
        clean_up_with(&cx, &stack, Some(&user), &[("DECIBELL_ST_PICTURE", "1".into())]).await;
    }
    finish(&cx, result)
}

async fn parent_membership_added(cx: TestCx) -> Result<()> {
    let Some(stack) = stack_for(DESIGN_LAB)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let dir = TempDir::new("settings-membership")?;
    let file = dir.0.join("state.json");
    // Not a member to begin with (an earlier run may have left it one).
    clean_up(&cx, &stack, Some(&user)).await;
    let result = async {
        let watcher = throwaway_child(&cx, &stack, &user, "watch-membership", &file);
        let joiner = async {
            // The first device is signed in and has its tiles.
            wait_file(&cx, &file.with_extension("ready"), Duration::from_secs(90)).await?;
            throwaway_child(&cx, &stack, &user, "join-device", &file).await
        };
        let (watched, joined) = futures::join!(watcher, joiner);
        joined?;
        watched
    }
    .await;
    clean_up(&cx, &stack, Some(&user)).await;
    result.map(|_| ())
}

async fn clean_up_with(cx: &TestCx, stack: &Stack, throwaway: Option<&str>, extra: &[(&str, String)]) {
    let mut env: Vec<(&str, String)> = throwaway.map(|u| vec![("DECIBELL_ST_USER", u.to_string())]).unwrap_or_default();
    env.extend(extra.iter().cloned());
    if let Err(e) = run_child(cx, "settings-cleanup", Some(stack.owner.as_str()), stack, &env).await {
        cx.log(format_args!("cleanup failed: {e:#}"));
    }
}

async fn wait_file(cx: &TestCx, path: &Path, limit: Duration) -> Result<()> {
    let started = Instant::now();
    while !path.exists() {
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {}", limit.as_secs(), path.display());
        cx.sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn touch(path: &Path) -> Result<()> {
    std::fs::write(path, b"1").with_context(|| format!("writing {}", path.display()))
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is not set"))
}

fn state_file() -> Result<PathBuf> {
    Ok(PathBuf::from(env("DECIBELL_ST_STATE")?))
}

fn write_json(path: &Path, value: serde_json::Value) -> Result<()> {
    std::fs::write(path, serde_json::to_vec(&value)?).with_context(|| format!("writing {}", path.display()))
}

fn read_json(path: &Path) -> Result<serde_json::Value> {
    Ok(serde_json::from_slice(&std::fs::read(path).with_context(|| format!("reading {}", path.display()))?)?)
}

// ── Shared pieces inside the app ──────────────────────────────────────────────────────────────

fn roles_list(cx: &App) -> Vec<Role> {
    model(cx).community(daily_gaming(), cx).map(|c| c.roles.read(cx).list().to_vec()).unwrap_or_default()
}

fn role_named(cx: &App, name: &str) -> Option<Role> {
    roles_list(cx).into_iter().find(|r| r.name == name)
}

fn position(cx: &App, id: i64) -> i32 {
    roles_list(cx).iter().find(|r| r.id == id).map_or(-1, |r| r.position)
}

fn roster_member(cx: &App, uid: i64) -> Option<Member> {
    model(cx).community(daily_gaming(), cx).and_then(|c| c.roster.read(cx).member(uid).cloned())
}

async fn wait_probe(cx: &TestCx, key: &str) -> Result<gpui::Bounds<Pixels>> {
    let k = key.to_string();
    wait_for(cx, Duration::from_secs(15), &format!("the probe {key}"), move |_| decibell_harness::probes::bounds(&k).is_some()).await?;
    decibell_harness::probes::bounds(key).context("probe")
}

/// Click what `key` was last laid out at, once the window drew what is pending.
async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    cx.settle().await?;
    let bounds = wait_probe(cx, key).await?;
    cx.click(bounds.center())
}

/// Click `key` while a text field has the focus (its caret blinks: the window never settles):
/// give the window a moment to draw what is pending first.
async fn click_probe_now(cx: &TestCx, key: &str) -> Result<()> {
    cx.sleep(Duration::from_millis(250)).await;
    let bounds = wait_probe(cx, key).await?;
    cx.click(bounds.center())
}

/// A real drag: press `from`, move in steps a frame apart, release at `to`.
async fn drag(cx: &TestCx, from: Point<Pixels>, to: Point<Pixels>) -> Result<()> {
    cx.mouse_down(from, 1, Modifiers::default())?;
    for i in 1..=10 {
        let f = i as f32 / 10.;
        cx.mouse_drag(point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f))?;
        cx.sleep(Duration::from_millis(16)).await;
    }
    cx.sleep(Duration::from_millis(60)).await;
    cx.mouse_up(to)
}

fn now() -> i64 {
    crate::now_secs()
}

/// Popovers close when their window stops being the active one (as the renderer's do on blur),
/// and other windows share this desktop: hold the window active for the checks that open one.
fn hold_active(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))
}

// ── roles-reorder ───────────────────────────────────────────────────────────────────────────

async fn check_roles_reorder(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    hold_active(&cx)?;
    let user = env("DECIBELL_ST_USER")?;
    let member = find_member(&cx, &user).await?;
    let (a_name, b_name) = (format!("{}-a", tag()), format!("{}-b", tag()));
    // New roles land at position 1: b, made last, starts right under a.
    for name in [&a_name, &b_name] {
        send(&cx, Request::CreateRole { server: daily_gaming(), name: name.clone(), color: 0, permissions: 0, manage_each_other: false }).await?;
        let n = name.clone();
        wait_for(&cx, Duration::from_secs(8), &format!("the role {name}"), move |cx| role_named(cx, &n).is_some()).await?;
    }
    let a = cx.update(|_, cx| role_named(cx, &a_name))?.context("a")?.id;
    let b = cx.update(|_, cx| role_named(cx, &b_name))?.context("b")?.id;
    let result = async {
        ensure!(cx.update(|_, cx| position(cx, a) == position(cx, b) + 1)?, "b isn't right under a after creating them");
        let (_window, tab) = open_settings(&cx, TabId::Roles).await?;
        let Tab::Roles(roles) = tab else { bail!("not the Roles tab") };
        let focused = |cx: &TestCx, id: i64| -> Result<bool> {
            let roles = roles.clone();
            cx.update(move |w, cx| roles.read(cx).row_focus(id).is_some_and(|h| h.is_focused(w)))
        };
        // A press on b's row selects it and gives it the focus (it is a button in the renderer).
        click_probe(&cx, &format!("role-row:{b}")).await?;
        let r = roles.clone();
        wait_for(&cx, Duration::from_secs(5), "b selected", move |cx| r.read(cx).selected() == Some(Selected::Role(b))).await?;
        cx.settle().await?;
        ensure!(focused(&cx, b)?, "the pressed row didn't take the focus");

        // Alt+↑: one place up, on the echo; the focus stays on b's row.
        let sent = Sent::record(&cx)?;
        let (pa, pb) = cx.update(|_, cx| (position(cx, a), position(cx, b)))?;
        cx.keystroke("alt-up")?;
        let t = wait_for(&cx, Duration::from_secs(8), "Alt+↑'s echo", move |cx| position(cx, b) == pa && position(cx, a) == pb).await?;
        let request = sent.find(|r| matches!(r, Request::UpdateRole { role_id, .. } if *role_id == b)).context("Alt+↑ sent no update")?;
        cx.update(|_, cx| requests::uninstall(cx))?;
        let Request::UpdateRole { position: asked, .. } = request else { bail!("not an update") };
        ensure!(asked == pb + 1, "Alt+↑ asked for position {asked}, b was at {pb}");
        cx.settle().await?;
        let (top_a, top_b) = (wait_probe(&cx, &format!("role-row:{a}")).await?.top(), wait_probe(&cx, &format!("role-row:{b}")).await?.top());
        ensure!(top_b < top_a, "the list didn't re-sort: b's row at {top_b:?}, a's at {top_a:?}");
        ensure!(focused(&cx, b)?, "the focus didn't follow b's row");
        cx.log(format_args!("Alt+↑: b moved to position {} on the echo in {} ms; the focus followed it", pa, t.as_millis()));

        // Alt+↓: back.
        cx.keystroke("alt-down")?;
        wait_for(&cx, Duration::from_secs(8), "Alt+↓'s echo", move |cx| position(cx, a) == pa && position(cx, b) == pb).await?;
        cx.settle().await?;
        ensure!(focused(&cx, b)?, "the focus didn't follow b's row back");
        cx.log("Alt+↓: b back under a; the focus still on b");

        // A drag: b over a's upper half drops it above a.
        let from = wait_probe(&cx, &format!("role-row:{b}")).await?;
        let to = wait_probe(&cx, &format!("role-row:{a}")).await?;
        drag(&cx, from.center(), point(to.center().x, to.top() + to.size.height * 0.25)).await?;
        let t = wait_for(&cx, Duration::from_secs(8), "the drag's echo", move |cx| position(cx, b) == pa && position(cx, a) == pb).await?;
        cx.log(format_args!("dragged b above a: echoed in {} ms", t.as_millis()));

        // The custom colour, driven as UI: the picker opens on its hex code, selected; typing six
        // digits applies them; a press in the saturation / value field picks there; Save.
        click_probe(&cx, &format!("role-row:{a}")).await?;
        let r = roles.clone();
        wait_for(&cx, Duration::from_secs(5), "a selected", move |cx| r.read(cx).selected() == Some(Selected::Role(a))).await?;
        click_probe(&cx, "role-section:Display").await?;
        click_probe(&cx, "role-color-custom").await?;
        // The hex field has the focus (a blinking caret from here on).
        cx.sleep(Duration::from_millis(300)).await;
        cx.type_text("123456")?;
        let r = roles.clone();
        wait_for(&cx, Duration::from_secs(3), "#123456 in the draft", move |cx| r.read(cx).fields(cx).color == 0x123456).await?;
        let field = wait_probe(&cx, "color-field").await?;
        // Its top-right corner: full saturation and value at #123456's hue (210°).
        cx.click(point(field.right() - px(2.), field.top() + px(2.)))?;
        let r = roles.clone();
        wait_for(&cx, Duration::from_secs(3), "the field's pick in the draft", move |cx| {
            let c = r.read(cx).fields(cx).color;
            let ch = [(c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff];
            ch.iter().max() >= Some(&250) && ch.iter().min() <= Some(&5) && c != 0x123456
        })
        .await?;
        let picked = cx.update(|_, cx| roles.read(cx).fields(cx).color)?;
        click_probe_now(&cx, "roles-save-save").await?;
        let t = wait_for(&cx, Duration::from_secs(8), "the colour's echo", move |cx| roles_list(cx).iter().any(|r| r.id == a && r.color == picked)).await?;
        cx.log(format_args!("the colour picker: #123456 typed, then #{picked:06x} picked in the field, saved and echoed in {} ms", t.as_millis()));
        // Holders: a's Members section, Add members, the throwaway picked, added; then its ✕.
        click_probe(&cx, "role-section:Members").await?;
        click_probe(&cx, "holders-add").await?;
        // The picker's search field has the focus (a blinking caret from here on).
        cx.sleep(Duration::from_millis(250)).await;
        cx.type_text(&user)?;
        click_probe_now(&cx, &format!("add-member:{}", member.uid)).await?;
        let r = roles.clone();
        wait_for(&cx, Duration::from_secs(5), "the throwaway picked", move |cx| r.read(cx).add_picker_state(cx).is_some_and(|(_, picked)| picked == 1))
            .await
            .with_context(|| format!("picker: {:?}", cx.update(|_, cx| roles.read(cx).add_picker_state(cx)).ok().flatten()))?;
        click_probe_now(&cx, "add-members-run").await?;
        let uid = member.uid;
        let t = wait_for(&cx, Duration::from_secs(10), "the throwaway to hold a", move |cx| roster_member(cx, uid).is_some_and(|m| m.role_ids.contains(&a))).await?;
        cx.log(format_args!("{user} added to {a_name} through Add members ({} ms)", t.as_millis()));
        let row = wait_probe(&cx, &format!("holder:{uid}")).await?;
        // The ✕ shows on the row's hover.
        cx.mouse_move(row.center())?;
        click_probe_now(&cx, &format!("holder-remove:{uid}")).await?;
        wait_for(&cx, Duration::from_secs(10), "the throwaway to lose a", move |cx| roster_member(cx, uid).is_some_and(|m| !m.role_ids.contains(&a))).await?;
        cx.log("removed again with the row's ✕");
        anyhow::Ok(())
    }
    .await;
    cx.update(|_, cx| requests::uninstall(cx))?;
    for id in [a, b] {
        send(&cx, Request::DeleteRole { server: daily_gaming(), role_id: id }).await?;
    }
    wait_for(&cx, Duration::from_secs(8), "the roles to be deleted", move |cx| position(cx, a) < 0 && position(cx, b) < 0).await?;
    result
}

// ── moderation-live ─────────────────────────────────────────────────────────────────────────

async fn members_tab(cx: &TestCx) -> Result<Entity<crate::server::members::MembersTab>> {
    let (_, tab) = open_settings(cx, TabId::Members).await?;
    let Tab::Members(members) = tab else { bail!("not the Members tab") };
    Ok(members)
}

async fn check_moderation(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let user = env("DECIBELL_ST_USER")?;
    let file = state_file()?;
    match env("DECIBELL_ST_PHASE")?.as_str() {
        "timeout" => {
            let member = find_member(&cx, &user).await?;
            let uid = member.uid;
            let members = members_tab(&cx).await?;
            let gates = |cx: &App| crate::server::members::Gates::read(daily_gaming(), cx);
            ensure!(cx.update(|_, cx| roster_member(cx, uid).is_some_and(|m| gates(cx).acts(&m, now()).timeout))?, "Time out isn't offered for the throwaway");
            cx.update(|_, cx| {
                members.update(cx, |t, cx| {
                    t.open_confirm(ModKind::Timeout, member.clone(), cx);
                    t.set_mod_options(0, 0, 600, cx);
                    t.run_mod(cx);
                })
            })?;
            let t = wait_for(&cx, Duration::from_secs(10), "the time-out", move |cx| roster_member(cx, uid).is_some_and(|m| m.timed_out_until > now() + 500)).await?;
            let until = cx.update(|_, cx| roster_member(cx, uid).map_or(0, |m| m.timed_out_until))?;
            ensure!((until - now() - 600).abs() < 60, "timed out until {until}, expected ten minutes from now ({})", now());
            ensure!(cx.update(|_, cx| roster_member(cx, uid).is_some_and(|m| gates(cx).acts(&m, now()).end_timeout))?, "End timeout isn't offered");
            cx.log(format_args!("timed out for ten minutes ({} ms); End timeout offered", t.as_millis()));
            let m = cx.update(|_, cx| roster_member(cx, uid))?.context("the member")?;
            cx.update(|_, cx| members.update(cx, |t, cx| t.end_timeout(&m, cx)))?;
            wait_for(&cx, Duration::from_secs(10), "End timeout", move |cx| roster_member(cx, uid).is_some_and(|m| m.timed_out_until <= now())).await?;
            cx.log("End timeout: the time-out is gone");
            // The channel the throwaway posts in next.
            let channel = create_channel(&cx, &format!("{}-mod", tag()), "text", None).await?;
            write_json(&file, serde_json::json!({ "channel": channel, "uid": uid }))
        }
        "ban" => {
            let state = read_json(&file)?;
            let channel = state["channel"].as_str().context("channel")?.to_string();
            let uid = state["uid"].as_i64().context("uid")?;
            let words = format!("moderation {}", tag());
            // The post, in the channel's history.
            let id = ChannelId::from(channel.clone());
            let server = daily_gaming();
            cx.update(|_, cx| actions::chat::open_channel(server, id.clone(), cx))?;
            let (w, i) = (words.clone(), id.clone());
            let has_post = move |cx: &App| {
                model(cx).chat.channels.read(cx).get(server, &i).is_some_and(|s| s.read(cx).rows().iter().any(|m| m.content == w.as_str()))
            };
            let h = has_post.clone();
            wait_for(&cx, Duration::from_secs(10), "the throwaway's post", move |cx| h(cx)).await?;
            let member = find_member(&cx, &user).await?;
            let members = members_tab(&cx).await?;
            cx.update(|_, cx| {
                members.update(cx, |t, cx| {
                    t.open_confirm(ModKind::Ban, member.clone(), cx);
                    t.set_mod_options(3600, 3600, 600, cx);
                    t.run_mod(cx);
                })
            })?;
            wait_bans(&cx, "the ban to be listed", |bans| bans.iter().any(|b| b.uid == uid)).await?;
            let ban = cx
                .update(|_, cx| model(cx).community(server, cx).and_then(|c| c.moderation.read(cx).bans.iter().find(|b| b.uid == uid).cloned()))?
                .context("the ban")?;
            ensure!((ban.expires_at - now() - 3600).abs() < 120, "the ban expires at {} (now {})", ban.expires_at, now());
            let h = has_post.clone();
            let t = wait_for(&cx, Duration::from_secs(10), "the purge to take the post", move |cx| !h(cx)).await?;
            ensure!(cx.update(|_, cx| roster_member(cx, uid).is_none())?, "the banned member is still in the roster");
            cx.log(format_args!("banned for an hour (listed until {}); the last hour's post went ({} ms)", ban.expires_at, t.as_millis()));
            // Lifted from the Bans tab.
            let window = cx.update(|_, cx| host::server_window(cx))?.context("the window")?;
            let (_, tab) = show(&cx, &window, TabId::Bans).await?;
            let Tab::Bans(bans) = tab else { bail!("not the Bans tab") };
            cx.update(|_, cx| bans.update(cx, |t, cx| t.unban(uid, ban.username.clone(), cx)))?;
            wait_bans(&cx, "the unban", |bans| !bans.iter().any(|b| b.uid == uid)).await?;
            cx.log("unbanned from the Bans tab");
            send(&cx, Request::DeleteChannel { server, channel: id }).await?;
            Ok(())
        }
        "kick" => {
            let member = find_member(&cx, &user).await?;
            let uid = member.uid;
            let members = members_tab(&cx).await?;
            cx.update(|_, cx| {
                members.update(cx, |t, cx| {
                    t.open_confirm(ModKind::Kick, member.clone(), cx);
                    t.run_mod(cx);
                })
            })?;
            let t = wait_for(&cx, Duration::from_secs(10), "the kick", move |cx| roster_member(cx, uid).is_none()).await?;
            let search = cx.update(|_, cx| cx.new(|cx| MemberSearch::new(daily_gaming(), &user, 0, cx)))?;
            wait_for(&cx, Duration::from_secs(10), "a search after the kick", |cx| search.read(cx).loaded && !search.read(cx).loading).await?;
            ensure!(cx.update(|_, cx| !search.read(cx).members.iter().any(|m| m.uid == uid))?, "the kicked member is still found");
            cx.log(format_args!("kicked ({} ms): gone from the roster and the search", t.as_millis()));
            Ok(())
        }
        other => bail!("unknown phase {other:?}"),
    }
}

// ── transfer-ownership-live ──────────────────────────────────────────────────────────────────

fn owner_of(cx: &App) -> i64 {
    model(cx).community(daily_gaming(), cx).map_or(0, |c| c.meta.read(cx).owner)
}

/// Overview's Transfer ownership as a person drives it: the picker, the member found by name, the
/// name typed into the confirmation, the button.
async fn transfer_to(cx: &TestCx, name: &str, uid: i64) -> Result<()> {
    hold_active(cx)?;
    let (window, _) = open_settings(cx, TabId::Overview).await?;
    wait_for(cx, Duration::from_secs(10), "our ownership", |cx| model(cx).is_owner(daily_gaming(), cx)).await?;
    cx.update(|_, cx| window.update(cx, |w, cx| w.scroll_pane_to_bottom(cx)))?;
    click_probe(cx, "owner-picker-trigger").await?;
    // The picker's search field has the focus (a blinking caret from here on).
    cx.sleep(Duration::from_millis(250)).await;
    cx.type_text(name)?;
    click_probe_now(cx, &format!("owner-option:{uid}")).await?;
    cx.update(|_, cx| window.update(cx, |w, cx| w.scroll_pane_to_bottom(cx)))?;
    click_probe_now(cx, "transfer-confirm").await?;
    cx.sleep(Duration::from_millis(250)).await;
    cx.type_text(name)?;
    click_probe_now(cx, "transfer-run").await?;
    let t = wait_for(cx, Duration::from_secs(10), "the new owner", move |cx| owner_of(cx) == uid).await?;
    cx.log(format_args!("ownership transferred to {name} ({} ms)", t.as_millis()));
    wait_for(cx, Duration::from_secs(5), "the Overview to drop the transfer section", |cx| !model(cx).is_owner(daily_gaming(), cx)).await?;
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    Ok(())
}

async fn check_transfer(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let user = env("DECIBELL_ST_USER")?;
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    wait_for(&cx, Duration::from_secs(10), "the owner's uid", |cx| owner_of(cx) != 0).await?;
    match env("DECIBELL_ST_PHASE")?.as_str() {
        "give" => {
            ensure!(cx.update(|_, cx| owner_of(cx))? == me, "we don't own the community to begin with");
            let member = find_member(&cx, &user).await?;
            write_json(&state_file()?, serde_json::json!({ "owner_uid": me }))?;
            transfer_to(&cx, &user, member.uid).await
        }
        "verify" => {
            let owner = cx.update(|_, cx| owner_of(cx))?;
            ensure!(owner == me, "the community is owned by uid {owner}, not by us ({me}): give it back by hand");
            cx.log("we own the community again");
            Ok(())
        }
        other => bail!("unknown phase {other:?}"),
    }
}

/// The throwaway's side: give the community back to its seeded owner if it holds it.
async fn give_back(cx: &TestCx) -> Result<()> {
    let owner = env("DECIBELL_ST_OWNER")?;
    wait_for(cx, Duration::from_secs(30), "the community to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    wait_for(cx, Duration::from_secs(10), "the owner's uid", |cx| owner_of(cx) != 0).await?;
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    if cx.update(|_, cx| owner_of(cx))? != me {
        cx.log("the throwaway doesn't own the community: nothing to give back");
        return Ok(());
    }
    let member = find_member(cx, &owner).await?;
    transfer_to(cx, &owner, member.uid).await
}

// ── server-picture-live ─────────────────────────────────────────────────────────────────────

fn picture_version(cx: &App) -> String {
    model(cx).servers.read(cx).picture_version(daily_gaming()).to_string()
}

async fn check_picture(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let file = state_file()?;
    ensure!(cx.update(|_, cx| picture_version(cx))?.is_empty(), "the community has a picture already: this test restores only \"none\"");
    wait_file(&cx, &file.with_extension("ready"), Duration::from_secs(120)).await?;
    let (window, tab) = open_settings(&cx, TabId::Overview).await?;
    let Tab::Overview(overview) = tab else { bail!("not the Overview tab") };
    let png = {
        let img = sample_rgba(1200, 600);
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png)?;
        out.into_inner()
    };
    cx.update(|w, cx| overview.update(cx, |t, cx| t.open_editor(PictureSource::Bytes { bytes: png, name: "st-picture.png".into() }, w, cx)))?;
    let o = overview.clone();
    wait_for(&cx, Duration::from_secs(10), "the editor's picture", move |cx| o.read(cx).editor().is_some_and(|e| e.read(cx).picture().is_some())).await?;
    cx.settle().await?;
    // The editor's Save.
    cx.keystroke("enter")?;
    let t = wait_for(&cx, Duration::from_secs(15), "the new picture version", |cx| !picture_version(cx).is_empty()).await?;
    let version = cx.update(|_, cx| picture_version(cx))?;
    cx.log(format_args!("uploaded: version {version} ({} ms)", t.as_millis()));
    touch(&file.with_extension("uploaded"))?;
    wait_file(&cx, &file.with_extension("seen"), Duration::from_secs(30)).await?;
    // Remove, through its confirmation.
    let _ = window;
    click_probe(&cx, "picture-remove").await?;
    cx.sleep(Duration::from_millis(350)).await;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(10), "the picture to be removed", |cx| picture_version(cx).is_empty()).await?;
    cx.log("removed through its confirmation");
    touch(&file.with_extension("removed"))?;
    wait_file(&cx, &file.with_extension("seen-removed"), Duration::from_secs(30)).await?;
    Ok(())
}

async fn observe_picture(cx: &TestCx) -> Result<()> {
    let file = state_file()?;
    wait_for(cx, Duration::from_secs(30), "the community to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    let before = cx.update(|_, cx| picture_version(cx))?;
    ensure!(before.is_empty(), "a picture before the upload: {before}");
    touch(&file.with_extension("ready"))?;
    let t = wait_for(cx, Duration::from_secs(60), "the new picture to arrive", |cx| {
        !picture_version(cx).is_empty() && model(cx).servers.read(cx).picture(daily_gaming()).is_some()
    })
    .await?;
    let version = cx.update(|_, cx| picture_version(cx))?;
    cx.log(format_args!("the second session sees version {version} and its picture ({} ms after it was ready)", t.as_millis()));
    touch(&file.with_extension("seen"))?;
    wait_for(cx, Duration::from_secs(60), "the removal to arrive", |cx| picture_version(cx).is_empty()).await?;
    cx.log("the second session sees the picture removed");
    touch(&file.with_extension("seen-removed"))
}

// ── membership-added-live ───────────────────────────────────────────────────────────────────

async fn watch_membership(cx: &TestCx) -> Result<()> {
    let file = state_file()?;
    let server = daily_gaming();
    wait_for(cx, Duration::from_secs(15), "the memberships", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.sleep(Duration::from_secs(3)).await;
    ensure!(cx.update(move |_, cx| !model(cx).connections.read(cx).is_connected(server))?, "already a member of the community");
    touch(&file.with_extension("ready"))?;
    let t = wait_for(cx, Duration::from_secs(60), "the tile of the community joined elsewhere", move |cx| {
        let m = model(cx);
        m.servers.read(cx).get(server).is_some() && m.connections.read(cx).is_connected(server)
    })
    .await?;
    cx.settle().await?;
    let tile = wait_probe(cx, &format!("server-tile:{}", server.get())).await?;
    cx.log(format_args!("the other device's join reached this one: the tile shows ({tile:?}) and connected after {} ms", t.as_millis()));
    Ok(())
}

/// The throwaway's phases this module adds (`check_throwaway` signs it in first and out after).
pub(super) async fn throwaway_phase(cx: &TestCx, phase: &str) -> Result<()> {
    match phase {
        "post" => {
            let state = read_json(&state_file()?)?;
            let channel = state["channel"].as_str().context("channel")?.to_string();
            let server = daily_gaming();
            wait_for(cx, Duration::from_secs(30), "the community to connect", move |cx| model(cx).connections.read(cx).is_connected(server)).await?;
            let c = channel.clone();
            wait_for(cx, Duration::from_secs(10), "the channel", move |cx| channel_of(cx, &c).is_some()).await?;
            let id = ChannelId::from(channel);
            cx.update(|_, cx| actions::chat::open_channel(server, id.clone(), cx))?;
            cx.sleep(Duration::from_millis(500)).await;
            let words = format!("moderation {}", tag());
            let w = words.clone();
            cx.update(|_, cx| actions::chat::send_message(server, id.clone(), &w, 0, cx))?;
            wait_for(cx, Duration::from_secs(10), "the post's echo", move |cx| {
                model(cx).chat.channels.read(cx).get(server, &id).is_some_and(|s| s.read(cx).rows().iter().any(|m| m.content == words.as_str() && !m.pending))
            })
            .await?;
            cx.log("posted in the test channel");
            Ok(())
        }
        "give-back" => give_back(cx).await,
        "observe-picture" => observe_picture(cx).await,
        "watch-membership" => watch_membership(cx).await,
        other => bail!("unknown phase {other:?}"),
    }
}

// ── server-settings-guard ───────────────────────────────────────────────────────────────────

async fn check_guard(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let (window, tab) = open_settings(&cx, TabId::Overview).await?;
    let Tab::Overview(overview) = tab else { bail!("not on Overview") };
    let original = cx.update(|_, cx| community_meta(cx).1)?;
    let result = async {
        // An edit that is never saved.
        cx.update(|_, cx| overview.update(cx, |t, cx| t.type_description(&format!("{original} (unsaved)"), cx)))?;
        ensure!(cx.update(|_, cx| overview.read(cx).meta_dirty(cx))?, "typing made no draft");
        cx.settle().await?;
        let held = |what: &str, before: u32| -> Result<u32> {
            let (nudge, open, tab, dirty) = cx.update(|_, cx| {
                let w = window.read(cx);
                (w.nudge(), model(cx).overlays.read(cx).is_open(modal::SERVER_SETTINGS), w.tab().id(), overview.read(cx).meta_dirty(cx))
            })?;
            ensure!(open && tab == TabId::Overview && dirty, "{what}: the window or its edits went (open {open}, tab {tab:?}, dirty {dirty})");
            ensure!(nudge > before, "{what} didn't shake the save bar (nudge {before} → {nudge})");
            cx.log(format_args!("{what}: refused, the save bar shook (nudge {nudge})"));
            Ok(nudge)
        };
        let mut nudge = cx.update(|_, cx| window.read(cx).nudge())?;
        cx.keystroke("escape")?;
        cx.settle().await?;
        nudge = held("Esc", nudge)?;
        click_probe(&cx, "settings-close").await?;
        cx.settle().await?;
        nudge = held("✕", nudge)?;
        // The backdrop: a press near the window's lower left corner, outside the settings frame.
        let visible = cx.update(|w, cx| decibell_ui::visible_window_bounds(w, cx))?;
        cx.click(point(visible.left() + px(12.), visible.bottom() - px(12.)))?;
        cx.settle().await?;
        nudge = held("a backdrop press", nudge)?;
        click_probe(&cx, "server-tab:roles").await?;
        cx.settle().await?;
        held("switching to Roles", nudge)?;
        anyhow::Ok(())
    }
    .await;
    // Undo the edit (nothing was sent): now the window goes.
    cx.update(|_, cx| overview.update(cx, |t, cx| t.type_description(&original, cx)))?;
    result?;
    ensure!(!cx.update(|_, cx| overview.read(cx).meta_dirty(cx))?, "still dirty after putting the text back");
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "the window to close", |cx| !model(cx).overlays.read(cx).is_open(modal::SERVER_SETTINGS)).await?;
    cx.log("with nothing unsaved, Esc closes it");
    Ok(())
}

// ── channel-settings-more ───────────────────────────────────────────────────────────────────

async fn close_settings(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    wait_for(cx, Duration::from_secs(5), "the settings to close", |cx| host::channel_window(cx).is_none() && host::server_window(cx).is_none()).await?;
    Ok(())
}

async fn check_channel_more(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    hold_active(&cx)?;
    let user = env("DECIBELL_ST_USER")?;
    let member = find_member(&cx, &user).await?;
    let server = daily_gaming();
    let text = create_channel(&cx, &format!("{}-t", tag()), "text", None).await?;
    let voice = create_channel(&cx, &format!("{}-v", tag()), "voice", None).await?;
    let sealed_name = format!("{}-e", tag());
    send(&cx, Request::CreateChannel { server, name: sealed_name.clone(), kind: "text", category: None, encrypted: true }).await?;
    let n = sealed_name.clone();
    wait_for(&cx, Duration::from_secs(8), "the encrypted channel", move |cx| channel_named(cx, &n).is_some()).await?;
    let sealed = cx.update(|_, cx| channel_named(cx, &sealed_name))?.context("vanished")?.id.to_string();
    let result = async {
        // A topic: saved, echoed, drawn in the header.
        let id = ChannelId::from(text.clone());
        cx.update(move |_, cx| actions::chat::open_channel(server, id, cx))?;
        let window = open_channel_settings(&cx, &text).await?;
        cx.settle().await?;
        ensure!(!decibell_harness::probes::flag_set("channel-topic-unencrypted-note"), "a plain channel says its topic isn't encrypted");
        let topic = format!("What {} is about", tag());
        let t2 = topic.clone();
        let w = window.clone();
        cx.update(move |_, cx| w.update(cx, |w, cx| w.topic.update(cx, |s, cx| s.set_value(&t2, cx))))?;
        cx.update(|_, cx| window.update(cx, |w, cx| w.save(cx)))?;
        let (c, t2, w) = (text.clone(), topic.clone(), window.clone());
        wait_for(&cx, Duration::from_secs(8), "the topic's echo", move |cx| {
            !w.read(cx).saving() && !w.read(cx).dirty(cx) && channel_of(cx, &c).is_some_and(|c| c.topic == t2.as_str())
        })
        .await?;
        close_settings(&cx).await?;
        // The channel open (opening its settings opened the community, which may have shown
        // another of its channels).
        let id = ChannelId::from(text.clone());
        cx.update(move |_, cx| actions::chat::open_channel(server, id, cx))?;
        let t2 = topic.clone();
        wait_for(&cx, Duration::from_secs(8), "the header to draw the topic", move |_| {
            decibell_harness::probes::text_of("channel-header-topic").is_some_and(|t| t == t2.as_str())
        })
        .await?;
        cx.log("a saved topic: echoed and drawn in the channel header");

        // An encrypted channel: the topic stays in the clear, and the settings say so.
        let _window = open_channel_settings(&cx, &sealed).await?;
        wait_for(&cx, Duration::from_secs(5), "the encrypted channel's topic note", |_| {
            decibell_harness::probes::flag_set("channel-topic-unencrypted-note")
        })
        .await?;
        cx.log("an encrypted channel's Overview: \"Topics aren't encrypted. The server host can read them.\"");
        close_settings(&cx).await?;

        // A voice channel's bitrate: set, echoed; Reset to the default, echoed.
        let window = open_channel_settings(&cx, &voice).await?;
        cx.update(|_, cx| {
            window.update(cx, |w, cx| {
                w.change(cx, |d| d.bitrate = 96);
                w.save(cx);
            })
        })?;
        let (v, w) = (voice.clone(), window.clone());
        wait_for(&cx, Duration::from_secs(8), "96 kbps echoed", move |cx| {
            !w.read(cx).saving() && !w.read(cx).dirty(cx) && channel_of(cx, &v).is_some_and(|c| c.voice_bitrate_kbps == 96)
        })
        .await?;
        click_probe(&cx, "bitrate-reset").await?;
        ensure!(cx.update(|_, cx| window.read(cx).dirty(cx))?, "Reset made no change");
        cx.update(|_, cx| window.update(cx, |w, cx| w.save(cx)))?;
        let (v, w) = (voice.clone(), window.clone());
        wait_for(&cx, Duration::from_secs(8), "the default echoed", move |cx| {
            !w.read(cx).saving() && !w.read(cx).dirty(cx) && channel_of(cx, &v).is_some_and(|c| c.voice_bitrate_kbps == 0)
        })
        .await?;
        cx.log("bitrate: 96 kbps echoed; Reset back to the default (0) echoed");
        close_settings(&cx).await?;

        // A member overwrite: the throwaway picked from the + picker; its three-way switch clicked.
        let window = open_channel_settings(&cx, &text).await?;
        cx.update(|_, cx| window.update(cx, |w, cx| w.select_tab(crate::channel::Tab::Permissions, cx)))?;
        click_probe(&cx, "perm-add-target").await?;
        // The picker's search field has the focus (a blinking caret).
        cx.sleep(Duration::from_millis(250)).await;
        cx.type_text(&user)?;
        click_probe_now(&cx, &format!("perm-pick:{}", member.uid)).await?;
        let key = format!("member:{}", member.uid);
        let bit = perm::SEND_MESSAGES;
        for (label, want) in [("Allow", Bits { allow: bit, deny: 0 }), ("Inherit", Bits { allow: 0, deny: 0 }), ("Deny", Bits { allow: 0, deny: bit })] {
            let probe = format!("tri:{bit}:{label}");
            let b = wait_probe(&cx, &probe).await?;
            let w = window.clone();
            cx.update(move |_, cx| w.update(cx, |w, cx| w.reveal(b, cx)))?;
            cx.sleep(Duration::from_millis(250)).await;
            click_probe_now(&cx, &probe).await?;
            let (k, w) = (key.clone(), window.clone());
            wait_for(&cx, Duration::from_secs(3), &format!("{label} staged"), move |cx| {
                let got = w.read(cx).perms.pending_bits(&k).unwrap_or(Bits { allow: 0, deny: 0 });
                got.allow & bit == want.allow && got.deny & bit == want.deny
            })
            .await?;
            cx.log(format_args!("clicked {label}: staged"));
        }
        cx.update(|_, cx| window.update(cx, |w, cx| w.save(cx)))?;
        let (uid, win) = (member.uid, window.clone());
        wait_for(&cx, Duration::from_secs(8), "the member overwrite's echo", move |cx| {
            let w = win.read(cx);
            !w.saving() && !w.dirty(cx) && w.overwrites(cx).iter().any(|o| o.target_uid == uid && o.deny & bit != 0 && o.allow & bit == 0)
        })
        .await?;
        cx.log("a member overwrite (deny Send Messages) saved through the picker and the switch: echoed");
        close_settings(&cx).await?;
        anyhow::Ok(())
    }
    .await;
    let _ = close_settings(&cx).await;
    for id in [text, voice, sealed] {
        send(&cx, Request::DeleteChannel { server, channel: ChannelId::from(id.clone()) }).await?;
        wait_for(&cx, Duration::from_secs(8), "a delete", move |cx| channel_of(cx, &id).is_none()).await?;
    }
    result
}

// ── overview-meta-live ──────────────────────────────────────────────────────────────────────

async fn parent_meta(cx: TestCx) -> Result<()> {
    let Some(rust) = stack_for(RUSTACEANS)? else {
        skip_down(&cx);
        return Ok(());
    };
    let named = run_child(&cx, "overview-meta-live", Some(rust.owner.as_str()), &rust, &[("DECIBELL_ST_PHASE", "name-storage".into())]).await;
    if matches!(named, Ok(Child::NoDevLogin)) {
        skip_no_login(&cx);
        return Ok(());
    }
    named?;
    let Some(gaming) = stack_for(DAILY_GAMING)? else {
        skip_down(&cx);
        return Ok(());
    };
    run_child(&cx, "overview-meta-live", Some(gaming.owner.as_str()), &gaming, &[("DECIBELL_ST_PHASE", "listing".into())]).await?;
    Ok(())
}

async fn check_meta(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    match env("DECIBELL_ST_PHASE")?.as_str() {
        "name-storage" => {
            let (window, tab) = open_settings(&cx, TabId::Overview).await?;
            let Tab::Overview(overview) = tab else { bail!("not the Overview tab") };
            let (original, _) = cx.update(|_, cx| community_meta(cx))?;
            let renamed = format!("{original} {}", tag());
            let r2 = renamed.clone();
            let saved = async {
                cx.update(|_, cx| overview.update(cx, |t, cx| t.type_name(&r2, cx)))?;
                cx.update(|_, cx| overview.update(cx, |t, cx| t.save_meta(cx)))?;
                let (r3, o) = (r2.clone(), overview.clone());
                wait_for(&cx, Duration::from_secs(8), "the new name's echo", move |cx| !o.read(cx).saving_meta() && community_meta(cx).0 == r3).await
            }
            .await;
            // Back, whatever happened.
            let o = original.clone();
            let ov = overview.clone();
            wait_for(&cx, Duration::from_secs(8), "the save to settle", move |cx| !ov.read(cx).saving_meta()).await?;
            cx.update(|_, cx| overview.update(cx, |t, cx| t.type_name(&o, cx)))?;
            cx.update(|_, cx| overview.update(cx, |t, cx| t.save_meta(cx)))?;
            let (o, ov) = (original.clone(), overview.clone());
            wait_for(&cx, Duration::from_secs(8), "the original name's echo", move |cx| !ov.read(cx).saving_meta() && community_meta(cx).0 == o).await?;
            let t = saved?;
            cx.log(format_args!("renamed to \"{renamed}\" (echoed in {} ms) and back to \"{original}\"", t.as_millis()));

            // Storage: ↑ in the headroom box steps it half a gigabyte; saved, echoed; put back.
            let (_, tab) = show(&cx, &window, TabId::Storage).await?;
            let Tab::Storage(storage) = tab else { bail!("not the Storage tab") };
            wait_for(&cx, Duration::from_secs(10), "the storage answer", |cx| storage.read(cx).info().is_some()).await?;
            let before = cx.update(|_, cx| storage.read(cx).min_free_text(cx))?;
            let stored = cx.update(|_, cx| storage.read(cx).info().map_or(0, |i| i.min_free_bytes))?;
            click_probe(&cx, "storage-min-free").await?;
            cx.sleep(Duration::from_millis(250)).await;
            cx.keystroke("up")?;
            let after = cx.update(|_, cx| storage.read(cx).min_free_text(cx))?;
            let value = crate::server::storage::parse_float(&before).unwrap_or(0.);
            let want = crate::server::storage::js_number(crate::server::storage::step_number(value, 0.5, true));
            ensure!(after == want, "↑ made {before:?} into {after:?}, expected {want:?}");
            click_probe_now(&cx, "storage-save-save").await?;
            let gib = 1024. * 1024. * 1024.;
            let target = (crate::server::storage::parse_float(&after).unwrap_or(0.) * gib).round() as i64;
            let s2 = storage.clone();
            let saved = wait_for(&cx, Duration::from_secs(8), "the headroom's echo", move |cx| {
                !s2.read(cx).saving() && s2.read(cx).info().is_some_and(|i| i.min_free_bytes == target)
            })
            .await;
            let s2 = storage.clone();
            wait_for(&cx, Duration::from_secs(8), "the save to settle", move |cx| !s2.read(cx).saving()).await?;
            cx.update(|_, cx| storage.update(cx, |t, cx| t.type_min_free(&before, cx)))?;
            click_probe_now(&cx, "storage-save-save").await?;
            let s2 = storage.clone();
            wait_for(&cx, Duration::from_secs(8), "the original headroom", move |cx| {
                !s2.read(cx).saving() && s2.read(cx).info().is_some_and(|i| i.min_free_bytes == stored)
            })
            .await?;
            saved?;
            cx.log(format_args!("headroom: ↑ stepped {before} → {after} GB, saved and echoed; put back to {before}"));
            Ok(())
        }
        "listing" => {
            let (_window, tab) = open_settings(&cx, TabId::Overview).await?;
            let Tab::Overview(overview) = tab else { bail!("not the Overview tab") };
            let listed = |cx: &App| model(cx).community(daily_gaming(), cx).is_some_and(|c| c.meta.read(cx).public_listing);
            let original = cx.update(|_, cx| listed(cx))?;
            for next in [!original, original] {
                click_probe(&cx, "server-public").await?;
                let o = overview.clone();
                let t = wait_for(&cx, Duration::from_secs(8), "the listing's echo", move |cx| !o.read(cx).saving_public() && listed(cx) == next).await?;
                cx.log(format_args!("public listing {} (echoed in {} ms)", if next { "on" } else { "off" }, t.as_millis()));
            }
            ensure!(cx.update(|_, cx| listed(cx))? == original, "the listing isn't back");
            Ok(())
        }
        other => bail!("unknown phase {other:?}"),
    }
}
