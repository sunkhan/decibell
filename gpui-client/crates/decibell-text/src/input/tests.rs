//! The display layer against a real text system (GPUI's Linux one with the bundled fonts):
//! positions, vertical movement, what an edit re-shapes. The expectations that are numbers
//! from Chromium are `tests/data/chromium-input.json`'s `misc` entries.

use std::borrow::Cow;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Hsla, TextSystem};

use super::buffer::{Buffer, Rules};
use super::display::{Display, DisplayStats, InputStyle};
use super::history::EditKind;
use crate::{Affinity, Engine, FontSpec, TextStyle};

fn engine() -> Rc<Engine> {
    let platform = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter");
    let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/static");
    let mut files: Vec<_> = std::fs::read_dir(fonts)
        .expect("gpui-client/assets/fonts/static")
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ttf"))
        .collect();
    files.sort();
    let text_system = Arc::new(TextSystem::new(Arc::new(platform)));
    text_system
        .add_fonts(files.iter().map(|file| Cow::Owned(std::fs::read(file).unwrap())).collect())
        .unwrap();
    Engine::new(text_system)
}

fn style(engine: &Engine) -> InputStyle {
    let face = engine.face(&FontSpec::new("Decibell Inter", 400));
    InputStyle::new(TextStyle::new(face, 13., Hsla::default()))
}

struct Field {
    engine: Rc<Engine>,
    style: InputStyle,
    buffer: Buffer,
    display: Display,
    width: f32,
}

impl Field {
    fn new(rules: Rules, text: &str, width: f32) -> Self {
        let engine = engine();
        let mut field = Self {
            style: style(&engine),
            engine,
            buffer: Buffer::new(rules),
            display: Display::default(),
            width,
        };
        field.buffer.set_text(text);
        field.update();
        field
    }

    fn update(&mut self) -> DisplayStats {
        self.display.update(&self.buffer, &self.style, self.width, 1., &self.engine)
    }

    /// Up or Down from `offset` with a goal x, as `InputState::move_caret` does it.
    fn vertical(&self, offset: usize, goal: f32, down: bool) -> usize {
        match self.display.vertical(offset, Affinity::Downstream, goal, down) {
            Some((offset, _)) => offset,
            None if down => self.buffer.text().len(),
            None => 0,
        }
    }
}

fn misc(name: &str) -> serde_json::Value {
    let vectors: serde_json::Value = serde_json::from_str(include_str!("../../tests/data/chromium-input.json")).unwrap();
    vectors["misc"][name].clone()
}

#[test]
fn up_and_down_keep_their_column_as_in_chromium() {
    // "0123456789\nab\n0123456789" from offset 8: Down, Down, Up, Up.
    let field = Field::new(Rules::default(), "0123456789\nab\n0123456789", 400.);
    let (goal, _, _) = field.display.caret(8, Affinity::Downstream);
    let mut at = 8;
    let mut seen = Vec::new();
    for down in [true, true, false, false] {
        at = field.vertical(at, goal, down);
        seen.push(at as u64);
    }
    let expected: Vec<u64> = misc("goal_column").as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(seen, expected);
    // Up on the first line goes to the start, Down on the last to the end.
    let field = Field::new(Rules::default(), "abcdef\nxyz", 400.);
    assert_eq!(field.vertical(3, 20., false) as u64, misc("area_up_first_line")[0].as_u64().unwrap());
    assert_eq!(field.vertical(8, 20., true) as u64, misc("area_down_last_line")[0].as_u64().unwrap());
}

#[test]
fn home_and_end_are_the_visual_lines_ends() {
    let field = Field::new(Rules::default(), "ab\ncdef\ngh", 400.);
    let ((start, _), (end, _)) = field.display.visual_line(5, Affinity::Downstream);
    assert_eq!(start as u64, misc("home_end")[0][0].as_u64().unwrap());
    assert_eq!(end as u64, misc("home_end")[1][0].as_u64().unwrap());
    // A soft wrap: End stays on its line (upstream), Home of the next line is the same
    // offset downstream.
    let text = "word ".repeat(40);
    let field = Field::new(Rules::default(), text.trim_end(), 200.);
    let ((start, _), (end, end_side)) = field.display.visual_line(3, Affinity::Downstream);
    assert_eq!(start, 0);
    assert!(end > 10 && end < text.len() - 10 && end_side == Affinity::Upstream, "the first line ends at {end}");
    let (_, first_top, height) = field.display.caret(end, Affinity::Upstream);
    let (x, second_top, _) = field.display.caret(end, Affinity::Downstream);
    assert_eq!(first_top, 0.);
    assert_eq!(second_top, height);
    assert_eq!(x, 0.);
    let ((next_start, _), _) = field.display.visual_line(end, Affinity::Downstream);
    assert_eq!(next_start, end);
    // An empty line between two: its caret is at x 0 on its own line box.
    let field = Field::new(Rules::default(), "a\n\nb", 200.);
    let (x, top, height) = field.display.caret(2, Affinity::Downstream);
    assert_eq!((x, top), (0., height));
    assert_eq!(field.display.height(), 3. * height);
    assert_eq!(field.display.hit(50., top + 2.).0, 2);
}

