//! Discover (`features/servers/ServerBrowseView.tsx`): central's public directory as cards (minus
//! the communities we are in or joining), a search over names and descriptions, and joining by a
//! pasted invite code or link (`decibell://invite/<CODE>`; an older `host:port/code` link brings
//! its own endpoint) or, for an unlisted community, by host and port.
//!
//! The cards are a grid (`repeat(auto-fill, minmax(280px, 360px))`, 16 px gaps) drawn by rows
//! through the kit's windowed scroll area: only the rows near the viewport are built. A row is as
//! tall as its tallest card, whose description (two lines at most) is measured once per list and
//! width.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use decibell_common::Watch;
use decibell_core::api;
use decibell_model::call::offer_retrust;
use decibell_model::error::UiError;
use decibell_model::invite_link::parse_invite_link;
use decibell_model::regional::Regional;
use decibell_model::stores::servers::Server;
use decibell_model::{Model, ServerId, actions};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FontRole, Label, RowHeights, ScrollArea, ScrollHandle, Spinner};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, Image, ImageSource, IntoElement, ObjectFit, Render, SharedString, Task,
    Window, canvas, div, img, linear_color_stop, linear_gradient, prelude::*, px,
};

use crate::ui::{self, HEADING_TRACKING, InputLook};

/// One card: the directory's row and the picture to show (`None`: the letter banner; `Some(None)`:
/// a picture that isn't here yet).
#[derive(Clone)]
struct Card {
    server: Server,
    picture: Option<Option<Arc<Image>>>,
    connecting: bool,
    connected: bool,
}

/// The grid at one width: columns, column width, each row's cards and height.
struct Grid {
    width: f32,
    key: Vec<(ServerId, SharedString)>,
    column_width: f32,
    /// Card indices per row and the row's card height.
    rows: Vec<(Vec<usize>, f32)>,
}

pub struct BrowseView {
    model: Model,
    search: Entity<InputState>,
    invite: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    loading_list: bool,
    connecting: Option<ServerId>,
    error: Option<SharedString>,
    redeeming: bool,
    invite_error: Option<SharedString>,
    redeem_target: Option<ServerId>,
    show_manual_host: bool,
    scroll: ScrollHandle,
    /// Picture versions asked for (`useFetchServerPictureIfMissing`), and decoded pictures.
    asked: HashMap<ServerId, SharedString>,
    pictures: HashMap<ServerId, (SharedString, Option<Arc<Image>>)>,
    grid: Option<Grid>,
    /// The grid's width as last laid out (the rows record it).
    measured: Rc<Cell<f32>>,
    _list: Option<Task<()>>,
    _redeem: Option<Task<()>>,
    _watch: Watch,
}

