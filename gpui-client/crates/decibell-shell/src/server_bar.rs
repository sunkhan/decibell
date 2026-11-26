//! The server bar (`features/servers/ServerBar.tsx`): the home button, the unread-DM tiles (a
//! slot: they need the DM store), one tile per community the account is connected to or
//! auto-rejoining, and the browse button. The renderer has no tile context menu and no drag to
//! reorder (leaving is in the channel sidebar's server menu).

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine as _;
use decibell_common::Watch;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{Model, ServerId, actions};
use decibell_ui::identity::ramp_index;
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, title_tooltip};
use gpui::{
    AnyElement, App, ClickEvent, Context, Hsla, Image, ImageFormat, ImageSource, IntoElement, ObjectFit, Render,
    SharedString, Window, div, img, linear_color_stop, linear_gradient, prelude::*, px,
};

use crate::glow::{active_tile_glow, glow_svg};
use crate::slots::{PaneHost, Slot};

/// `TILE_WIDTH` × `TILE_HEIGHT` (`serverTileDimensions.ts`); the DM tiles and the home / browse
/// buttons share the height.
pub const TILE_WIDTH: f32 = 128.;
pub const TILE_HEIGHT: f32 = 44.;
/// `h-[64px]`.
pub const BAR_HEIGHT: f32 = 64.;

/// A community picture, decoded from central's `data:` URL once per version.
struct Picture {
    version: SharedString,
    image: Option<Arc<Image>>,
}

pub struct ServerBar {
    /// What of the navigation the bar shows: the view and the active community.
    nav: (ActiveView, Option<ServerId>),
    /// Picture versions asked for (the renderer's module-level `inflightFetches`).
    asked: HashMap<ServerId, SharedString>,
    pictures: HashMap<ServerId, Picture>,
    /// The glow's SVG for the theme's tile radius.
    glow: Option<(f32, Vec<u8>)>,
    unread_dms: PaneHost,
    _watch: Watch,
    _activation: gpui::Subscription,
}

impl ServerBar {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.servers, cx).store(&model.connections, cx).store_if(&model.nav, cx, |this, nav, _| {
            let next = (nav.active_view, nav.active_server);
            std::mem::replace(&mut this.nav, next) != next
        });
        let nav = model.nav.read(cx);
        Self {
            nav: (nav.active_view, nav.active_server),
            asked: HashMap::new(),
            pictures: HashMap::new(),
            glow: None,
            unread_dms: PaneHost::default(),
            _watch: watch,
            // The glow breathes only while the window is focused.
            _activation: cx.observe_window_activation(window, |_, _, cx| cx.notify()),
        }
    }

    /// The decoded picture of `server` at `version`, fetching it (once per version) when central
    /// hasn't sent the bytes yet.
    fn picture(&mut self, server: ServerId, version: &SharedString, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        let model = Model::global(cx).clone();
        let data = model.servers.read(cx).picture(server).cloned();
        let Some(data) = data else {
            if self.asked.get(&server) != Some(version) {
                self.asked.insert(server, version.clone());
                cx.defer(move |cx| actions::servers::fetch_server_picture(server, cx));
            }
            return None;
        };
        if let Some(picture) = self.pictures.get(&server)
            && picture.version == *version
        {
            return picture.image.clone();
        }
        let image = decode_data_url(&data).map(Arc::new);
        if image.is_none() {
            log::warn!("[server bar] community {server}'s picture isn't a data URL of an image");
        }
        self.pictures.insert(server, Picture { version: version.clone(), image: image.clone() });
        image
    }

    fn glow_data(&mut self, radius: f32) -> &[u8] {
        if self.glow.as_ref().is_none_or(|(r, _)| *r != radius) {
            self.glow = Some((radius, glow_svg(TILE_WIDTH, TILE_HEIGHT, radius)));
        }
        &self.glow.as_ref().expect("just set").1
    }
}

/// `data:image/<type>;base64,<bytes>` → an image GPUI decodes when drawn.
fn decode_data_url(url: &str) -> Option<Image> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    let format = match mime {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        _ => return None,
    };
    let bytes = base64::engine::general_purpose::STANDARD.decode(payload).ok()?;
    Some(Image::from_bytes(format, bytes))
}

/// A square button of the bar (home, browse): `TILE_HEIGHT` square.
fn square(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .size(px(TILE_HEIGHT))
        .items_center()
        .justify_center()
        .cursor_pointer()
}