#[test]
fn hit_testing_returns_caret_stops() {
    let mut field = Field::new(
        Rules {
            emoji_atoms: true,
            ..Rules::default()
        },
        "ab😀cd",
        400.,
    );
    let (before, _, _) = field.display.caret(2, Affinity::Downstream);
    let (after, _, _) = field.display.caret(6, Affinity::Downstream);
    // `.rich-emoji`: 1.375 em and a 1 px margin on each side.
    assert!((after - before - (1.375 * 13. + 2.)).abs() < 0.1, "the atom is {} px wide", after - before);
    assert_eq!(field.display.hit(before + 3., 5.).0, 2);
    assert_eq!(field.display.hit(after - 3., 5.).0, 6);
    assert_eq!(field.display.hit(-50., -50.).0, 0);
    assert_eq!(field.display.hit(1e4, 1e4).0, field.buffer.text().len());
    assert_eq!(field.display.shown_text(), "ab\u{fffc}cd");
    // The preedit is drawn (underlined) in place and does not move what follows it oddly.
    field.buffer.move_to(2, false);
    field.buffer.set_marked(None, "か", None);
    let stats = field.update();
    assert_eq!(stats.shaped, 1);
    assert_eq!(field.display.shown_text(), "abか\u{fffc}cd");
}

#[test]
fn an_edit_shapes_its_line_and_a_new_width_shapes_nothing() {
    let text = (0..40).map(|line| format!("line {line} of a long draft")).collect::<Vec<_>>().join("\n");
    let mut field = Field::new(Rules::default(), &text, 300.);
    let before = field.engine.stats();
    // Typing in the middle line.
    field.buffer.move_to(text.len() / 2, false);
    field.buffer.insert("x", EditKind::Typing);
    assert_eq!(field.update(), DisplayStats { shaped: 1, reused: 39, wrapped: 0 });
    assert_eq!(field.engine.stats().paragraphs - before.paragraphs, 1);
    // A new line above everything: the lines below keep their layouts.
    field.buffer.move_to(0, false);
    field.buffer.insert("\n", EditKind::Typing);
    assert_eq!(field.update(), DisplayStats { shaped: 1, reused: 40, wrapped: 0 });
    // Nothing changed: nothing happens.
    assert_eq!(field.update(), DisplayStats::default());
    // A resize wraps every line again and shapes none.
    let shapes = field.engine.stats().shapes;
    field.width = 120.;
    assert_eq!(field.update(), DisplayStats { shaped: 0, reused: 0, wrapped: 41 });
    assert_eq!(field.engine.stats().shapes, shapes);
    assert!(field.display.height() > 41. * field.display.line_height());
}

#[test]
fn a_password_is_bullets_and_its_text_is_in_no_layout() {
    let rules = Rules {
        multiline: false,
        masked: true,
        ..Rules::default()
    };
    let mut field = Field::new(rules, "hunter2 şifre", 200.);
    assert_eq!(field.display.shown_text(), "•".repeat(13));
    // Positions map through the bullets: the caret after 3 characters is 3 bullets in.
    let (one, _, _) = field.display.caret(1, Affinity::Downstream);
    let (three, _, _) = field.display.caret(3, Affinity::Downstream);
    assert!((three - 3. * one).abs() < 0.01 && one > 2.);
    assert_eq!(field.display.hit(one * 9.1, 5.).0, "hunter2 ş".len());
    // Another password of the same shape reuses the layout: the key holds no character.
    field.buffer.set_text("0123456 çabcd");
    assert_eq!(field.update(), DisplayStats { shaped: 0, reused: 1, wrapped: 0 });
}

#[test]
fn a_single_line_never_wraps() {
    let text = "a value much longer than the box it is in ".repeat(8);
    let field = Field::new(
        Rules {
            multiline: false,
            ..Rules::default()
        },
        &text,
        120.,
    );
    assert_eq!(field.display.height(), field.display.line_height());
    assert!(field.display.width() > 1500.);
    // Trailing spaces are kept and the caret stands after them.
    let (end, _, _) = field.display.caret(text.len(), Affinity::Downstream);
    let (before_space, _, _) = field.display.caret(text.len() - 1, Affinity::Downstream);
    assert!(end > before_space);
}
