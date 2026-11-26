//! Port of the parts of KaTeX `src/Settings.ts` that matter with Decibell's options.
//!
//! Fixed, as the app renders: `throwOnError: false`, `trust: false`, `strict: "warn"` (warnings
//! only, never a behaviour change), `output: "htmlAndMathml"` (only the HTML half is visible,
//! so only it is built), `macros: {}`, `globalGroup: false`.

/// Render settings. [`Settings::default`] is what Decibell's Electron renderer passes.
#[derive(Clone, Debug)]
pub struct Settings {
    pub display_mode: bool,
    /// The error colour KaTeX writes into error spans' `style`. The layout reports errors with
    /// a symbolic colour instead; this string only appears in [`crate::render_to_markup`].
    pub error_color: String,
    /// `maxSize` in em (the app uses 16; KaTeX's default is unbounded).
    pub max_size: f64,
    /// `maxExpand` (the app uses 256; KaTeX's default is 1000).
    pub max_expand: u32,
    pub min_rule_thickness: f64,
    pub leqno: bool,
    pub fleqn: bool,
    pub color_is_text_color: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            display_mode: false,
            error_color: "#cc0000".to_string(),
            max_size: 16.0,
            max_expand: 256,
            min_rule_thickness: 0.0,
            leqno: false,
            fleqn: false,
            color_is_text_color: false,
        }
    }
}
