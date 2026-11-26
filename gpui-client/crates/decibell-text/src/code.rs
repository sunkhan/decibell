//! Code blocks: `decibell-highlight`'s spans as a paragraph of non-wrapping (or wrapping)
//! mono lines, with the highlight run off the UI thread when it is not instant.
//!
//! The block's chrome (header, copy button, the 14-line collapse) belongs to the chat view;
//! this module gives it the text: [`Engine::highlighted`] for the spans and
//! [`Engine::code_paragraph`] for a [`Paragraph`] to wrap ([`crate::Layout::new`]) and show
//! ([`crate::TextBlock`]).
//!
//! The Electron client highlights synchronously, so its blocks never show plain first. Here
//! a block is highlighted within the frame when that is quick (grammar already compiled,
//! small step budget); otherwise the caller draws it plain and is called back when the spans
//! are in the cache. Colours and the comment slant change then, the size does not: advances
//! are the same, and the row keeps its height.

use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::{Hash as _, Hasher as _};
use std::sync::Arc;

use decibell_highlight::{Budget, Fallback, Highlighted, Language, Token};
use gpui::{App, Hsla};

use crate::cache::{Generations, KeyHasher};
use crate::style::hash_style;
use crate::{Engine, Face, Layout, Paragraph, ParagraphBuilder, TextStyle, Wrapping};

/// Highlighted blocks kept (the renderer keeps 200 trees).
const CACHE_BYTES: usize = 1 << 20;

/// What a highlight may cost inside a frame: about a quarter of a millisecond of regex work.
/// A block that needs more goes to a background thread.
const INLINE_STEPS: u64 = 40_000;

/// How code is drawn: the block's text style (mono face, size, line height and the colour
/// of unstyled text) and the palette `globals.css` maps highlight.js's classes to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CodeStyle {
    pub text: TextStyle,
    /// The face for comments (the mono face, italic).
    pub italic: Face,
    /// `--color-accent-bright`: keyword, built_in, literal, doctag, meta.
    pub keyword: Hsla,
    /// `--color-accent`: title, name, section, type, selector-tag.
    pub title: Hsla,
    /// `--color-success`: string, regexp, quote, addition.
    pub string: Hsla,
    /// `--color-warning`: number, symbol, attr, variable, …
    pub number: Hsla,
    /// `--color-text-muted`: comment.
    pub comment: Hsla,
    /// `--color-error`: deletion.
    pub deletion: Hsla,
    /// `--color-text-secondary`: operator, punctuation.
    pub operator: Hsla,
}

impl CodeStyle {
    fn color(&self, token: Token) -> Hsla {
        match token {
            Token::Keyword => self.keyword,
            Token::Title => self.title,
            Token::String => self.string,
            Token::Number => self.number,
            Token::Comment => self.comment,
            Token::Deletion => self.deletion,
            Token::Operator => self.operator,
            Token::Plain => self.text.color,
        }
    }
}

pub(crate) struct CodeStore {
    highlighted: RefCell<Generations<Arc<Highlighted>>>,
    /// Blocks a background thread is working on.
    pending: RefCell<HashSet<u64>>,
}

impl Default for CodeStore {
    fn default() -> Self {
        Self {
            highlighted: RefCell::new(Generations::new(CACHE_BYTES)),
            pending: RefCell::default(),
        }
    }
}

fn code_key(language: Language, code: &str) -> u64 {
    let mut hasher = KeyHasher::default();
    language.name().hash(&mut hasher);
    code.hash(&mut hasher);
    hasher.finish()
}

impl Engine {
    /// The highlight of `code`, if it is known or quick to make. Otherwise `None` now, and
    /// `ready` runs on the UI thread once it is cached (notify the view that shows the
    /// block; a second call then returns it). `ready` is dropped when a result is returned.
    pub fn highlighted(
        &self,
        language: Language,
        code: &str,
        cx: &mut App,
        ready: impl FnOnce(&mut App) + 'static,
    ) -> Option<Arc<Highlighted>> {
        let key = code_key(language, code);
        if let Some(hit) = self.code.highlighted.borrow_mut().get(key) {
            return Some(hit);
        }
        if language.is_loaded() {
            let budget = Budget {
                max_steps: INLINE_STEPS,
                max_time: None,
                ..Budget::default()
            };
            let highlighted = decibell_highlight::highlight_language(language, code, &budget);
            if highlighted.fallback != Some(Fallback::OutOfBudget) {
                return Some(self.store_highlight(key, highlighted));
            }
        }
        if self.code.pending.borrow_mut().insert(key) {
            let source = code.to_owned();
            let work = cx
                .background_executor()
                .spawn(async move { decibell_highlight::highlight_language(language, &source, &Budget::default()) });
            cx.spawn(async move |cx| {
                let highlighted = work.await;
                cx.update(|cx| {
                    let engine = Engine::global(cx);
                    engine.code.pending.borrow_mut().remove(&key);
                    engine.store_highlight(key, highlighted);
                    ready(cx);
                });
            })
            .detach();
        }
        None
    }

    fn store_highlight(&self, key: u64, highlighted: Highlighted) -> Arc<Highlighted> {
        let bytes = size_of::<Highlighted>() + highlighted.spans.len() * size_of::<decibell_highlight::Span>() * 2;
        let highlighted = Arc::new(highlighted);
        self.code.highlighted.borrow_mut().insert(key, highlighted.clone(), bytes);
        highlighted
    }

    /// The paragraph of a code block: one line per `\n` with [`Wrapping::PRE`] (long lines
    /// run on; the caller scrolls them), or wrapped with [`Wrapping::PRE_WRAP`]. Without
    /// `highlighted` the text is plain.
    pub fn code_paragraph(
        &self,
        code: &str,
        highlighted: Option<&Highlighted>,
        style: &CodeStyle,
        wrapping: Wrapping,
        scale: f32,
    ) -> Paragraph {
        let mut builder = ParagraphBuilder::new(self, &style.text, wrapping, scale);
        match highlighted {
            Some(highlighted) => {
                for span in &highlighted.spans {
                    let mut run = style.text;
                    run.color = style.color(span.token);
                    if span.italic {
                        run.face = style.italic;
                    }
                    builder.text(&code[span.range.clone()], &run, 0);
                }
            }
            None => {
                builder.text(code, &style.text, 0);
            }
        }
        builder.build()
    }

    /// The layout of a code block's text at `width` px (`f32::INFINITY` for lines that
    /// never wrap), cached by (code, plain or highlighted, style, width, scale): the
    /// "CodeText" a code block's chrome puts in a [`crate::TextBlock`]. Its `width()` is
    /// the longest line, for a horizontal scroll range.
    pub fn code_layout(
        &self,
        code: &str,
        highlighted: Option<&Highlighted>,
        style: &CodeStyle,
        wrapping: Wrapping,
        width: f32,
        scale: f32,
    ) -> std::rc::Rc<Layout> {
        let mut hasher = KeyHasher::default();
        code.hash(&mut hasher);
        (highlighted.is_some(), wrapping, scale.to_bits()).hash(&mut hasher);
        hash_style(&style.text, &mut hasher);
        style.italic.hash(&mut hasher);
        for color in [style.keyword, style.title, style.string, style.number, style.comment, style.deletion, style.operator] {
            for value in [color.h, color.s, color.l, color.a] {
                value.to_bits().hash(&mut hasher);
            }
        }
        let key = hasher.finish();
        self.labels.layout(key, width, scale, || {
            let paragraph = self
                .labels
                .paragraph(key, || self.code_paragraph(code, highlighted, style, wrapping, scale));
            Layout::new(paragraph, width, self)
        })
    }
}
