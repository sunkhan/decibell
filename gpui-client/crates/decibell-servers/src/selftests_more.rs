//! More of this crate's stack tests (the conventions of `selftests.rs`: a child app per session, a
//! throwaway account for every join and leave, the seeded communities left as found):
//!
//! - `servers-leave-rejoin`: a leave sticks (nothing joins again behind it: the core's
//!   `left_communities`, `MEMBERSHIP_ADDED` echoes of the join included), and a leave followed at
//!   once by a re-join through the invite joins and stays (`forget_leaving`).
//! - `servers-join-manual`: Browse's search filters the cards; an unlisted community (Rustaceans) is
//!   joined by its code with host and port typed; a link with an unknown code opens the join dialog
//!   in its invalid state and Cancel closes it.
//! - `welcome-gating`: in Rustaceans (whose @everyone has neither Create Invite nor Manage
//!   Channels) bob makes an empty channel; alice opens it and sees the banner without either action.
//! - `server-owner-leave`: Server Settings offers the owner no Leave; the menu's Leave is refused by
//!   the community with its message (the client hides the tile at once, as the renderer does).

use super::*;

const RUSTACEANS: &str = "Rustaceans";

pub(super) fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "servers-leave-rejoin",
            about: "throwaway account: after a leave nothing joins again by itself; a leave followed at once by a re-join through the \
                    invite joins and stays",
            run: |cx| Box::pin(throwaway_swept(cx, "servers-leave-rejoin", DESIGN_LAB, "carol")),
        },
        SelfTest {
            name: "servers-join-manual",
            about: "throwaway account: Browse's search filters the cards; the unlisted Rustaceans joined by code with host and port; \
                    an unknown code's join dialog says so, Cancel closes it",
            run: |cx| Box::pin(throwaway_swept(cx, "servers-join-manual", RUSTACEANS, "bob")),
        },
        SelfTest {
            name: "welcome-gating",
            about: "Rustaceans: bob makes an empty channel; alice (no Create Invite, no Manage Channels there) sees its banner with neither action",
            run: |cx| Box::pin(welcome_gating(cx)),
        },
        SelfTest {
            name: "server-owner-leave",
            about: "carol, Design Lab: Server Settings shows the owner no Leave; the menu's Leave is refused by the community with its message",
            run: |cx| Box::pin(carol(cx, "server-owner-leave")),
        },
    ]
}

pub(super) fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "servers-leave-rejoin", about: "(in the app) leave, rejoin", run: |cx| Box::pin(check_leave_rejoin(cx)) },
        SelfTest { name: "servers-join-manual", about: "(in the app) search, host and port, a bad code", run: |cx| Box::pin(check_join_manual(cx)) },
        SelfTest { name: "welcome-host", about: "(in the app) bob's empty channel", run: |cx| Box::pin(check_welcome_host(cx)) },
        SelfTest { name: "welcome-member", about: "(in the app) alice's view of it", run: |cx| Box::pin(check_welcome_member(cx)) },
        SelfTest { name: "server-owner-leave", about: "(in the app) the owner's leave", run: |cx| Box::pin(check_owner_leave(cx)) },
        SelfTest { name: "servers-sweep-in", about: "(in the app) remove throwaway members of a community", run: |cx| Box::pin(check_sweep_in(cx)) },
    ]
}

/// A throwaway child; afterwards the community's owner removes these tests' throwaway accounts
/// from `community`.
async fn throwaway_swept(cx: TestCx, check: &'static str, community_name: &'static str, owner: &'static str) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let env = throwaway_env("");
    cx.log(format_args!("throwaway account {}", env[0].1));
    let profile = Profile::new(check)?;
    let result = spawn(check, None, &stack, &profile.0, &env)?.finish(&cx, CHILD_LIMIT).await;
    let sweep_profile = Profile::new("servers-sweep-in")?;
    let swept = spawn("servers-sweep-in", Some(owner), &stack, &sweep_profile.0, &[("DECIBELL_TEST_COMMUNITY", community_name.to_string())])?
        .finish(&cx, Duration::from_secs(120))
        .await;
    result?;
    swept
}

async fn check_sweep_in(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(&env("DECIBELL_TEST_COMMUNITY")?)?.id;
    wait_for(&cx, Duration::from_secs(30), "the community", move |cx| model(cx).connections.read(cx).is_connected(server)).await?;
    cx.sleep(Duration::from_secs(2)).await;
    sweep_members(&cx, server).await?;
    log_out(&cx).await
}

fn present(server: ServerId, cx: &App) -> bool {
    let m = model(cx);
    let c = m.connections.read(cx);
    c.is_connected(server) || c.is_pending(server)
}

