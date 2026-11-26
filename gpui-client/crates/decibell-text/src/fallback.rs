//! Fallback fonts on Linux, loaded the first time a character needs one.
//!
//! The client starts with only its bundled faces in GPUI's font database: indexing every
//! installed font costs tens of milliseconds before the first frame and makes every later
//! font query walk the whole index (`gpui-client/patches/README.md`, 0011). When shaping
//! meets a character none of the loaded fonts has, this module asks fontconfig which
//! installed font the system would use for it (what Chromium asks too), maps that one file
//! and adds it. The database grows by the fonts a user's conversations actually need.
//!
//! libfontconfig is opened with `dlopen` at that moment, not linked: a machine without it
//! simply gets no fallback, and nothing is paid at startup.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use gpui::TextSystem;

/// A run may need a few fonts (CJK text with an emoji and a symbol); past this it is drawn
/// with the missing glyphs it has.
pub(crate) const MAX_FONTS_PER_RUN: usize = 4;

#[derive(Default)]
pub(crate) struct SystemFallback {
    /// Font files already added (by path) and characters no installed font has.
    loaded: RefCell<HashSet<String>>,
    hopeless: RefCell<HashSet<char>>,
}

impl SystemFallback {
    /// Adds the installed font that covers `character`. `false` when there is none, or it
    /// is already loaded (shaping again would not help).
    pub fn load_font_for(&self, character: char, text_system: &TextSystem) -> bool {
        if self.hopeless.borrow().contains(&character) {
            return false;
        }
        let started = std::time::Instant::now();
        let found = fontconfig().and_then(|fc| fc.font_for(character));
        let Some(path) = found else {
            self.hopeless.borrow_mut().insert(character);
            return false;
        };
        if !self.loaded.borrow_mut().insert(path.clone()) {
            // The font is loaded and the shaper still found no glyph: don't ask again.
            self.hopeless.borrow_mut().insert(character);
            return false;
        }
        let Some(bytes) = map_file(&path) else {
            return false;
        };
        let added = text_system.add_fonts(vec![Cow::Borrowed(bytes)]).is_ok();
        log::info!(
            "fallback font for U+{:04X}: {path} ({:.1} ms)",
            character as u32,
            started.elapsed().as_secs_f64() * 1e3
        );
        added
    }
}

/// Maps a font file for the life of the process: its pages stay file-backed, and only the
/// ones a glyph touches are ever resident.
fn map_file(path: &str) -> Option<&'static [u8]> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: a private read-only mapping; installed fonts are replaced by rename, not
    // rewritten in place, so the mapping keeps its contents.
    let map = unsafe { memmap2::Mmap::map(&file).ok()? };
    Some(&**Box::leak(Box::new(map)))
}

/// Loads libfontconfig and its configuration now, from any thread, so the first fallback
/// lookup does not pay for it (about 10 ms). Optional.
pub fn prewarm_system_fonts() {
    let _ = fontconfig();
}

fn fontconfig() -> Option<&'static Fontconfig> {
    static LIBRARY: OnceLock<Option<Fontconfig>> = OnceLock::new();
    LIBRARY.get_or_init(Fontconfig::open).as_ref()
}

type FcPattern = c_void;
type FcCharSet = c_void;
type FcConfig = c_void;

#[repr(C)]
struct FcFontSet {
    nfont: c_int,
    sfont: c_int,
    fonts: *mut *mut FcPattern,
}

/// The dozen fontconfig entry points a character lookup needs.
struct Fontconfig {
    config: *mut FcConfig,
    pattern_create: unsafe extern "C" fn() -> *mut FcPattern,
    pattern_destroy: unsafe extern "C" fn(*mut FcPattern),
    pattern_add_charset: unsafe extern "C" fn(*mut FcPattern, *const c_char, *const FcCharSet) -> c_int,
    pattern_add_bool: unsafe extern "C" fn(*mut FcPattern, *const c_char, c_int) -> c_int,
    pattern_add_string: unsafe extern "C" fn(*mut FcPattern, *const c_char, *const u8) -> c_int,
    pattern_get_string: unsafe extern "C" fn(*const FcPattern, *const c_char, c_int, *mut *mut u8) -> c_int,
    pattern_get_charset: unsafe extern "C" fn(*const FcPattern, *const c_char, c_int, *mut *mut FcCharSet) -> c_int,
    charset_create: unsafe extern "C" fn() -> *mut FcCharSet,
    charset_destroy: unsafe extern "C" fn(*mut FcCharSet),
    charset_add_char: unsafe extern "C" fn(*mut FcCharSet, u32) -> c_int,
    charset_has_char: unsafe extern "C" fn(*const FcCharSet, u32) -> c_int,
    config_substitute: unsafe extern "C" fn(*mut FcConfig, *mut FcPattern, c_int) -> c_int,
    default_substitute: unsafe extern "C" fn(*mut FcPattern),
    font_sort: unsafe extern "C" fn(*mut FcConfig, *mut FcPattern, c_int, *mut *mut FcCharSet, *mut c_int) -> *mut FcFontSet,
    font_set_destroy: unsafe extern "C" fn(*mut FcFontSet),
}

