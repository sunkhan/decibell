//! Renders corpus cases with the engine to PNG files, for the pixel comparison with
//! Chromium's captures (`tools/README.md`).
//!
//!   cargo run --release -p decibell-text --example render -- cases.json out-dir [--scale 2]
//!
//! Each case is drawn like the harness page draws it: #ece6da text on #17141c, the paragraph
//! 4 px in from the picture's edge. Uses GPUI's headless renderer (Linux, any GPU).

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "../tests/common/mod.rs"]
mod common;

fn main() {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    linux::main();
    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    eprintln!("this tool builds GPUI's Linux text system: run it on Linux");
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux {
    use super::common;


    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::Arc;

    use decibell_text::{Engine, Layout, PaintOptions, ParagraphBuilder};
    use gpui::{
        AnyWindowHandle, App, AppContext as _, Bounds, Context, HeadlessAppContext, IntoElement, Pixels, Render, Styled as _,
        Window, canvas, fill, point, px, rgb, size,
    };
    use serde_json::Value;

    struct Stage {
        layout: Option<Rc<Layout>>,
    }

    impl Render for Stage {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let layout = self.layout.clone();
            canvas(
                |_, _, _| {},
                move |bounds: Bounds<Pixels>, _, window: &mut Window, cx: &mut App| {
                    window.paint_quad(fill(bounds, rgb(0x17141c)));
                    if let Some(layout) = &layout {
                        layout.paint(point(bounds.origin.x + px(4.), bounds.origin.y + px(4.)), &PaintOptions::default(), window, cx);
                    }
                },
            )
            .size_full()
        }
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let cases: Value = serde_json::from_str(&std::fs::read_to_string(&args[0]).unwrap()).unwrap();
        let out_dir = std::path::PathBuf::from(&args[1]);
        let scale: f32 = args.iter().position(|arg| arg == "--scale").map_or(1., |at| args[at + 1].parse().unwrap());
        std::fs::create_dir_all(&out_dir).unwrap();

        let platform = Arc::new(gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter"));
        let mut cx = HeadlessAppContext::with_platform(platform, Arc::new(()), gpui_platform::current_headless_renderer);
        let fonts = std::fs::read_dir(common::assets().join("fonts/static"))
            .unwrap()
            .map(|entry| std::borrow::Cow::Owned(std::fs::read(entry.unwrap().path()).unwrap()))
            .collect();
        cx.update(|cx| cx.text_system().add_fonts(fonts)).unwrap();
        let window = cx
            .open_window(size(px(920.), px(600.)), |_, cx| cx.new(|_| Stage { layout: None }))
            .expect("a headless renderer");
        let any: AnyWindowHandle = window.into();
        cx.simulate_window_scale_factor_change(any, scale);

        let styles: HashMap<String, common::BodyStyle> = cases["styles"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, value)| (name.clone(), common::BodyStyle::from_json(value)))
            .collect();
        for case in cases["cases"].as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            let body = &styles[case["style"].as_str().unwrap()];
            let width = case["w"].as_f64().unwrap() as f32;
            let segs = case["segs"].as_array().unwrap();
            let wrap = case["wrap"].as_str().unwrap_or("body");
            let layout = cx.update(|cx| {
                let engine = Engine::global(cx);
                let text_color = rgb(0xece6da).into();
                let base = body.base(&engine).color(text_color);
                let mut builder = ParagraphBuilder::new(&engine, &base, common::wrapping(wrap), scale);
                for (index, seg) in segs.iter().enumerate() {
                    let seg = seg.as_array().unwrap();
                    let flags = seg[0].as_u64().unwrap();
                    let mut style = body.style(&engine, flags).color(text_color);
                    if flags & common::LINK != 0 {
                        style.color = rgb(0xc9a45c).into();
                    }
                    if flags & common::META != 0 {
                        style.color = rgb(0x8a8494).into();
                    }
                    if let Some(chip) = &mut style.chip {
                        chip.background = rgb(0x0e0c12).into();
                        chip.radius = 4.;
                    }
                    builder.text(seg[1].as_str().unwrap(), &style, index as u32);
                }
                Rc::new(Layout::new(Rc::new(builder.build()), width, &engine))
            });
            let height = layout.height();
            cx.update_window(any, |root, window, cx| {
                root.downcast::<Stage>().unwrap().update(cx, |stage, _| stage.layout = Some(layout));
                window.refresh();
            })
            .unwrap();
            cx.run_until_parked();
            let image = cx.capture_screenshot(any).expect("a capture");
            let crop = |value: f32| (value * scale).ceil() as u32;
            let picture = image::imageops::crop_imm(&image, 0, 0, crop(width + 8.), crop(height + 8.)).to_image();
            let path = out_dir.join(format!("{id}@{scale}x.png"));
            picture.save(&path).unwrap();
            println!("{}", path.display());
        }
    }
}