// ── servers-leave-rejoin ────────────────────────────────────────────────────────────────────

async fn check_leave_rejoin(cx: TestCx) -> Result<()> {
    throwaway_sign_in(&cx).await?;
    // The server menu closes when the window stops being the active one; other windows share
    // this desktop.
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    let design = community(DESIGN_LAB)?;
    let server = design.id;
    join_by_code(&cx, &design).await?;
    open_community(&cx, server).await?;
    // 1. A leave sticks: central's MEMBERSHIP_ADDED for the join (sent to every device, this one
    // included, a moment after it) must not take the account back in, nor may a reconnect.
    leave_by_menu(&cx, server).await?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        ensure!(!cx.update(move |_, cx| present(server, cx))?, "back in the community {} ms after the leave", started.elapsed().as_millis());
        cx.sleep(Duration::from_millis(100)).await;
    }
    cx.log("left: still out 8 s later (no rejoin behind the leave)");
    // 2. Join, leave, and at once join again through the invite.
    join_by_code(&cx, &design).await?;
    open_community(&cx, server).await?;
    settle_drawn(&cx).await?;
    click_probe(&cx, "server-menu-button").await?;
    wait_for(&cx, Duration::from_secs(5), "the server menu", |_| probes::flag_set("server-menu-open")).await?;
    click_probe(&cx, "server-menu-leave").await?;
    wait_for(&cx, Duration::from_secs(5), "the confirmation", |cx| model(cx).overlays.read(cx).is_open(modal::LEAVE_SERVER_CONFIRM)).await?;
    cx.sleep(Duration::from_millis(350)).await;
    cx.keystroke("tab")?;
    cx.keystroke("tab")?;
    cx.keystroke("enter")?;
    // The leave's answer: the community closes the session.
    wait_for(&cx, Duration::from_secs(15), "the leave's answer (Browse)", |cx| model(cx).nav.read(cx).active_view == ActiveView::Browse).await?;
    let (host, port, code) = (design.host.clone(), design.port, design.invite.clone());
    let failed: Rc<std::cell::RefCell<Option<String>>> = Rc::default();
    let sink = failed.clone();
    cx.update(move |_, cx| {
        actions::servers::redeem_invite(host, port, code, cx, move |r, _| {
            if let Err(e) = r {
                *sink.borrow_mut() = Some(e.to_string());
            }
        })
    })?;
    let f = failed.clone();
    let t = wait_for(&cx, Duration::from_secs(30), "the re-join", move |cx| f.borrow().is_some() || model(cx).connections.read(cx).is_connected(server)).await?;
    if let Some(e) = failed.borrow().clone() {
        bail!("re-joining right after the leave: {e}");
    }
    cx.log(format_args!("re-joined through the invite {} ms after the leave's answer", t.as_millis()));
    // It stays: past the core's 3 s wait on MEMBERSHIP_ADDED, the tile and our member row there.
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(8) {
        ensure!(cx.update(move |_, cx| model(cx).connections.read(cx).is_connected(server))?, "dropped out again {} ms after the re-join", started.elapsed().as_millis());
        cx.sleep(Duration::from_millis(100)).await;
    }
    wait_for(&cx, Duration::from_secs(10), "our member row", move |cx| {
        model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).member(me).is_some())
    })
    .await?;
    settle_drawn(&cx).await?;
    wait_probe(&cx, &format!("server-tile:{}", server.get())).await?;
    cx.log("still a member 8 s later: the tile and our member row are there");
    leave_quietly(&cx, server).await?;
    log_out(&cx).await
}

// ── servers-join-manual ─────────────────────────────────────────────────────────────────────