// SAFETY: fontconfig is thread-safe for these calls on an initialised configuration, and
// the function pointers are plain addresses.
unsafe impl Send for Fontconfig {}
unsafe impl Sync for Fontconfig {}

impl Fontconfig {
    // Each symbol's type is its field's, a few lines below the lookup.
    #[allow(clippy::missing_transmute_annotations)]
    fn open() -> Option<Fontconfig> {
        // SAFETY: dlopen / dlsym with NUL-terminated names; each symbol is transmuted to
        // the signature fontconfig documents for it.
        unsafe {
            let library = libc::dlopen(c"libfontconfig.so.1".as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if library.is_null() {
                log::warn!("libfontconfig.so.1 not found: no fallback fonts");
                return None;
            }
            macro_rules! symbol {
                ($name:literal) => {{
                    let address = libc::dlsym(library, $name.as_ptr());
                    if address.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, _>(address)
                }};
            }
            let init: unsafe extern "C" fn() -> *mut FcConfig = symbol!(c"FcInitLoadConfigAndFonts");
            let config = init();
            if config.is_null() {
                return None;
            }
            Some(Fontconfig {
                config,
                pattern_create: symbol!(c"FcPatternCreate"),
                pattern_destroy: symbol!(c"FcPatternDestroy"),
                pattern_add_charset: symbol!(c"FcPatternAddCharSet"),
                pattern_add_bool: symbol!(c"FcPatternAddBool"),
                pattern_add_string: symbol!(c"FcPatternAddString"),
                pattern_get_string: symbol!(c"FcPatternGetString"),
                pattern_get_charset: symbol!(c"FcPatternGetCharSet"),
                charset_create: symbol!(c"FcCharSetCreate"),
                charset_destroy: symbol!(c"FcCharSetDestroy"),
                charset_add_char: symbol!(c"FcCharSetAddChar"),
                charset_has_char: symbol!(c"FcCharSetHasChar"),
                config_substitute: symbol!(c"FcConfigSubstitute"),
                default_substitute: symbol!(c"FcDefaultSubstitute"),
                font_sort: symbol!(c"FcFontSort"),
                font_set_destroy: symbol!(c"FcFontSetDestroy"),
            })
        }
    }

    /// The file of the font fontconfig ranks first among those that have `character`: a
    /// scalable sans-serif face, a colour face for emoji.
    fn font_for(&self, character: char) -> Option<String> {
        const MATCH_PATTERN: c_int = 0;
        const RESULT_MATCH: c_int = 0;
        // SAFETY: every pointer comes from fontconfig and is released below; strings are
        // NUL-terminated; out-parameters point at live locals.
        unsafe {
            let pattern = (self.pattern_create)();
            let charset = (self.charset_create)();
            (self.charset_add_char)(charset, character as u32);
            (self.pattern_add_charset)(pattern, c"charset".as_ptr(), charset);
            (self.pattern_add_bool)(pattern, c"scalable".as_ptr(), 1);
            if is_emoji_presentation(character) {
                (self.pattern_add_bool)(pattern, c"color".as_ptr(), 1);
                (self.pattern_add_string)(pattern, c"family".as_ptr(), c"emoji".as_ptr().cast());
            } else {
                (self.pattern_add_string)(pattern, c"family".as_ptr(), c"sans-serif".as_ptr().cast());
            }
            (self.config_substitute)(self.config, pattern, MATCH_PATTERN);
            (self.default_substitute)(pattern);

            let mut result = 0;
            let set = (self.font_sort)(self.config, pattern, 0, std::ptr::null_mut(), &mut result);
            let mut found = None;
            if !set.is_null() {
                let fonts = std::slice::from_raw_parts((*set).fonts, (*set).nfont.max(0) as usize);
                for &font in fonts {
                    let mut font_charset = std::ptr::null_mut();
                    if (self.pattern_get_charset)(font, c"charset".as_ptr(), 0, &mut font_charset) != RESULT_MATCH
                        || (self.charset_has_char)(font_charset, character as u32) == 0
                    {
                        continue;
                    }
                    let mut file = std::ptr::null_mut();
                    if (self.pattern_get_string)(font, c"file".as_ptr(), 0, &mut file) == RESULT_MATCH {
                        found = CStr::from_ptr(file.cast()).to_str().ok().map(str::to_owned);
                        break;
                    }
                }
                (self.font_set_destroy)(set);
            }
            (self.charset_destroy)(charset);
            (self.pattern_destroy)(pattern);
            found
        }
    }
}

/// Characters drawn as colour emoji without a variation selector, roughly: enough to ask
/// fontconfig for a colour font first.
fn is_emoji_presentation(character: char) -> bool {
    matches!(character as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x231A..=0x23FF)
}
