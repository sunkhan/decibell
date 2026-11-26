//! A byte-bounded cache with two generations: a hit in the old generation moves the entry to
//! the new one, and when the new one outgrows half the budget the old one is dropped. O(1)
//! per lookup, no per-entry bookkeeping, and nothing is evicted while it is in use every
//! frame.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::rc::Rc;

use crate::{Layout, Paragraph};

/// Budget of the cache behind [`crate::Label`]: a few thousand labels.
pub(crate) const LABEL_CACHE_BYTES: usize = 2 << 20;

/// Keys are already hashes.
#[derive(Default)]
pub(crate) struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self.0.rotate_left(8) ^ *byte as u64;
        }
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

type Map<V> = HashMap<u64, (V, u32), BuildHasherDefault<IdentityHasher>>;

/// See the module docs. `V` is cheap to clone (an `Rc`).
pub struct Generations<V> {
    young: Map<V>,
    old: Map<V>,
    young_bytes: usize,
    budget: usize,
}

impl<V: Clone> Generations<V> {
    pub fn new(budget: usize) -> Self {
        Self {
            young: Map::default(),
            old: Map::default(),
            young_bytes: 0,
            budget,
        }
    }

    pub fn get(&mut self, key: u64) -> Option<V> {
        if let Some((value, _)) = self.young.get(&key) {
            return Some(value.clone());
        }
        let (value, bytes) = self.old.remove(&key)?;
        self.insert(key, value.clone(), bytes as usize);
        Some(value)
    }

    pub fn insert(&mut self, key: u64, value: V, bytes: usize) {
        if self.young_bytes + bytes > self.budget / 2 && !self.young.is_empty() {
            self.old = std::mem::take(&mut self.young);
            self.young_bytes = 0;
        }
        self.young_bytes += bytes;
        self.young.insert(key, (value, bytes as u32));
    }

    pub fn clear(&mut self) {
        self.young.clear();
        self.old.clear();
        self.young_bytes = 0;
    }

    /// Entries and bytes held (both generations).
    pub fn usage(&self) -> (usize, usize) {
        let old: usize = self.old.values().map(|(_, bytes)| *bytes as usize).sum();
        (self.young.len() + self.old.len(), self.young_bytes + old)
    }
}

/// Paragraphs by content and layouts by (content, width, scale).
pub struct LayoutCache {
    paragraphs: std::cell::RefCell<Generations<Rc<Paragraph>>>,
    layouts: std::cell::RefCell<Generations<Rc<Layout>>>,
}

impl LayoutCache {
    pub fn new(budget: usize) -> Self {
        Self {
            paragraphs: std::cell::RefCell::new(Generations::new(budget / 2)),
            layouts: std::cell::RefCell::new(Generations::new(budget / 2)),
        }
    }

    /// The paragraph for `key`, built on a miss.
    pub fn paragraph(&self, key: u64, build: impl FnOnce() -> Paragraph) -> Rc<Paragraph> {
        if let Some(hit) = self.paragraphs.borrow_mut().get(key) {
            return hit;
        }
        let paragraph = Rc::new(build());
        let bytes = paragraph.heap_bytes() + size_of::<Paragraph>();
        self.paragraphs.borrow_mut().insert(key, paragraph.clone(), bytes);
        paragraph
    }

    /// The layout of the paragraph `key` at a width, built on a miss.
    pub fn layout(&self, key: u64, width: f32, scale: f32, build: impl FnOnce() -> Layout) -> Rc<Layout> {
        let key = layout_key(key, width, scale);
        if let Some(hit) = self.layouts.borrow_mut().get(key) {
            return hit;
        }
        let layout = Rc::new(build());
        let bytes = layout.heap_bytes() + size_of::<Layout>();
        self.layouts.borrow_mut().insert(key, layout.clone(), bytes);
        layout
    }

    pub fn clear(&self) {
        self.paragraphs.borrow_mut().clear();
        self.layouts.borrow_mut().clear();
    }

    /// (entries, bytes) of the paragraphs and of the layouts.
    pub fn usage(&self) -> ((usize, usize), (usize, usize)) {
        (self.paragraphs.borrow().usage(), self.layouts.borrow().usage())
    }
}

/// A layout's key: the content key, the width in device pixels and the scale factor.
pub fn layout_key(content: u64, width: f32, scale: f32) -> u64 {
    let mut hasher = KeyHasher::default();
    content.hash(&mut hasher);
    let device_width = if width.is_finite() { (width * scale * 64.).floor() as i64 } else { -1 };
    device_width.hash(&mut hasher);
    scale.to_bits().hash(&mut hasher);
    hasher.finish()
}

/// The hasher for cache keys (FNV-1a over 64 bits: short inputs, no DoS concern, since a
/// collision needs two texts shown at once).
pub struct KeyHasher(u64);

impl Default for KeyHasher {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = (self.0 ^ *byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}