async fn check_join_manual(cx: TestCx) -> Result<()> {
    throwaway_sign_in(&cx).await?;
    let rust = community(RUSTACEANS)?;
    let server = rust.id;
    // Browse's search: the directory filtered as typed.
    cx.update(|_, cx| model(cx).nav.update(cx, |n, cx| n.set_active_view(ActiveView::Browse, cx)))?;
    wait_for(&cx, Duration::from_secs(20), "the directory", |cx| {
        let s = model(cx).servers.read(cx).discoverable().iter().map(|s| s.name.to_string()).collect::<Vec<_>>();
        s.iter().any(|n| n == DESIGN_LAB) && s.iter().any(|n| n == "Daily Gaming")
    })
    .await?;
    settle_drawn(&cx).await?;
    wait_probe(&cx, "browse-card:Daily Gaming").await?;
    cx.update(|_, cx| {
        if let Some(view) = crate::current::<BrowseView>(cx) {
            view.update(cx, |v, cx| v.set_search("design", cx));
        }
    })?;
    wait_for(&cx, Duration::from_secs(5), "the filtered cards", |cx| {
        crate::current::<BrowseView>(cx).is_some_and(|v| {
            let shown = v.read(cx).shown_names(cx);
            shown.iter().any(|n| n == DESIGN_LAB) && !shown.iter().any(|n| n == "Daily Gaming")
        })
    })
    .await?;
    cx.log("searching \"design\" leaves Design Lab and hides Daily Gaming");
    cx.update(|_, cx| {
        if let Some(view) = crate::current::<BrowseView>(cx) {
            view.update(cx, |v, cx| v.set_search("", cx));
        }
    })?;

    // The unlisted community by its code, host and port (no central lookup).
    cx.update(|_, cx| {
        if let Some(view) = crate::current::<BrowseView>(cx) {
            view.update(cx, |v, cx| v.set_manual(&rust.invite, &rust.host, rust.port, cx));
        }
    })?;
    settle_drawn(&cx).await?;
    click_probe(&cx, "browse-join").await?;
    // Joined by host and port, the connection opens under the provisional "host:port" id; the core
    // moves it onto central's id only when central has that address on record (the directory or
    // an invite resolution), which an unlisted community joined this way never gets: it stays
    // provisional until the next sign-in (the shared core's rule, Electron's too).
    let provisional = ServerId::from_wire(&format!("{}:{}", rust.host, rust.port));
    let t = wait_for(&cx, Duration::from_secs(30), "joining Rustaceans by host and port", move |cx| {
        let m = model(cx);
        let (c, active) = (m.connections.read(cx), m.nav.read(cx).active_server);
        (c.is_connected(server) && active == Some(server)) || (c.is_connected(provisional) && active == Some(provisional))
    })
    .await?;
    let joined = if cx.update(move |_, cx| model(cx).connections.read(cx).is_connected(server))? { server } else { provisional };
    cx.log(format_args!(
        "joined the unlisted Rustaceans by code + {}:{} in {} ms ({})",
        rust.host,
        rust.port,
        t.as_millis(),
        if joined == server { "under central's id" } else { "under the provisional host:port id" }
    ));
    leave_by_menu(&cx, joined).await?;

    // A link with an unknown code: the dialog resolves it as invalid; Cancel closes it.
    ensure!(cx.update(|_, cx| crate::open_invite_link("decibell://invite/ZZZZ9999ZZ", cx))?, "the link didn't parse");
    wait_for(&cx, Duration::from_secs(15), "the dialog's invalid state", |cx| {
        crate::current::<crate::join::JoinOverlay>(cx).is_some_and(|j| j.read(cx).state(cx).invalid)
    })
    .await?;
    settle_drawn(&cx).await?;
    let error = cx.update(|_, cx| crate::current::<crate::join::JoinOverlay>(cx).and_then(|j| j.read(cx).shown_error(cx)))?;
    ensure!(error.as_deref() == Some("This invite is unknown, expired, or used up."), "the dialog says {error:?}");
    ensure!(probes::bounds("join-accept").is_none() || !cx.update(|_, cx| crate::current::<crate::join::JoinOverlay>(cx).is_some_and(|j| j.read(cx).can_accept(cx)))?, "Accept is offered for an invalid code");
    click_probe(&cx, "join-cancel").await?;
    wait_for(&cx, Duration::from_secs(5), "the dialog to close", |cx| model(cx).servers.read(cx).pending_invite.is_none()).await?;
    cx.log("an unknown code: \"This invite is unknown, expired, or used up.\"; Cancel closed the dialog");
    log_out(&cx).await
}

// ── welcome-gating ──────────────────────────────────────────────────────────────────────────

async fn welcome_gating(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let host_profile = Profile::new("welcome-host")?;
    let member_profile = Profile::new("welcome-member")?;
    let go = host_profile.0.join("seen");
    let mut host = spawn("welcome-host", Some("bob"), &stack, &host_profile.0, &[("DECIBELL_TEST_GO", go.display().to_string())])?;
    if let Err(e) = host.wait_mark(&cx, "channel", Duration::from_secs(120)).await {
        if host.no_dev_login() {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            return Ok(());
        }
        return Err(e);
    }
    let channel = host.mark_value("channel").context("the channel")?;
    let member = spawn("welcome-member", Some("alice"), &stack, &member_profile.0, &[("DECIBELL_TEST_CHANNEL", channel)])?;
    let seen = member.finish(&cx, Duration::from_secs(120)).await;
    std::fs::write(&go, b"go")?;
    host.finish(&cx, Duration::from_secs(120)).await?;
    seen
}

