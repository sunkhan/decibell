use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Cursor;

use gpui::TestAppContext;
use image::{Rgba, RgbaImage};

use super::*;

/// A fetcher from memory that records what was asked and can hold answers back.
#[derive(Default)]
struct Fake {
    files: RefCell<HashMap<Source, Bytes>>,
    asked: RefCell<Vec<Source>>,
    cancelled: RefCell<Vec<Source>>,
    hold: RefCell<bool>,
    held: RefCell<Vec<(Source, futures::channel::oneshot::Sender<Result<Bytes, SharedString>>, CancellationToken)>>,
}

struct FakeFetcher(Rc<Fake>);

impl Fetcher for FakeFetcher {
    fn fetch(&self, source: &Source, cancel: CancellationToken, cx: &mut App) -> Task<Result<Bytes, SharedString>> {
        self.0.asked.borrow_mut().push(source.clone());
        let found = self.0.files.borrow().get(source).cloned().ok_or(SharedString::new_static("HTTP 404"));
        if *self.0.hold.borrow() {
            let (tx, rx) = futures::channel::oneshot::channel();
            self.0.held.borrow_mut().push((source.clone(), tx, cancel));
            return cx.background_spawn(async move { rx.await.unwrap_or(Err(SharedString::new_static("dropped"))) });
        }
        Task::ready(found)
    }
}

fn png(w: u32, h: u32, color: [u8; 4]) -> Bytes {
    let mut out = Vec::new();
    RgbaImage::from_pixel(w, h, Rgba(color)).write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png).unwrap();
    Bytes::from(out)
}

fn attachment(id: i64) -> Source {
    Source::Attachment { server: "1".into(), id, thumb: 640 }
}

fn key(id: i64, w: f32) -> PicKey {
    PicKey::new(attachment(id), Spec { width: w, height: w * 0.5, scale: 1., fit: Fit::Contain, upscale: true })
}

fn setup(cx: &mut TestAppContext) -> (Rc<Fake>, Entity<Pictures>) {
    let fake = Rc::new(Fake::default());
    let pictures = cx.update(|cx| Pictures::install(Rc::new(FakeFetcher(fake.clone())), cx));
    (fake, pictures)
}

fn owner(cx: &mut TestAppContext) -> Owner {
    let view = cx.update(|cx| cx.new(|_| ()));
    (view.entity_id(), 0)
}

#[gpui::test]
fn a_wanted_picture_is_fetched_decoded_at_its_box_and_kept(cx: &mut TestAppContext) {
    let (fake, pictures) = setup(cx);
    fake.files.borrow_mut().insert(attachment(1), png(640, 320, [200, 10, 10, 255]));
    let list = owner(cx);
    let k = key(1, 300.);
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: true }], cx));
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&k)), Look::Waiting));
    cx.run_until_parked();
    let Look::Ready(picture) = pictures.read_with(cx, |p, _| p.look(&k)) else { panic!("not decoded") };
    // Decoded at the box (300 × 150), not at the file's 640 × 320; one texel of border.
    assert_eq!(picture.placement.out, (300, 150));
    assert_eq!(picture.image.size(0).width.0, 302);
    assert_eq!(picture.natural, (640, 320));
    let (decoded, encoded) = pictures.read_with(cx, |p, _| p.bytes());
    assert_eq!(decoded, 302 * 152 * 4);
    assert!(encoded > 0);
    // Wanted again (another frame of the list): no second fetch.
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: true }], cx));
    cx.run_until_parked();
    assert_eq!(fake.asked.borrow().len(), 1);
    // The same file in another box: decoded again from memory, not fetched.
    let bigger = key(1, 400.);
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: bigger.clone(), visible: true }], cx));
    // While it decodes, the 300 px one stands in.
    assert!(pictures.read_with(cx, |p, _| p.stand_in(&bigger)).is_some());
    cx.run_until_parked();
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&bigger)), Look::Ready(_)));
    assert_eq!(fake.asked.borrow().len(), 1);
}

#[gpui::test]
fn a_row_that_leaves_cancels_its_fetch(cx: &mut TestAppContext) {
    let (fake, pictures) = setup(cx);
    *fake.hold.borrow_mut() = true;
    fake.files.borrow_mut().insert(attachment(2), png(64, 32, [0, 0, 0, 255]));
    let list = owner(cx);
    let k = key(2, 64.);
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: false }], cx));
    cx.run_until_parked();
    assert_eq!(fake.held.borrow().len(), 1);
    pictures.update(cx, |p, cx| p.set_wants(list, Vec::new(), cx));
    let (_, _, cancel) = fake.held.borrow_mut().pop().unwrap();
    assert!(cancel.is_cancelled());
    assert_eq!(pictures.read_with(cx, |p, _| (p.entry_count(), p.pending())), (0, 0));
}

#[gpui::test]
fn at_most_six_fetches_per_community_visible_first(cx: &mut TestAppContext) {
    let (fake, pictures) = setup(cx);
    *fake.hold.borrow_mut() = true;
    let list = owner(cx);
    let mut wants: Vec<Want> = (1..=8).map(|id| Want { key: key(id, 100.), visible: false }).collect();
    wants.push(Want { key: key(9, 100.), visible: true });
    pictures.update(cx, |p, cx| p.set_wants(list, wants, cx));
    cx.run_until_parked();
    let asked = fake.asked.borrow().clone();
    assert_eq!(asked.len(), 6);
    assert_eq!(asked[0], attachment(9), "the visible one first");
}

