//! The composer screens, drawn from the chat crate's seeded model (no network, no core): the
//! `#general` fixture list with the real composer under it, in each state
//! (`decibell --screen composer-typing`, `decibell-capture --screen … --all-themes`).

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use decibell_attachments::upload::{PreparedUpload, UploadControl, UploadProgress, UploadSource, UploadedAttachment};
use decibell_attachments::{AttachmentKind, AttachmentTarget, EncryptedAttachmentMeta};
use decibell_chat::fixture::{self, Which};
use decibell_chat::{ListEvent, ListSource, MessageList};
use decibell_harness::Screen;
use decibell_model::actions::chat::SendOutcome;
use decibell_model::chat::composer::{AttachmentService, UploadError};
use decibell_model::chat::{ComposerAttachments, DraftKey, MessageSlice, SliceKey};
use decibell_model::{ChannelId, Model, ServerId};
use decibell_ui::{ActiveTheme as _, AssumeClientDecorations};
use futures::channel::{mpsc, oneshot};
use gpui::{AnyView, App, AppContext as _, Context, Entity, IntoElement, Render, SharedString, StyleRefinement, Subscription, Task, Window, canvas, div, prelude::*};

use crate::gifs::{FixtureGifs, GifService};
use crate::panel::SnippetKind;
use crate::target::{ChannelTarget, ComposerTarget, Gate, Variant};
use crate::Composer;

/// The pane a screen draws: the fixture's list over the composer, as ChatPanel lays them out.
pub struct ScreenPane {
    pub list: Entity<MessageList>,
    pub composer: Entity<Composer>,
    _subscription: Subscription,
}

impl Render for ScreenPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = cx.theme().color_bg_mid;
        let view_size = self.list.read(cx).view_size_cell();
        let probe = canvas(
            move |bounds, _, _| view_size.set((f32::from(bounds.size.width).round(), (f32::from(bounds.size.height).round() - 70.).max(0.))),
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(background)
            .child(probe)
            .child(div().flex_1().min_h_0().child(self.list.clone().cached(StyleRefinement::default().size_full())))
            .child(self.composer.clone())
    }
}

pub fn daily_gaming() -> ServerId {
    ServerId::from_central(decibell_shell::fixture::DAILY_GAMING)
}

/// The fixture's `#general` with its list, and a composer on `target` (default: `#general`).
pub fn pane(target: Option<Rc<dyn ComposerTarget>>, window: &mut Window, cx: &mut App) -> Entity<ScreenPane> {
    // A capture renders one frame on a clock that does not run: entrances show their end.
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
    // The capture tool doesn't run the app's `init_features`: the slots this crate fills.
    if decibell_chat::slots(cx).editor.is_none() {
        crate::init(cx);
    }
    let source = fixture::open(Which::General, cx);
    let target = target.unwrap_or_else(|| Rc::new(ChannelTarget { server: daily_gaming(), channel: "general".into() }));
    cx.new(|cx| {
        let list_source: Rc<dyn ListSource> = source.clone();
        let list = cx.new(|cx| MessageList::new(list_source, cx));
        // The pane's own rail (ChatPane does the same).
        let subscription = cx.subscribe(&list, |_, list, event: &ListEvent, cx| {
            if let ListEvent::Reply(id) = event {
                list.update(cx, |list, cx| list.set_reply_target(Some(*id), cx));
            }
        });
        let composer = cx.new(|cx| Composer::new(target, Some(list.clone()), window, cx));
        ScreenPane { list, composer, _subscription: subscription }
    })
}

fn with(pane: &Entity<ScreenPane>, cx: &mut App, f: impl FnOnce(&mut Composer, &mut Context<Composer>)) {
    let composer = pane.read(cx).composer.clone();
    composer.update(cx, f);
}

/// A message of `#general`'s newest page, `back` rows from the bottom.
fn recent_id(back: usize, cx: &App) -> i64 {
    let model = Model::global(cx);
    let slice = model.chat.channels.read(cx).get(daily_gaming(), "general").cloned();
    slice.map_or(0, |slice| {
        let rows = slice.read(cx).rows();
        rows.iter().rev().nth(back).map_or(0, |m| m.id)
    })
}