impl BrowseView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            let state = cx.new(|cx| InputState::single_line(window, cx).placeholder(placeholder));
            cx.subscribe(&state, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::Submit => this.redeem(cx),
                _ => {}
            })
            .detach();
            state
        };
        let search = cx.new(|cx| InputState::single_line(window, cx).placeholder("Search servers..."));
        cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
            if *event == InputEvent::Change {
                cx.notify();
            }
        })
        .detach();
        let invite = input("Have an invite? Paste a code or decibell:// link", window, cx);
        let host = input("host (e.g. 203.0.113.5)", window, cx);
        let port = input("port", window, cx);
        let mut watch = Watch::new();
        watch
            .store(&model.servers, cx)
            .store(&model.connections, cx)
            // A failed redemption's answer (`authError` for the community we are joining).
            .store_if(&model.overlays, cx, |this: &mut Self, overlays, _| {
                let Some(target) = this.redeem_target else { return false };
                let Some(error) = overlays.auth_error.as_ref().filter(|e| e.server == target) else { return false };
                this.invite_error = Some(if error.message.is_empty() { error.error_code.clone() } else { error.message.clone() });
                this.redeeming = false;
                this.redeem_target = None;
                true
            });
        let mut view = Self {
            model,
            search,
            invite,
            host,
            port,
            loading_list: false,
            connecting: None,
            error: None,
            redeeming: false,
            invite_error: None,
            redeem_target: None,
            show_manual_host: false,
            scroll: ScrollHandle::new(),
            asked: HashMap::new(),
            pictures: HashMap::new(),
            grid: None,
            measured: Rc::new(Cell::new(0.)),
            _list: None,
            _redeem: None,
            _watch: watch,
        };
        view.request_list(cx);
        cx.set_global(crate::Current::<BrowseView>(cx.weak_entity()));
        if let Some(search) = crate::screen_state(cx).and_then(|s| s.browse_search.clone()) {
            view.search.update(cx, |s, cx| s.set_value(&search, cx));
        }
        view
    }

    /// `invoke("request_server_list")` on mount: the skeleton shows while it is out.
    fn request_list(&mut self, cx: &mut Context<Self>) {
        if !decibell_model::core::is_running() {
            return;
        }
        self.loading_list = true;
        self._list = Some(cx.spawn(async move |this, cx| {
            let result = api::servers::request_server_list().await;
            if let Err(e) = result {
                log::error!("request_server_list: {e}");
            }
            let _ = this.update(cx, |this, cx| {
                this.loading_list = false;
                cx.notify();
            });
        }));
    }

    /// `handleConnect`: a card's click.
    fn connect(&mut self, server: &Server, cx: &mut Context<Self>) {
        self.connecting = Some(server.id);
        self.error = None;
        cx.notify();
        let weak = cx.weak_entity();
        actions::servers::connect(server.id, server.host.to_string(), server.port, cx, move |result, cx| {
            let _ = weak.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(e.js_string());
                }
                this.connecting = None;
                cx.notify();
            });
        });
    }

    /// `handleRedeemInvite`: the Join button or Enter in one of the invite fields.
    pub fn redeem(&mut self, cx: &mut Context<Self>) {
        let trimmed = self.invite.read(cx).value().trim().to_string();
        if trimmed.is_empty() {
            self.invite_error = Some("Enter an invite code or link".into());
            cx.notify();
            return;
        }
        self.invite_error = None;
        self.model.overlays.update(cx, |o, cx| o.set_auth_error(None, cx));
        let mut host = self.host.read(cx).value().trim().to_string();
        let mut port = ui::parse_int(self.port.read(cx).value());
        let mut code = trimmed.clone();
        let is_link = trimmed.to_lowercase().starts_with("decibell:");
        self.redeeming = true;
        if is_link {
            // Links are code-only; an older host:port link brings its own endpoint and skips
            // the central lookup below.
            match parse_invite_link(&trimmed) {
                None => {
                    self.invite_error = Some("That isn't a valid decibell:// invite link".into());
                    self.redeeming = false;
                    cx.notify();
                    return;
                }
                Some(parsed) => {
                    code = parsed.code.to_string();
                    if let (Some(h), Some(p)) = (parsed.host, parsed.port) {
                        host = h.to_string();
                        port = Some(p as i64);
                    }
                }
            }
        }
        cx.notify();
        let has_host = !host.is_empty() && port.is_some_and(|p| p > 0);
        self._redeem = Some(cx.spawn(async move |this, cx| {
            if !has_host {
                let args = api::servers::ResolveInviteCodeArgs { code: code.clone() };
                match api::servers::resolve_invite_code(args).await.map_err(UiError::from) {
                    Ok(resolved) => {
                        host = resolved.host;
                        port = Some(resolved.port as i64);
                        code = resolved.code;
                    }
                    Err(e) => {
                        let _ = this.update(cx, |this, cx| {
                            this.invite_error = Some(format!("{}. Enter the server's host and port below.", e.js_string()).into());
                            this.redeeming = false;
                            cx.notify();
                        });
                        return;
                    }
                }
            }
            let port = port.unwrap_or(0);
            let server = ServerId::from_wire(&format!("{host}:{port}"));
            let _ = this.update(cx, |this, _| this.redeem_target = Some(server));
            let args = api::servers::RedeemInviteArgs {
                server_id: server.wire(),
                host,
                port: port.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                invite_code: code,
            };
            let result = if decibell_model::core::is_running() {
                api::servers::redeem_invite(args).await.map_err(UiError::from)
            } else {
                Err(UiError::not_running())
            };
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    actions::servers::open_server(server, cx);
                    for field in [&this.invite, &this.host, &this.port] {
                        field.update(cx, |f, cx| f.clear(cx));
                    }
                }
                Err(e) => {
                    if !offer_retrust(&e, None, cx) {
                        this.invite_error = Some(e.js_string());
                    }
                    this.redeeming = false;
                    this.redeem_target = None;
                    cx.notify();
                }
            });
        }));
    }

    /// The decoded picture of `server` at `version`, asking central for it once per version.
    fn picture(&mut self, server: ServerId, version: &SharedString, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        let data = self.model.servers.read(cx).picture(server).cloned();
        let Some(data) = data else {
            if self.asked.get(&server) != Some(version) {
                self.asked.insert(server, version.clone());
                cx.defer(move |cx| actions::servers::fetch_server_picture(server, cx));
            }
            return None;
        };
        if let Some((v, image)) = self.pictures.get(&server)
            && v == version
        {
            return image.clone();
        }
        let image = ui::decode_data_url(&data);
        self.pictures.insert(server, (version.clone(), image.clone()));
        image
    }

    /// The servers shown: the directory minus the ones we are in or joining, filtered by the
    /// search (`filtered`).
    fn filtered(&self, cx: &App) -> Vec<Server> {
        let query = self.search.read(cx).value().to_lowercase();
        let servers = self.model.servers.read(cx);
        let connections = self.model.connections.read(cx);
        servers
            .discoverable()
            .iter()
            .filter(|s| {
                !connections.is_connected(s.id)
                    && !connections.is_pending(s.id)
                    && (s.name.to_lowercase().contains(&query) || s.description.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    /// Lay the grid out at `width` (the cards' row minus its `px-8`): `auto-fill` columns of
    /// 280–360 px with 16 px gaps; a row is its tallest card (the description's lines).
    fn layout(&mut self, cards: &[Card], width: f32, window: &Window, cx: &mut Context<Self>) {
        let key: Vec<(ServerId, SharedString)> = cards.iter().map(|c| (c.server.id, c.server.description.clone())).collect();
        if self.grid.as_ref().is_some_and(|g| g.width == width && g.key == key) {
            return;
        }
        // `auto-fill` counts the tracks at their maximum (360) when it is definite (CSS Grid
        // §7.2.3.2); each track then grows to at most 360 and shrinks to no less than 280.
        let columns = (((width + 16.) / (360. + 16.)).floor() as usize).max(1);
        let column_width = ((width - 16. * (columns - 1) as f32) / columns as f32).clamp(280., 360.);
        let style = ui::text_style(FontRole::Sans, 12., 16., cx);
        let scale = window.scale_factor();
        let rows = cards
            .chunks(columns)
            .enumerate()
            .map(|(r, chunk)| {
                let tallest = chunk
                    .iter()
                    .map(|c| ui::line_count(&c.server.description, &style, column_width - 32. - 2., scale, cx).min(2))
                    .max()
                    .unwrap_or(0);
                ((r * columns..r * columns + chunk.len()).collect(), card_height(tallest))
            })
            .collect();
        self.grid = Some(Grid { width, key, column_width, rows });
    }
}

/// A card's height with `lines` lines of description: border, the 96 px banner, `p-4`, the name
/// (20), `gap-1`, the description, `gap-1`, `mt-0.5` and the member count (16).
fn card_height(lines: usize) -> f32 {
    2. + 96. + 16. + 20. + 4. + 16. * lines as f32 + 4. + 2. + 16. + 16.
}

impl Render for BrowseView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.theme();
        let (bg_mid, bg_dark, bg_primary, border, bright, secondary, muted, error, accent, accent_hover, on_accent) = (
            t.color_bg_mid,
            t.color_bg_dark,
            t.color_bg_primary,
            t.color_border,
            t.color_text_bright,
            t.color_text_secondary,
            t.color_text_muted,
            t.color_error,
            t.color_accent,
            t.color_accent_hover,
            t.color_on_accent,
        );
        let (radius_md, radius_lg, radius_xl) = (t.radius_md, t.radius_lg, t.radius_xl);

        let invite_text = self.invite.read(cx).value().trim().to_string();
        let is_link = invite_text.to_lowercase().starts_with("decibell:");
        let can_join = !self.redeeming && !invite_text.is_empty();

        let search = ui::input_box("browse-search", &self.search, InputLook::new(bg_dark, radius_lg).text_sm().pad(16., 10.).ring(), window, cx);
        let mut invite_look = InputLook::new(bg_primary, radius_md).text_sm().mono();
        invite_look.placeholder_font = FontRole::Sans;
        let invite = ui::input_box("browse-invite", &self.invite, invite_look, window, cx);
        let manual = (!is_link && self.show_manual_host).then(|| {
            let look = InputLook::new(bg_primary, radius_md).text_sm().plain_placeholder();
            div()
                .mt_2()
                .flex()
                .gap_2()
                .child(div().flex_1().child(ui::input_box("browse-host", &self.host, look, window, cx)))
                .child(div().w_24().flex_none().child(ui::input_box("browse-port", &self.port, look, window, cx)))
        });
        let toggle_label = if self.show_manual_host { "Hide host & port" } else { "Unlisted server? Enter host & port" };
        let join_button = div()
            .id("browse-join")
            .children(decibell_shell::probes::probe("browse-join"))
            .relative()
            .flex_none()
            .rounded(radius_md)
            .bg(accent)
            .px_4()
            .py_2()
            .when(can_join, |this| this.cursor_pointer().hover(move |s| s.bg(accent_hover)))
            .when(!can_join, |this| this.opacity(0.5).cursor_not_allowed())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if can_join {
                    this.redeem(cx)
                }
            }))
            .child(Label::new(if self.redeeming { "Joining..." } else { "Join" }).size(14.).line_height(20.).weight(600.).color(on_accent));
        let invite_box = div()
            .mt_5()
            .flex()
            .max_w(px(672.))
            .items_start()
            .gap_2()
            .rounded(radius_xl)
            .border_1()
            .border_color(border)
            .bg(bg_dark)
            .p_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(invite)
                    .children(manual)
                    .child(
                        div()
                            .mt_1p5()
                            .flex()
                            .items_center()
                            .gap_3()
                            .when(!is_link, |this| {
                                this.child(
                                    div()
                                        .id("browse-manual-toggle")
                                        .group("browse-manual-toggle")
                                        .flex_none()
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                            this.show_manual_host = !this.show_manual_host;
                                            cx.notify();
                                        }))
                                        .child(
                                            div()
                                                .text_color(muted)
                                                .group_hover("browse-manual-toggle", move |s| s.text_color(secondary))
                                                .child(Label::new(toggle_label).size(11.).line_height(16.5)),
                                        ),
                                )
                            })
                            .children(self.invite_error.clone().map(|e| {
                                div().flex_1().min_w_0().child(Label::new(e).size(12.).line_height(16.).color(error))
                            })),
                    ),
            )
            .child(join_button);

        let header = div()
            .flex_none()
            .border_b_1()
            .border_color(border)
            .px_8()
            .py_6()
            .child(
                div().mb_1().child(
                    Label::new("Discover Servers").size(18.).line_height(27.).weight(600.).tracking(HEADING_TRACKING).color(bright),
                ),
            )
            .child(div().mb_4().child(Label::new("Browse available community servers or search for one").size(14.).line_height(20.).color(secondary)))
            .child(div().w_full().max_w(px(448.)).child(search))
            .child(invite_box);

        // The cards.
        let filtered = self.filtered(cx);
        let connected: Vec<bool> = {
            let connections = self.model.connections.read(cx);
            filtered.iter().map(|s| connections.is_connected(s.id)).collect()
        };
        let mut cards = Vec::with_capacity(filtered.len());
        for (server, connected) in filtered.into_iter().zip(connected) {
            let version: SharedString = self.model.servers.read(cx).picture_version(server.id).to_string().into();
            let picture = (!version.is_empty()).then(|| self.picture(server.id, &version, cx));
            cards.push(Card { connecting: self.connecting == Some(server.id), connected, picture, server });
        }
        let discoverable_empty = self.model.servers.read(cx).discoverable().is_empty();
        let skeleton = self.loading_list && discoverable_empty;

        // The width the grid is laid out at: as last measured, else the window's less what is
        // around the workspace (an estimate for the first frame).
        let width = match self.measured.get() {
            w if w > 0. => w,
            _ => (f32::from(window.viewport_size().width) - 20. - 16. - 2. - 64.).max(280.),
        };
        let list = if skeleton {
            ScrollArea::new("browse-list").track_scroll(&self.scroll).flex_1().child(skeleton_grid(cx)).into_any_element()
        } else if cards.is_empty() {
            // `filtered.length === 0 && !isLoadingList`: under the (empty) grid, `mt-12`.
            ScrollArea::new("browse-list")
                .track_scroll(&self.scroll)
                .flex_1()
                .child(
                    div().px_8().py_6().child(
                        div().mt_12().flex().justify_center().child(Label::new("No servers found.").size(14.).line_height(20.).color(muted)),
                    ),
                )
                .into_any_element()
        } else {
            self.layout(&cards, width, window, cx);
            let grid = self.grid.as_ref().expect("laid out");
            let count = grid.rows.len();
            let mut offsets = Vec::with_capacity(count + 1);
            let mut top = 0.;
            offsets.push(px(0.));
            for (i, (_, height)) in grid.rows.iter().enumerate() {
                top += if i == 0 { 24. } else { 0. } + height + if i + 1 == count { 24. } else { 16. };
                offsets.push(px(top));
            }
            let rows: Rc<Vec<(Vec<Card>, f32)>> = Rc::new(
                grid.rows.iter().map(|(indices, height)| (indices.iter().map(|i| cards[*i].clone()).collect(), *height)).collect(),
            );
            let column_width = grid.column_width;
            let measured = self.measured.clone();
            let view = cx.entity().downgrade();
            ScrollArea::new("browse-list")
                .track_scroll(&self.scroll)
                .flex_1()
                .windowed(count, RowHeights::Offsets(offsets.into()), move |range, _, cx| {
                    range
                        .map(|i| {
                            let (row, height) = &rows[i];
                            let first = i == 0;
                            let measured = measured.clone();
                            let view = view.clone();
                            div()
                                .relative()
                                .size_full()
                                .px_8()
                                .when(first, |this| this.pt_6())
                                .child(
                                    // The grid's width, for the next layout (a resize, the scrollbar).
                                    canvas(
                                        move |bounds, _, cx| {
                                            let width = f32::from(bounds.size.width);
                                            if (width - measured.get()).abs() > 0.5 {
                                                measured.set(width);
                                                if let Some(view) = view.upgrade() {
                                                    cx.notify(view.entity_id());
                                                }
                                            }
                                        },
                                        |_, _, _, _| {},
                                    )
                                    .absolute()
                                    .left_8()
                                    .right_8()
                                    .h_0(),
                                )
                                .child(
                                    div().flex().gap_4().children(row.iter().map(|card| card_element(card, column_width, *height, cx))),
                                )
                                .into_any_element()
                        })
                        .collect()
                })
                .into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(bg_mid)
            .child(header)
            .children(self.error.clone().map(|e| div().flex_none().px_8().pt_3().child(Label::new(e).size(14.).line_height(20.).color(error))))
            .child(list)
    }
}

