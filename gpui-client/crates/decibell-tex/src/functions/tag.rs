//! Port of KaTeX `src/functions/tag.ts`. KaTeX defines only a MathML builder for `tag`
//! nodes; on the HTML side `buildHTML` lays out the tag itself (`build_html::build_html`),
//! and `\tag` is a macro (`\tag@literal` in `macros.rs`). Nothing to port here.

use crate::define_function::FunctionSpec;

pub static SPECS: &[FunctionSpec] = &[];