/// The composer's draft, as typed.
fn type_draft(pane: &Entity<ScreenPane>, text: &str, cx: &mut App) {
    let input = pane.read(cx).composer.read(cx).input().clone();
    input.update(cx, |input, cx| input.set_value(text, cx));
}

const FORMATTED: &str = "Release notes for **tonight**: `cargo build --release` then\n```rust\nfn main() {\n    println!(\"hello, decibell\");\n}\n```\nand the bound is $$\\sum_{k=1}^{n} k = \\frac{n(n+1)}{2}$$";

/// A target whose gate is fixed (the disabled screens' reasons).
struct FixedGate {
    gate: Gate,
}

impl ComposerTarget for FixedGate {
    fn variant(&self) -> Variant {
        Variant::Channel
    }
    fn draft_key(&self) -> DraftKey {
        DraftKey::Channel(daily_gaming(), "general".into())
    }
    fn server(&self) -> Option<ServerId> {
        Some(daily_gaming())
    }
    fn slice(&self, cx: &App) -> Option<Entity<MessageSlice>> {
        Model::global(cx).chat.channels.read(cx).get(daily_gaming(), "general").cloned()
    }
    fn placeholder(&self, gate: &Gate, _: &App) -> SharedString {
        format!("Message #{}", gate.name).into()
    }
    fn gate(&self, _: &App) -> Gate {
        self.gate.clone()
    }
    fn send(&self, _: &str, _: i64, _: &mut App) -> SendOutcome {
        SendOutcome::Nothing
    }
    fn send_gif(&self, _: &str, _: i64, _: &mut App) -> SendOutcome {
        SendOutcome::Nothing
    }
    fn set_draft(&self, _: SharedString, _: &mut App) {}
    fn latest_own_editable(&self, _: &App) -> Option<i64> {
        None
    }
    fn watch(&self, _: &mut Context<Composer>) -> Vec<Subscription> {
        Vec::new()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// ── Attachments in several states (a service whose uploads the screen finishes) ─────────────────

/// Uploads that end as the screen says: by name, "ready…" succeeds, "failed…" fails, "uploading…"
/// stays at 42 %, the rest stay queued.
struct ScreenUploads;

impl AttachmentService for ScreenUploads {
    fn prepare(&self, _: UploadSource, _: &mut App) -> Task<Result<PreparedUpload, String>> {
        Task::ready(Err("not in screens".into()))
    }

    fn upload(
        &self,
        _: ServerId,
        _: ChannelId,
        file: Arc<PreparedUpload>,
        _: bool,
        _: UploadControl,
        cx: &mut App,
    ) -> (Task<Result<UploadedAttachment, UploadError>>, mpsc::UnboundedReceiver<UploadProgress>) {
        let (progress, receiver) = mpsc::unbounded();
        let total = file.size;
        let name = file.name.clone();
        if name.starts_with("uploading") {
            let _ = progress.unbounded_send(UploadProgress { sent: total * 42 / 100, total, speed_bps: 1.0e6, paused: false });
            // Never ends while the screen shows.
            let (keep, wait) = oneshot::channel::<()>();
            std::mem::forget(keep);
            return (cx.spawn(async move |_| {
                let _ = wait.await;
                Err(UploadError { message: "gone".into(), cancelled: true })
            }), receiver);
        }
        let result = if name.starts_with("failed") {
            Err(UploadError { message: "the file is larger than this server allows".into(), cancelled: false })
        } else {
            Ok(UploadedAttachment { id: 9001, kind: file.kind, filename: name, mime: file.mime.clone(), size_bytes: total, thumbnail_sizes_mask: 0, encrypted: None })
        };
        (Task::ready(result), receiver)
    }

    fn set_target(&self, _: ServerId, _: Option<AttachmentTarget>) {}
    fn clear_all(&self) {}
    fn register_keys(&self, _: ServerId, _: Vec<EncryptedAttachmentMeta>) {}
}

/// A picture for the image chips: a diagonal gradient in the accent's hue, as a 320 px JPEG
/// thumbnail (what the probe makes).
fn picture(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_fn(width, height, |x, y| {
        let t = (x + y) as f32 / (width + height) as f32;
        image::Rgba([(200. - 120. * t) as u8, (120. + 60. * t) as u8, (230. - 40. * t) as u8, 255])
    });
    decibell_attachments::thumbs::encode_jpeg(&image, 85).unwrap_or_default()
}

fn prepared(name: &str, kind: AttachmentKind, size: u64, thumb: bool) -> PreparedUpload {
    PreparedUpload {
        source: UploadSource::Memory { name: name.into(), mime: String::new(), data: Default::default() },
        name: name.into(),
        mime: match kind {
            AttachmentKind::Image => "image/png".into(),
            _ => "application/pdf".into(),
        },
        kind,
        size,
        width: 640,
        height: 400,
        duration_ms: 0,
        placeholder: String::new(),
        thumbnails: if thumb { vec![(320, picture(320, 200))] } else { Vec::new() },
    }
}

/// Swaps the composer store for one over [`ScreenUploads`] and fills `#general`'s box.
fn fill_attachments(cx: &mut App) {
    let store = cx.new(|_| ComposerAttachments::new(Rc::new(ScreenUploads)));
    cx.update_global::<Model, _>(|model, _| model.chat.composer = store.clone());
    let (server, channel): SliceKey = (daily_gaming(), "general".into());
    store.update(cx, |c, cx| {
        let files = [
            ("ready-sunset.png", AttachmentKind::Image, 2_400_000, true),
            ("uploading-trailer.png", AttachmentKind::Image, 8_000_000, true),
            ("release-notes.pdf", AttachmentKind::Document, 310_000, false),
            ("failed-raw-footage.mov", AttachmentKind::Video, 900_000_000, false),
        ];
        for (name, kind, size, thumb) in files {
            let id = c.add(server, channel.clone(), prepared(name, kind, size, thumb), false, cx).expect("under the cap");
            if !name.starts_with("release") {
                c.start_upload(&id, cx);
            }
        }
    });
}

// ── The screens ────────────────────────────────────────────────────────────────────────────────

fn build(window: &mut Window, cx: &mut App, target: Option<Rc<dyn ComposerTarget>>, setup: impl FnOnce(&Entity<ScreenPane>, &mut Window, &mut App)) -> AnyView {
    let pane = pane(target, window, cx);
    setup(&pane, window, cx);
    pane.into()
}

fn set_recent(cx: &mut App) {
    let model = Model::global(cx).clone();
    let ids = ["joy", "fire", "thumbsup", "heart", "eyes", "tada", "sob", "pray", "100", "rocket", "sparkles"];
    model.layout.update(cx, |layout, cx| layout.set_emoji_recent(ids.iter().map(|s| s.to_string()).collect(), cx));
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen { name: "composer-empty", about: "The composer at rest under #general (placeholder, attach, code / math, emoji, send)", build: |window, cx| build(window, cx, None, |_, _, _| {}) },
        Screen {
            name: "composer-typing",
            about: "A formatted draft: the live preview strip renders it as a sent message (code block, formula)",
            build: |window, cx| build(window, cx, None, |pane, _, cx| type_draft(pane, FORMATTED, cx)),
        },
        Screen {
            name: "composer-reply",
            about: "Replying: the bar in the card, the rail on the message",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    let id = recent_id(2, cx);
                    with(pane, cx, |composer, cx| composer.start_reply(id, window, cx));
                })
            },
        },
        Screen {
            name: "composer-attachments",
            about: "Attachment chips: ready (picture), uploading 42 %, queued (document), failed (the server's refusal)",
            build: |window, cx| {
                let view = build(window, cx, None, |_, _, _| {});
                fill_attachments(cx);
                view
            },
        },
        Screen {
            name: "composer-slowmode",
            about: "#slow-lane (30 s): a send held, the pill counting down",
            build: |window, cx| {
                let target: Rc<dyn ComposerTarget> = Rc::new(ChannelTarget { server: daily_gaming(), channel: "slow-lane".into() });
                build(window, cx, Some(target), |pane, _, cx| {
                    let model = Model::global(cx).clone();
                    let now = cx.background_executor().now();
                    model.chat.slowmode.update(cx, |s, cx| s.note_own_message((daily_gaming(), "slow-lane".into()), 30, now - Duration::from_secs(6), cx));
                    type_draft(pane, "held until the window ends", cx);
                })
            },
        },
        Screen {
            name: "composer-disabled",
            about: "No SEND_MESSAGES in #announcements: the notice instead of the card",
            build: |window, cx| {
                let target: Rc<dyn ComposerTarget> = Rc::new(ChannelTarget { server: daily_gaming(), channel: "announcements".into() });
                build(window, cx, Some(target), |_, _, _| {})
            },
        },
        Screen {
            name: "composer-timed-out",
            about: "Timed out for two hours: the notice with the time left",
            build: |window, cx| {
                let gate = Gate { name: "general".into(), can_send: true, timed_out_until: now_secs() + 7_000, can_attach: true, slowmode: 0 };
                build(window, cx, Some(Rc::new(FixedGate { gate })), |_, _, _| {})
            },
        },
        Screen {
            name: "emoji-picker",
            about: "The emoji picker open over the composer, with frequently used emoji",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    set_recent(cx);
                    with(pane, cx, |composer, cx| composer.open_picker(window, cx))
                })
            },
        },
        Screen {
            name: "emoji-picker-search",
            about: "The emoji picker searching \"smile\"",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    with(pane, cx, |composer, cx| composer.open_picker(window, cx));
                    let picker = pane.read(cx).composer.read(cx).picker().cloned();
                    if let Some(picker) = picker {
                        let search = picker.read(cx).search_input().clone();
                        search.update(cx, |s, cx| s.set_value("smile", cx));
                    }
                })
            },
        },
        Screen {
            name: "gif-picker",
            about: "The GIFs tab with a canned KLIPY page (tiles without pictures: no network in screens)",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    GifService::set(cx, Rc::new(FixtureGifs::pages(16, 2, None)));
                    gif_tab(cx);
                    with(pane, cx, |composer, cx| composer.open_picker(window, cx))
                })
            },
        },
        Screen {
            name: "gif-picker-unconfigured",
            about: "The GIFs tab without a key: the set-up notice",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    let mut source = FixtureGifs::pages(0, 1, None);
                    source.configured = false;
                    GifService::set(cx, Rc::new(source));
                    gif_tab(cx);
                    with(pane, cx, |composer, cx| composer.open_picker(window, cx))
                })
            },
        },
        Screen {
            name: "code-panel",
            about: "The code block panel over the composer, with code typed",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    with(pane, cx, |composer, cx| composer.open_snippet(SnippetKind::Code, window, cx));
                    if let Some(panel) = pane.read(cx).composer.read(cx).panel().cloned() {
                        let field = panel.read(cx).field().clone();
                        field.update(cx, |f, cx| f.set_value("fn main() {\n    let answer = 42;\n    println!(\"{answer}\");\n}", cx));
                        panel.update(cx, |p, cx| p.set_lang("rust", cx));
                    }
                })
            },
        },
        Screen {
            name: "math-panel",
            about: "The math panel with a formula and its live rendering",
            build: |window, cx| {
                build(window, cx, None, |pane, window, cx| {
                    with(pane, cx, |composer, cx| composer.open_snippet(SnippetKind::Math, window, cx));
                    if let Some(panel) = pane.read(cx).composer.read(cx).panel().cloned() {
                        let field = panel.read(cx).field().clone();
                        field.update(cx, |f, cx| f.set_value("\\int_0^1 x^2\\,dx = \\frac{1}{3}", cx));
                    }
                })
            },
        },
        Screen {
            name: "inline-editor",
            about: "Editing a message in place: the field, the hint",
            build: |window, cx| {
                // ArrowUp in the empty box: the latest message of ours.
                build(window, cx, None, |pane, window, cx| with(pane, cx, |composer, cx| composer.edit_latest(window, cx)))
            },
        },
    ]
}

fn gif_tab(cx: &mut App) {
    let model = Model::global(cx).clone();
    model.layout.update(cx, |layout, cx| layout.set_picker_tab(true, cx));
}