impl Render for ServerBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let chrome = crate::ink::chrome(cx);
        let (view, active_server) = self.nav;
        let breathing = window.is_window_active();
        log::debug!("[server bar] render, window active: {breathing}");

        // The tiles: memberships connected or still connecting, in the order central listed them.
        let tiles: Vec<(ServerId, SharedString, bool, SharedString)> = {
            let servers = model.servers.read(cx);
            let connections = model.connections.read(cx);
            servers
                .servers()
                .iter()
                .filter(|s| connections.is_connected(s.id) || connections.is_pending(s.id))
                .map(|s| (s.id, s.name.clone(), connections.is_pending(s.id), servers.picture_version(s.id).to_string().into()))
                .collect()
        };

        let home_active = view == ActiveView::Home;
        let home = square("server-bar-home")
            .relative()
            .children(crate::probes::probe("home"))
            .rounded(chrome.radius_sm)
            .bg(if home_active { chrome.color_accent } else { chrome.color_surface_active })
            .when(!home_active, |this| this.hover(|s| s.bg(chrome.color_accent)))
            .group("server-bar-home")
            .tooltip(title_tooltip("Home"))
            .on_click(|_: &ClickEvent, _, cx| actions::servers::go_home(cx))
            .child(
                Icon::new(IconName::ShellHome)
                    .size(px(22.))
                    .color(if home_active { chrome.color_on_accent } else { chrome.color_text_secondary })
                    .group_hover("server-bar-home", chrome.color_on_accent),
            );

        let radius = f32::from(chrome.radius_md);
        let mut tile_elements: Vec<AnyElement> = Vec::with_capacity(tiles.len());
        for (id, name, pending, version) in tiles {
            // Active only while the user looks at it (server or voice view): on home / browse /
            // dm the active community is remembered but not shown as active.
            let active = active_server == Some(id) && matches!(view, ActiveView::Server | ActiveView::Voice);
            let picture = if version.is_empty() { None } else { Some(self.picture(id, &version, cx)) };
            let glow = (active && !pending).then(|| self.glow_data(radius).to_vec());
            tile_elements.push(tile(id, name, pending, active, picture, glow, breathing, cx));
        }
        let any_tiles = !tile_elements.is_empty();

        let browse_active = view == ActiveView::Browse;
        let browse = square("server-bar-browse")
            .rounded(chrome.radius_md)
            .group("server-bar-browse")
            .map(|this| {
                if browse_active {
                    this.bg(chrome.color_success)
                } else {
                    this.border(px(1.5))
                        .border_dashed()
                        .border_color(chrome.color_text_muted)
                        .hover(|s| s.border_color(chrome.color_accent).bg(chrome.color_accent_soft))
                }
            })
            .tooltip(title_tooltip("Browse servers"))
            .on_click({
                let nav = model.nav.clone();
                move |_: &ClickEvent, _, cx| nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Browse, cx))
            })
            .child(
                Icon::new(IconName::SettingsPlus)
                    .size(px(20.))
                    .stroke_width(2.5)
                    .color(if browse_active { chrome.color_on_accent } else { chrome.color_text_muted })
                    .group_hover("server-bar-browse", if browse_active { chrome.color_on_accent } else { chrome.color_accent }),
            );

        let tiles = |_: &mut Window, cx: &mut App| -> gpui::AnyView { cx.new(crate::dm_rows::UnreadDmTiles::new).into() };
        let unread = self.unread_dms.view(Slot::UnreadDms, window, cx, Some(&tiles));

        div()
            .relative()
            .flex()
            .w_full()
            .h(px(BAR_HEIGHT))
            .items_center()
            .bg(chrome.color_bg_darkest)
            .child(div().flex().flex_none().items_center().px_2().child(home))
            .child(div().flex_none().h_7().w(px(1.)).bg(chrome.color_border_divider))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .child(unread)
                    .children(tile_elements)
                    .when(any_tiles, |this| {
                        this.child(div().mx_1().flex_none().h_6().w(px(1.)).bg(chrome.color_border_divider))
                    })
                    .child(browse),
            )
    }
}

/// The 20 × 3 px accent bar 9 px under an active tile, centred (`-bottom-[9px] left-1/2 h-[3px]
/// w-5 -translate-x-1/2 rounded-t`).
fn active_indicator(accent: Hsla) -> impl IntoElement + use<> {
    div()
        .absolute()
        .bottom(px(-9.))
        .left(px((TILE_WIDTH - 20.) / 2.))
        .w_5()
        .h(px(3.))
        .rounded_t(px(4.))
        .bg(accent)
}