/// `ServerBrowseCard`.
fn card_element(card: &Card, width: f32, height: f32, cx: &App) -> AnyElement {
    let t = cx.theme();
    let server = card.server.clone();
    let disabled = card.connected || card.connecting;
    let id = server.id;
    let inner = t.radius_xl - px(1.);
    let banner = match &card.picture {
        Some(picture) => div()
            .h_24()
            .w_full()
            .flex_none()
            .rounded_t(inner)
            .overflow_hidden()
            .children(picture.clone().map(|image| {
                img(ImageSource::Image(image)).size_full().object_fit(ObjectFit::Cover).rounded_t(inner)
            }))
            .into_any_element(),
        None => {
            let base = ui::gradient_base(&server.name, cx);
            let initial: SharedString = server.name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default().into();
            div()
                .h_24()
                .w_full()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_t(inner)
                .bg(linear_gradient(135., linear_color_stop(base, 0.), linear_color_stop(ui::darker(base), 1.)))
                // `text-4xl font-semibold text-white`.
                .child(Label::new(initial).size(36.).line_height(40.).weight(600.).color(gpui::white()))
                .into_any_element()
        }
    };
    let members = Regional::get().group(server.member_count.max(0) as u64);
    div()
        .id(("browse-card", id.get() as u64))
        .relative()
        .children(decibell_shell::probes::probe(format!("browse-card:{}", server.name)))
        .flex()
        .flex_col()
        .flex_none()
        .w(px(width))
        .h(px(height))
        .overflow_hidden()
        .rounded(t.radius_xl)
        .border_1()
        .border_color(t.color_border)
        .bg(t.color_bg_dark)
        .when(disabled, |this| this.opacity(0.6))
        .when(!disabled, |this| {
            let hover = t.color_accent.opacity(0.4);
            this.cursor_pointer().hover(move |s| s.border_color(hover))
        })
        .when(!card.connected && !card.connecting, |this| {
            let server = server.clone();
            this.on_click(move |_: &ClickEvent, _, cx| connect_from_card(server.clone(), cx))
        })
        .child(banner)
        .child(
            div()
                .flex()
                .flex_1()
                .flex_col()
                .gap_1()
                .p_4()
                .child(
                    div()
                        .flex()
                        .items_start()
                        .justify_between()
                        .gap_2()
                        .child(div().min_w_0().child(Label::new(server.name.clone()).size(14.).line_height(20.).weight(600.).color(t.color_text_bright).truncate()))
                        .when(card.connecting, |this| this.child(Spinner::new().size(px(16.)).color(t.color_accent))),
                )
                .child(Label::new(server.description.clone()).size(12.).line_height(16.).color(t.color_text_secondary).clamp(2))
                .child(
                    div()
                        .mt_0p5()
                        .flex()
                        .child(Label::new(SharedString::from(format!("{members} members"))).size(12.).line_height(16.).color(t.color_text_muted))
                        .when(card.connected, |this| {
                            this.child(div().ml_1().child(Label::new("· Connected").size(12.).line_height(16.).color(t.color_success)))
                        }),
                ),
        )
        .into_any_element()
}

