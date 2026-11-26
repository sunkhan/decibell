//! Layers in pixels: the `kit-layers` screen (a modal with an open select, a context menu and a
//! tooltip inside it) through GPUI's real renderer, headless. Each of the three must be drawn
//! above the modal's backdrop (`decibell_ui::layer`): a pixel of each, outside the dialog card, is
//! the popup's own colour. Drawn under the backdrop (GPUI's order by bare priority, which put a
//! select's menu at 3 under a modal at 10) it would be darkened to 35 %.
//!
//! Needs a GPU (or a software Vulkan / GL driver): without one the test says so and passes.

#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use std::sync::Arc;

use decibell_theme::ThemeId;
use decibell_ui::kit::KitLayers;
use decibell_ui::{ActiveTheme as _, AssumeClientDecorations, visible_window_bounds};
use gpui::{AnyWindowHandle, AppContext as _, HeadlessAppContext, Hsla, Rgba, px, size};

fn rgb(color: Hsla) -> [u8; 3] {
    let c: Rgba = color.into();
    [(c.r * 255.).round() as u8, (c.g * 255.).round() as u8, (c.b * 255.).round() as u8]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (*x as i16 - y as i16).abs() <= 4)
}

#[test]
fn what_opens_in_a_modal_is_drawn_above_it() {
    let platform = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter"));
    let mut cx = HeadlessAppContext::with_platform(platform, Arc::new(decibell_ui::Assets), gpui_platform::current_headless_renderer);
    // A light theme: the backdrop's 65 % black is far from every popup colour there.
    cx.update(|cx| {
        cx.set_global(AssumeClientDecorations);
        decibell_ui::init(cx, ThemeId::GraphiteLight)
    })
    .expect("the kit's init");
    cx.update(|cx| cx.set_reduce_motion(true));
    // A headless window is never active, and open menus close when their window isn't.
    cx.update(|cx| decibell_harness::focus::hold_window_focus(Some(true), cx));
    let outer = decibell_ui::outer_window_size(size(px(1280.), px(800.)));
    let mut fixture = None;
    let window = cx.open_window(outer, |_, cx| {
        let view = cx.new(KitLayers::new);
        fixture = Some(view.clone());
        view
    });
    let window: AnyWindowHandle = match window {
        Ok(window) => window.into(),
        Err(error) => {
            eprintln!("skipped: no headless renderer here ({error:#})");
            return;
        }
    };
    let fixture = fixture.expect("the fixture");
    // Two frames: the select opens in the first (`initially_open`), its menu is placed in the next.
    for _ in 0..3 {
        cx.update_window(window, |_, window, _| window.refresh()).expect("refresh");
        cx.run_until_parked();
    }
    let image = match cx.capture_screenshot(window) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("skipped: the headless renderer cannot draw here ({error:#})");
            return;
        }
    };
    if let Ok(path) = std::env::var("DECIBELL_LAYERS_PNG") {
        image.save(&path).expect("saving the capture");
    }
    let (origin, scale, card, trigger, menu_bg, tooltip_bg, page) = cx
        .update_window(window, |_, window, cx| {
            let theme = cx.theme();
            let f = fixture.read(cx);
            (
                visible_window_bounds(window, cx).origin,
                window.scale_factor(),
                f.card.get(),
                f.trigger.get(),
                rgb(theme.color_bg_light),
                rgb(theme.color_bg_darkest),
                rgb(theme.color_bg_primary),
            )
        })
        .expect("reading the fixture");
    let pixel = |x: gpui::Pixels, y: gpui::Pixels| {
        let p = image.get_pixel((f32::from(x) * scale) as u32, (f32::from(y) * scale) as u32).0;
        [p[0], p[1], p[2]]
    };
    // The backdrop over the page, for the record.
    let backdrop = pixel(origin.x + px(20.), origin.y + px(780.));
    println!("page {page:?} under the backdrop: {backdrop:?}; card {card:?}, select trigger {trigger:?}");
    assert!(card.size.width > px(0.) && trigger.size.width > px(0.), "the fixture was not laid out");

    // The select's menu drops past the card's bottom edge: a pixel of its padding under the card.
    let below_card = card.bottom() + px(24.);
    let select = pixel(trigger.left() + px(40.), below_card);
    // The context menu and the tooltip sit on the backdrop left of the card.
    let (mx, my) = KitLayers::MENU_AT;
    // In the top padding band, clear of the rounded corner and the border.
    let menu = pixel(origin.x + px(mx + 40.), origin.y + px(my + 3.));
    let (tx, ty, tw, th) = KitLayers::TOOLTIP_AT;
    let tooltip = pixel(origin.x + px(tx + tw + 16. + 3.), origin.y + px(ty + th / 2.));
    println!("select menu {select:?} / context menu {menu:?} (bg-light {menu_bg:?}); tooltip {tooltip:?} (bg-darkest {tooltip_bg:?})");
    assert!(near(select, menu_bg), "the select's menu is not drawn above the modal: {select:?}, its colour is {menu_bg:?}");
    assert!(near(menu, menu_bg), "the context menu is not drawn above the modal: {menu:?}, its colour is {menu_bg:?}");
    assert!(near(tooltip, tooltip_bg), "the tooltip is not drawn above the modal: {tooltip:?}, its colour is {tooltip_bg:?}");
}