#[gpui::test]
fn unwanted_pictures_are_evicted_past_the_budget(cx: &mut TestAppContext) {
    let (fake, pictures) = setup(cx);
    // Each decode is 1002 × 502 × 4 ≈ 2 MB: thirteen go past the 24 MiB budget.
    for id in 1..=20 {
        fake.files.borrow_mut().insert(attachment(id), png(1000, 500, [id as u8, 0, 0, 255]));
    }
    let list = owner(cx);
    for id in 1..=20 {
        pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: key(id, 1000.), visible: true }], cx));
        cx.run_until_parked();
    }
    pictures.update(cx, |p, cx| p.set_wants(list, Vec::new(), cx));
    let (decoded, _) = pictures.read_with(cx, |p, _| p.bytes());
    assert!(decoded <= DECODED_BUDGET, "{decoded}");
    // The most recently wanted stays; the oldest went.
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&key(20, 1000.))), Look::Ready(_)));
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&key(1, 1000.))), Look::Waiting));
}

#[gpui::test]
fn a_failure_is_retried_when_the_row_comes_back(cx: &mut TestAppContext) {
    let (fake, pictures) = setup(cx);
    let list = owner(cx);
    let k = key(3, 50.);
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: true }], cx));
    cx.run_until_parked();
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&k)), Look::Failed(_)));
    fake.files.borrow_mut().insert(attachment(3), png(100, 50, [1, 2, 3, 255]));
    pictures.update(cx, |p, cx| p.set_wants(list, Vec::new(), cx));
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: true }], cx));
    cx.run_until_parked();
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&k)), Look::Ready(_)));
}

fn gif(frames: usize) -> Bytes {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new(&mut out);
        enc.set_repeat(Repeat::Infinite).unwrap();
        for i in 0..frames {
            let frame = Frame::from_parts(RgbaImage::from_pixel(80, 40, Rgba([(i * 40) as u8, 0, 0, 255])), 0, 0, Delay::from_numer_denom_ms(50, 1));
            enc.encode_frame(frame).unwrap();
        }
    }
    Bytes::from(out)
}

#[gpui::test]
fn an_animation_plays_only_while_visible(cx: &mut TestAppContext) {
    let (fake, pictures) = setup(cx);
    let source = Source::Attachment { server: "1".into(), id: 7, thumb: 0 };
    fake.files.borrow_mut().insert(source.clone(), gif(4));
    let list = owner(cx);
    let k = PicKey::new(source, Spec { width: 80., height: 40., scale: 1., fit: Fit::Contain, upscale: true });
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: false }], cx));
    cx.run_until_parked();
    assert!(matches!(pictures.read_with(cx, |p, _| p.look(&k)), Look::Ready(_)));
    // Near but not on screen: a still, no timer.
    assert_eq!(pictures.read_with(cx, |p, _| p.playing()), 0);
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: true }], cx));
    cx.run_until_parked();
    assert_eq!(pictures.read_with(cx, |p, _| p.playing()), 1);
    let first = match pictures.read_with(cx, |p, _| p.look(&k)) {
        Look::Ready(p) => p.image.id,
        _ => unreachable!(),
    };
    cx.executor().advance_clock(Duration::from_millis(60));
    cx.run_until_parked();
    let second = match pictures.read_with(cx, |p, _| p.look(&k)) {
        Look::Ready(p) => p.image.id,
        _ => unreachable!(),
    };
    assert_ne!(first, second, "the next frame shows after the first one's delay");
    // Nothing painted the frame (no window here): the timer stops by itself.
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(pictures.read_with(cx, |p, _| p.playing()), 0);
    // Scrolled away: no timer either.
    pictures.update(cx, |p, cx| p.set_wants(list, vec![Want { key: k.clone(), visible: false }], cx));
    assert_eq!(pictures.read_with(cx, |p, _| p.playing()), 0);
}

#[gpui::test]
fn placeholders_decode_once_and_carry_a_border(cx: &mut TestAppContext) {
    let (_, pictures) = setup(cx);
    let hash = decibell_attachments::thumbhash::encode_b64(
        10,
        6,
        RgbaImage::from_fn(10, 6, |x, _| Rgba([x as u8 * 20, 100, 50, 255])).as_raw(),
    );
    let (image, size) = cx.update(|cx| pictures.update(cx, |p, cx| p.placeholder(&hash, cx))).expect("decodes");
    assert_eq!((image.size(0).width.0 as u32, image.size(0).height.0 as u32), (size.0 + 2, size.1 + 2));
    let (again, _) = cx.update(|cx| pictures.update(cx, |p, cx| p.placeholder(&hash, cx))).unwrap();
    assert_eq!(image.id, again.id);
    assert!(cx.update(|cx| pictures.update(cx, |p, cx| p.placeholder("", cx))).is_none());
    assert!(cx.update(|cx| pictures.update(cx, |p, cx| p.placeholder("!!", cx))).is_none());
}