/// One community's tile (`ServerTile`): the letter branch (a gradient square with the initial and
/// the name) or the picture branch (the picture, dimmed with the name over it until hovered or
/// active). Pending tiles ("Connecting…") are inert.
#[allow(clippy::too_many_arguments)]
fn tile(
    id: ServerId,
    name: SharedString,
    pending: bool,
    active: bool,
    picture: Option<Option<Arc<Image>>>,
    glow: Option<Vec<u8>>,
    breathing: bool,
    cx: &App,
) -> AnyElement {
    let chrome = crate::ink::chrome(cx);
    let group: SharedString = format!("server-tile-{}", id.get()).into();
    let title: SharedString = if pending { "Connecting…".into() } else { name.clone() };
    let mut button = div()
        .id(("server-tile", id.get() as u64))
        .group(group.clone())
        .relative()
        .flex()
        .size_full()
        .items_center()
        .rounded(chrome.radius_md)
        .px_3()
        .tooltip(title_tooltip(title))
        .children(crate::probes::probe(format!("server-tile:{}", id.get())))
        .when(!pending, |this| {
            this.cursor_pointer()
                .on_click(move |_: &ClickEvent, _, cx| actions::servers::open_server(id, cx))
        });

    button = match picture {
        // The picture branch: the picture under a 45 % black veil and the name, both fading out
        // on hover and while active.
        Some(image) => {
            let veiled = !active;
            let mut face = div().absolute().inset_0().rounded(chrome.radius_md).overflow_hidden();
            if let Some(image) = image {
                face = face.child(
                    img(ImageSource::Image(image)).size_full().object_fit(ObjectFit::Cover).rounded(chrome.radius_md),
                );
            }
            button
                .justify_center()
                .when(pending, |this| this.opacity(0.6))
                .child(face)
                .when(veiled, |this| {
                    // A literal of the renderer: `bg-black/45` over the picture.
                    let veil = div().absolute().inset_0().rounded(chrome.radius_md).bg(gpui::black().opacity(0.45));
                    this.child(if pending { veil } else { veil.group_hover(group.clone(), |s| s.opacity(0.)) })
                })
                .when(veiled, |this| {
                    let label = div()
                        .relative()
                        .max_w_full()
                        .child(
                            Label::new(name.clone())
                                .font(FontRole::Tile)
                                .size(13.)
                                .weight(600.)
                                // `text-white` over the veil.
                                .color(gpui::white())
                                .truncate(),
                        );
                    this.child(if pending { label } else { label.group_hover(group.clone(), |s| s.opacity(0.)) })
                })
                .when(active && !pending, |this| this.child(active_indicator(chrome.color_accent)))
        }
        // The letter branch.
        None => {
            let (bg, text) = if pending {
                (chrome.color_surface_hover, chrome.color_text_muted)
            } else if active {
                (chrome.color_accent, chrome.color_on_accent)
            } else {
                (chrome.color_surface_active, chrome.color_text_secondary)
            };
            let base = {
                let t = &cx.theme().chrome;
                [t.color_av_1, t.color_av_2, t.color_av_3, t.color_av_4, t.color_av_5, t.color_av_6, t.color_av_7, t.color_av_8]
                    [ramp_index(&name).clamp(1, 8) - 1]
            };
            let initial: SharedString =
                name.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default().into();
            button
                .gap_2()
                .bg(bg)
                .when(pending, |this| this.opacity(0.6))
                .when(!pending && !active, |this| this.hover(|s| s.bg(chrome.color_accent)))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .size_5()
                        .items_center()
                        .justify_center()
                        .rounded(chrome.radius_sm)
                        .bg(linear_gradient(135., linear_color_stop(base, 0.), linear_color_stop(darker(base), 1.)))
                        .child(Label::new(initial).size(11.).line_height(16.5).weight(600.).color(chrome.color_av_fg)),
                )
                .child(
                    // The label inherits its colour, so the tile's hover can recolour it.
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(text)
                        .when(!pending && !active, |this| {
                            let on_accent = chrome.color_on_accent;
                            this.group_hover(group.clone(), move |s| s.text_color(on_accent))
                        })
                        .child(Label::new(name).font(FontRole::Tile).size(13.).weight(600.).truncate()),
                )
                .when(active && !pending, |this| this.child(active_indicator(chrome.color_accent)))
        }
    };

    div()
        .relative()
        .flex_none()
        .w(px(TILE_WIDTH))
        .h(px(TILE_HEIGHT))
        .when_some(glow, |this, data| {
            this.child(active_tile_glow(&data, TILE_WIDTH, TILE_HEIGHT, chrome.color_accent, breathing))
        })
        .child(button)
        .into_any_element()
}

/// `color-mix(in srgb, c 72%, #000)`.
fn darker(color: Hsla) -> Hsla {
    let mut rgba: gpui::Rgba = color.into();
    rgba.r *= 0.72;
    rgba.g *= 0.72;
    rgba.b *= 0.72;
    rgba.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_come_as_data_urls() {
        // A 1×1 PNG.
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";
        let image = decode_data_url(url).unwrap();
        assert_eq!(image.format, ImageFormat::Png);
        assert!(decode_data_url("https://example.test/a.png").is_none());
        assert!(decode_data_url("data:text/plain;base64,aGk=").is_none());
    }
}