async fn check_welcome_host(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(RUSTACEANS)?.id;
    wait_for(&cx, Duration::from_secs(30), "Rustaceans", move |cx| model(cx).connections.read(cx).is_connected(server)).await?;
    wait_for(&cx, Duration::from_secs(10), "its channels", move |cx| !channels_of(server, cx).is_empty()).await?;
    clean_up(&cx, server).await?;
    let id = api_create(&cx, server, &format!("{}-gated", tag()), "text", None).await?;
    mark(format_args!("channel {id}"));
    let go = PathBuf::from(env("DECIBELL_TEST_GO")?);
    let started = Instant::now();
    while !go.exists() && started.elapsed() < Duration::from_secs(150) {
        cx.sleep(Duration::from_millis(200)).await;
    }
    api_delete(&cx, server, &id).await?;
    log_out(&cx).await
}

async fn check_welcome_member(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(RUSTACEANS)?.id;
    let channel = ChannelId::from(env("DECIBELL_TEST_CHANNEL")?);
    let c = channel.clone();
    wait_for(&cx, Duration::from_secs(30), "the channel", move |cx| channels_of(server, cx).iter().any(|x| x.id == c)).await?;
    let (invite, manage) = cx.update(|_, cx| {
        let m = model(cx);
        (
            m.has_permission(server, decibell_model::stores::permissions::perm::CREATE_INVITE, cx)
                || m.has_permission(server, decibell_model::stores::permissions::perm::MANAGE_INVITES, cx),
            m.channel_permission(server, &channel, decibell_model::stores::permissions::perm::MANAGE_CHANNELS, cx),
        )
    })?;
    ensure!(!invite && !manage, "alice may invite ({invite}) or manage the channel ({manage}) in Rustaceans: the premise changed");
    // As a person opens it: the community's tile, then the channel's row.
    open_community(&cx, server).await?;
    let row = cx.update(move |_, cx| channels_of(server, cx).into_iter().find(|c| c.id == channel))?.context("the row")?;
    click_probe(&cx, &row_key(&row)).await?;
    wait_for(&cx, Duration::from_secs(15), "the welcome banner", |_| probes::flag_set("welcome-shown")).await?;
    settle_drawn(&cx).await?;
    ensure!(!probes::flag_set("welcome-invite-shown") && !probes::flag_set("welcome-manage-shown"), "the banner offers an action alice may not take");
    ensure!(probes::bounds("welcome-invite").is_none() && probes::bounds("welcome-manage").is_none(), "an action button was laid out");
    cx.log("a member without Create Invite or Manage Channels: the banner, with neither action");
    log_out(&cx).await
}

// ── server-owner-leave ──────────────────────────────────────────────────────────────────────

async fn check_owner_leave(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    // The server menu closes when the window stops being the active one; other windows share
    // this desktop.
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    ensure!(cx.update(move |_, cx| model(cx).is_owner(server, cx))?, "carol doesn't own Design Lab: the premise changed");
    // Server Settings: the owner gets no Leave Server.
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.open_modal(modal::SERVER_SETTINGS, cx)))?;
    wait_probe(&cx, "server-tab:members").await?;
    settle_drawn(&cx).await?;
    ensure!(probes::bounds("server-leave").is_none(), "Server Settings offers the owner Leave Server");
    cx.log("Server Settings: no Leave Server for the owner");
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Server Settings to close", |cx| !model(cx).overlays.read(cx).is_open(modal::SERVER_SETTINGS)).await?;
    // The menu's Leave: the community refuses it and says why.
    settle_drawn(&cx).await?;
    click_probe(&cx, "server-menu-button").await?;
    wait_for(&cx, Duration::from_secs(5), "the server menu", |_| probes::flag_set("server-menu-open")).await?;
    click_probe(&cx, "server-menu-leave").await?;
    wait_for(&cx, Duration::from_secs(5), "the confirmation", |cx| model(cx).overlays.read(cx).is_open(modal::LEAVE_SERVER_CONFIRM)).await?;
    cx.sleep(Duration::from_millis(350)).await;
    cx.keystroke("tab")?;
    cx.keystroke("tab")?;
    cx.keystroke("enter")?;
    let refusal = "The server owner cannot leave their own server.";
    wait_for(&cx, Duration::from_secs(15), "the refusal's toast", move |cx| model(cx).toasts.read(cx).toasts().iter().any(|t| t.title == refusal)).await?;
    cx.log(format_args!("the menu's Leave: refused, toast \"{refusal}\""));
    // Still the owner and still a member: the session is alive (only the local tile went, as in
    // the renderer, until the next sign-in).
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    ensure!(cx.update(move |_, cx| model(cx).community(server, cx).is_some_and(|c| c.meta.read(cx).owner == me))?, "no longer the owner");
    log_out(&cx).await
}