/// A card's click goes to the browse view that drew it (the cards are built in the windowed
/// list's callback, which holds no view).
fn connect_from_card(server: Server, cx: &mut App) {
    let Some(view) = crate::current::<BrowseView>(cx) else { return };
    view.update(cx, |this, cx| this.connect(&server, cx));
}

/// The skeleton while the list request is out (`animate-pulse` cards; drawn still here: the
/// request is answered within a frame or two).
fn skeleton_grid(cx: &App) -> impl IntoElement {
    let t = cx.theme();
    div().px_8().py_6().flex().flex_wrap().gap_4().children((1..=6).map(|i| {
        div()
            .id(("browse-skeleton", i as u64))
            .w(px(280.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(t.radius_xl)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_dark)
            .child(div().h_24().w_full().bg(t.color_bg_tertiary))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .child(div().h_4().w_32().rounded(t.radius_sm).bg(t.color_bg_tertiary))
                    .child(div().h_3().w_48().rounded(t.radius_sm).bg(t.color_bg_tertiary))
                    .child(div().h_3().w_20().rounded(t.radius_sm).bg(t.color_bg_tertiary)),
            )
    }))
}

impl BrowseView {
    /// For the screens and self-tests: type into the search box.
    pub fn set_search(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search.update(cx, |s, cx| s.set_value(text, cx));
    }

    /// For the self-tests: paste into the invite box.
    pub fn set_invite(&mut self, text: &str, cx: &mut Context<Self>) {
        self.invite.update(cx, |s, cx| s.set_value(text, cx));
    }

    pub fn invite_error(&self) -> Option<SharedString> {
        self.invite_error.clone()
    }

    /// For the self-tests: the names of the cards the search leaves.
    pub fn shown_names(&self, cx: &App) -> Vec<String> {
        self.filtered(cx).iter().map(|s| s.name.to_string()).collect()
    }

    /// For the self-tests: a code typed with the host and port of an unlisted community (the
    /// manual fields shown, as their toggle shows them).
    pub fn set_manual(&mut self, code: &str, host: &str, port: u16, cx: &mut Context<Self>) {
        self.show_manual_host = true;
        self.invite.update(cx, |s, cx| s.set_value(code, cx));
        self.host.update(cx, |s, cx| s.set_value(host, cx));
        self.port.update(cx, |s, cx| s.set_value(&port.to_string(), cx));
        cx.notify();
    }
}
