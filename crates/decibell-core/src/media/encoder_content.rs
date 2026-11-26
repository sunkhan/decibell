//! Procedural screen-share content for the encoder quality bench
//! (`encoder_bench.rs`): five kinds of picture a real stream carries,
//! generated so a run is reproducible and nothing from anyone's screen is
//! captured.
//!
//!  - `Text`: a dark code editor scrolling at reading speed, small glyphs in
//!    syntax colours (thin saturated red / blue strokes are what 4:2:0
//!    chroma and a starved rate control smear first).
//!  - `Desktop`: a still document with a clock, a line being typed, a
//!    blinking caret and a mouse pointer: almost nothing changes.
//!  - `Slides`: a still slide (gradient, large and small type, thin chart
//!    lines) replaced every three seconds.
//!  - `Game`: a textured background panning under moving sprites,
//!    particles and a HUD.
//!  - `Video`: smooth moving colour fields with film grain and a cut every
//!    2.5 s.
//!
//! Motion is a function of time, not of the frame number, so 30 and 60 fps
//! runs show the same thing.
#![cfg(all(test, target_os = "linux"))]

use super::encoder_checks::{glyph, hash};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scene {
    Text,
    Desktop,
    Slides,
    Game,
    Video,
}

impl Scene {
    pub const ALL: [Scene; 5] = [Scene::Text, Scene::Desktop, Scene::Slides, Scene::Game, Scene::Video];

    pub fn name(self) -> &'static str {
        match self {
            Scene::Text => "text",
            Scene::Desktop => "desktop",
            Scene::Slides => "slides",
            Scene::Game => "game",
            Scene::Video => "video",
        }
    }

    pub fn parse(s: &str) -> Option<Scene> {
        Scene::ALL.into_iter().find(|scene| scene.name() == s.trim())
    }
}

/// One scene at one size and frame rate. `render` fills a BGRA frame with a
/// padded stride (as PipeWire's can be).
pub struct Content {
    pub scene: Scene,
    pub w: u32,
    pub h: u32,
    pub fps: u32,
    pub stride: usize,
    /// A pre-rendered layer, tightly packed BGRA: the editor's page (Text),
    /// the still desktop (Desktop), the tileable ground texture (Game).
    layer: Vec<u8>,
    layer_w: usize,
    layer_h: usize,
    /// Slides: the slide currently in `layer`.
    slide: Option<u32>,
    sine: Vec<i16>,
}

fn put(buf: &mut [u8], stride: usize, x: usize, y: usize, bgr: [u8; 3]) {
    buf[y * stride + x * 4..][..4].copy_from_slice(&[bgr[0], bgr[1], bgr[2], 0xFF]);
}

#[allow(clippy::too_many_arguments)]
fn rect(buf: &mut [u8], stride: usize, w: usize, h: usize, x: usize, y: usize, rw: usize, rh: usize, bgr: [u8; 3]) {
    for yy in y..(y + rh).min(h) {
        for xx in x..(x + rw).min(w) {
            put(buf, stride, xx, yy, bgr);
        }
    }
}

/// Glyph index of an ASCII digit or capital letter (anything else: 'X').
fn glyph_of(c: u8) -> usize {
    match c {
        b'0'..=b'9' => (c - b'0') as usize,
        b'A'..=b'Z' => (c - b'A') as usize + 10,
        _ => 33,
    }
}

#[allow(clippy::too_many_arguments)]
fn text(buf: &mut [u8], stride: usize, w: usize, h: usize, x: usize, y: usize, scale: usize, s: &str, bgr: [u8; 3]) {
    for (i, c) in s.bytes().enumerate() {
        if c != b' ' {
            glyph(buf, stride, w, h, x + i * 6 * scale, y, scale, glyph_of(c), bgr);
        }
    }
}

/// A line of generated "words" from (`x`, `y`) to `x_end`, each word in
/// the colour `ink(word index)` gives.
#[allow(clippy::too_many_arguments)]
fn words(buf: &mut [u8], stride: usize, w: usize, h: usize, x: usize, x_end: usize, y: usize, scale: usize, seed: u32, ink: &dyn Fn(u32) -> [u8; 3]) {
    let advance = 6 * scale;
    let (mut x, mut word, mut left, mut col) = (x, 0u32, 0u32, 0u32);
    while x + advance < x_end {
        if left == 0 {
            // A gap, then the next word of 2 to 10 glyphs.
            word += 1;
            left = 2 + hash(seed, word) % 9;
            x += advance;
            continue;
        }
        glyph(buf, stride, w, h, x, y, scale, (hash(seed ^ 0x55AA, col) >> 8) as usize, ink(word));
        left -= 1;
        col += 1;
        x += advance;
    }
}

/// Syntax colours of a dark editor theme, BGR. Among them the saturated
/// red and blue that chroma subsampling treats worst.
const SYNTAX: [[u8; 3]; 9] = [
    [0xD4, 0xD4, 0xD4],
    [0xC0, 0x86, 0xC5],
    [0xD6, 0x9C, 0x56],
    [0x55, 0x55, 0xFF],
    [0x55, 0x99, 0x6A],
    [0x78, 0x91, 0xCE],
    [0xAA, 0xDC, 0xDC],
    [0xFF, 0x60, 0x40],
    [0xB0, 0xC9, 0x4E],
];

/// A light document page (dark text, some coloured words), `w`×`h`.
fn document(w: usize, h: usize) -> Vec<u8> {
    let mut page = vec![0xFAu8; w * 4 * h];
    let mut y = 16;
    let mut line = 0u32;
    while y + 20 < h {
        let small = line % 7 == 6;
        let scale = if small { 1 } else { 2 };
        // Paragraphs end short.
        let end = if hash(line, 77) % 4 == 0 { w / 2 } else { w.saturating_sub(24) };
        words(&mut page, w * 4, w, h, 24, end, y, scale, line, &|word| {
            if hash(line, word) % 19 == 0 {
                [0xC0, 0x30, 0x10]
            } else {
                [0x20, 0x20, 0x20]
            }
        });
        y += if small { 12 } else { 22 };
        line += 1;
    }
    page
}

impl Content {
    pub fn new(scene: Scene, w: u32, h: u32, fps: u32) -> Self {
        let (wu, hu) = (w as usize, h as usize);
        let sine: Vec<i16> = (0..1024).map(|i| ((i as f64 / 1024.0 * std::f64::consts::TAU).sin() * 100.0) as i16).collect();
        let mut c = Self { scene, w, h, fps: fps.max(1), stride: wu * 4 + 32, layer: Vec::new(), layer_w: 0, layer_h: 0, slide: None, sine };
        match scene {
            Scene::Text => {
                // The editor's file, three screens tall.
                let (pw, ph) = (wu, hu * 3);
                let mut page = vec![0u8; pw * 4 * ph];
                rect(&mut page, pw * 4, pw, ph, 0, 0, pw, ph, [0x2E, 0x1E, 0x1E]);
                rect(&mut page, pw * 4, pw, ph, 0, 0, 64, ph, [0x25, 0x18, 0x18]);
                let mut y = 6;
                let mut line = 0u32;
                while y + 16 < ph {
                    text(&mut page, pw * 4, pw, ph, 8, y, 2, &format!("{:>4}", line + 1), [0x85, 0x85, 0x85]);
                    let indent = (hash(line, 1) % 5) as usize * 48;
                    let length = 240 + (hash(line, 2) as usize % (pw.saturating_sub(400)).max(1));
                    let comment = line % 8 == 7;
                    // A blank line now and then, as code has.
                    if hash(line, 3) % 9 != 0 {
                        words(&mut page, pw * 4, pw, ph, 76 + indent, (76 + indent + length).min(pw - 8), y, 2, line, &|word| {
                            if comment {
                                SYNTAX[4]
                            } else {
                                SYNTAX[(hash(line, word) % SYNTAX.len() as u32) as usize]
                            }
                        });
                    }
                    y += 20;
                    line += 1;
                }
                (c.layer, c.layer_w, c.layer_h) = (page, pw, ph);
            }
            Scene::Desktop => {
                let mut still = document(wu, hu);
                let s = wu * 4;
                // Side panel with icons, title bar, task bar.
                rect(&mut still, s, wu, hu, 0, 0, 72, hu, [0x30, 0x2A, 0x28]);
                for i in 0..8usize {
                    let colour = SYNTAX[(i + 1) % SYNTAX.len()];
                    rect(&mut still, s, wu, hu, 16, 48 + i * 64, 40, 40, colour);
                }
                rect(&mut still, s, wu, hu, 0, 0, wu, 32, [0x3A, 0x33, 0x30]);
                text(&mut still, s, wu, hu, 84, 9, 2, "QUARTERLY REPORT DRAFT 7", [0xE0, 0xE0, 0xE0]);
                rect(&mut still, s, wu, hu, 0, hu - 40, wu, 40, [0x20, 0x1C, 0x1A]);
                // The input box the typing goes into.
                rect(&mut still, s, wu, hu, 96, hu - 120, wu - 192, 44, [0xFF, 0xFF, 0xFF]);
                rect(&mut still, s, wu, hu, 96, hu - 120, wu - 192, 2, [0xC0, 0x80, 0x30]);
                (c.layer, c.layer_w, c.layer_h) = (still, wu, hu);
            }
            Scene::Slides => {
                (c.layer, c.layer_w, c.layer_h) = (vec![0u8; wu * 4 * hu], wu, hu);
            }
            Scene::Game => {
                // A tileable ground texture: three octaves of value noise
                // through a terrain palette, with per-pixel grit.
                const T: usize = 1024;
                let lattice = |x: usize, y: usize, cell: usize, seed: u32| -> i32 {
                    let n = T / cell;
                    (hash(((x / cell) % n) as u32 + seed * 131, ((y / cell) % n) as u32) & 0xFF) as i32
                };
                let noise = |x: usize, y: usize, cell: usize, seed: u32| -> i32 {
                    let (fx, fy) = ((x % cell) as i32, (y % cell) as i32);
                    let c = cell as i32;
                    let top = lattice(x, y, cell, seed) * (c - fx) + lattice(x + cell, y, cell, seed) * fx;
                    let bottom = lattice(x, y + cell, cell, seed) * (c - fx) + lattice(x + cell, y + cell, cell, seed) * fx;
                    (top * (c - fy) + bottom * fy) / (c * c)
                };
                let mut tile = vec![0u8; T * 4 * T];
                for y in 0..T {
                    for x in 0..T {
                        let v = (noise(x, y, 128, 1) * 4 + noise(x, y, 32, 2) * 2 + noise(x, y, 8, 3)) / 7;
                        let grit = (hash(x as u32, y as u32) % 17) as i32 - 8;
                        let shade = |base: i32, gain: i32| (base + (v - 128) * gain / 64 + grit).clamp(0, 255) as u8;
                        // Water, sand, grass, rock by height.
                        let bgr = match v {
                            0..=95 => [shade(170, 30), shade(110, 30), shade(40, 10)],
                            96..=110 => [shade(120, 20), shade(190, 20), shade(205, 20)],
                            111..=160 => [shade(50, 20), shade(140, 40), shade(60, 20)],
                            _ => [shade(120, 30), shade(125, 30), shade(130, 30)],
                        };
                        put(&mut tile, T * 4, x, y, bgr);
                    }
                }
                (c.layer, c.layer_w, c.layer_h) = (tile, T, T);
            }
            Scene::Video => {}
        }
        c
    }

    pub fn frame_bytes(&self) -> usize {
        self.stride * self.h as usize
    }

    /// Render frame `n` into `out` (`frame_bytes()` long), BGRA.
    pub fn render(&mut self, n: u32, out: &mut [u8]) {
        let (w, h, stride) = (self.w as usize, self.h as usize, self.stride);
        let t = n as f64 / self.fps as f64;
        let ms = (n as u64 * 1000 / self.fps as u64) as usize;
        match self.scene {
            Scene::Text => {
                // 240 px a second: a steady scroll through a file.
                let offset = (ms * 240 / 1000) % (self.layer_h - h);
                for y in 0..h {
                    out[y * stride..][..w * 4].copy_from_slice(&self.layer[(y + offset) * w * 4..][..w * 4]);
                }
                // Tab bar and status bar stay put.
                rect(out, stride, w, h, 0, 0, w, 28, [0x3A, 0x2A, 0x2A]);
                text(out, stride, w, h, 12, 7, 2, "MAIN RS   ENCODER RS   README", [0xD0, 0xD0, 0xD0]);
                rect(out, stride, w, h, 0, h - 24, w, 24, [0xCC, 0x7A, 0x00]);
                text(out, stride, w, h, 12, h - 19, 2, &format!("LN {}", offset / 20 + 1), [0xFF, 0xFF, 0xFF]);
            }
            Scene::Desktop => {
                for y in 0..h {
                    out[y * stride..][..w * 4].copy_from_slice(&self.layer[y * w * 4..][..w * 4]);
                }
                // The clock, counting seconds.
                let secs = 12 * 3600 + 34 * 60 + 50 + ms / 1000;
                let clock = format!("{:02} {:02} {:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60);
                text(out, stride, w, h, w - 120, h - 27, 2, &clock, [0xF0, 0xF0, 0xF0]);
                // Ten characters a second typed into the box, then cleared.
                let capacity = (w - 240) / 12;
                let typed = (ms / 100) % capacity;
                for i in 0..typed {
                    glyph(out, stride, w, h, 108 + i * 12, h - 106, 2, (hash(7, i as u32) >> 8) as usize, [0x20, 0x20, 0x20]);
                }
                if (ms / 500) % 2 == 0 {
                    rect(out, stride, w, h, 108 + typed * 12, h - 110, 2, 22, [0x20, 0x20, 0x20]);
                }
                // The pointer: a small arrow drifting over the page.
                let px = (w as f64 * (0.5 + 0.3 * (0.7 * t).sin())) as usize;
                let py = (h as f64 * (0.45 + 0.3 * (1.1 * t).sin())) as usize;
                for row in 0..18usize {
                    let width = if row < 12 { row + 1 } else { 18 - row };
                    for col in 0..width {
                        let edge = col == 0 || col + 1 == width || row == 11;
                        if px + col < w && py + row < h {
                            put(out, stride, px + col, py + row, if edge { [0, 0, 0] } else { [0xFF, 0xFF, 0xFF] });
                        }
                    }
                }
            }
            Scene::Slides => {
                let slide = (ms / 3000) as u32;
                if self.slide != Some(slide) {
                    self.render_slide(slide);
                    self.slide = Some(slide);
                }
                for y in 0..h {
                    out[y * stride..][..w * 4].copy_from_slice(&self.layer[y * w * 4..][..w * 4]);
                }
            }
            Scene::Game => {
                // The ground pans 300 px/s across and 120 px/s down.
                let (ox, oy) = (ms * 300 / 1000, ms * 120 / 1000);
                let (tw, th) = (self.layer_w, self.layer_h);
                for y in 0..h {
                    let src = &self.layer[((y + oy) % th) * tw * 4..][..tw * 4];
                    let row = &mut out[y * stride..][..w * 4];
                    let mut x = 0;
                    while x < w {
                        let from = (x + ox) % tw;
                        let run = (tw - from).min(w - x);
                        row[x * 4..][..run * 4].copy_from_slice(&src[from * 4..][..run * 4]);
                        x += run;
                    }
                }
                // Sprites: shaded discs bouncing at 100 to 500 px/s.
                let bounce = |start: u32, speed: f64, span: usize| -> usize {
                    let span = span.max(1) as f64;
                    let p = (start as f64 + speed * t) % (2.0 * span);
                    (if p > span { 2.0 * span - p } else { p }) as usize
                };
                for i in 0..12u32 {
                    let r = 24 + (hash(i, 10) % 25) as usize;
                    let cx = r + bounce(hash(i, 11) % w as u32, 100.0 + (hash(i, 12) % 400) as f64, w - 2 * r);
                    let cy = r + bounce(hash(i, 13) % h as u32, 100.0 + (hash(i, 14) % 300) as f64, h - 2 * r);
                    let base = SYNTAX[(i as usize + 1) % SYNTAX.len()];
                    for dy in 0..2 * r {
                        for dx in 0..2 * r {
                            let (ex, ey) = (dx as i32 - r as i32, dy as i32 - r as i32);
                            let d2 = ex * ex + ey * ey;
                            let r2 = (r * r) as i32;
                            if d2 > r2 {
                                continue;
                            }
                            let (x, y) = (cx + dx - r, cy + dy - r);
                            if x >= w || y >= h {
                                continue;
                            }
                            // A dark rim, lit from the top left.
                            let px = if d2 > r2 * 85 / 100 {
                                [0x10, 0x10, 0x10]
                            } else {
                                let light = 190 - (ex + ey) * 60 / r as i32;
                                base.map(|v| (v as i32 * light / 255).clamp(0, 255) as u8)
                            };
                            put(out, stride, x, y, px);
                        }
                    }
                }
                // Particles: 3 px sparks crossing the screen.
                for i in 0..300u32 {
                    let x = ((hash(i, 20) % w as u32) as f64 + (hash(i, 21) % 900) as f64 * t) as usize % (w - 3);
                    let y = ((hash(i, 22) % h as u32) as f64 + (200.0 + (hash(i, 23) % 500) as f64) * t) as usize % (h - 3);
                    let colour = [[0xFF, 0xFF, 0xFF], [0x60, 0xF0, 0xFF], [0xFF, 0xF0, 0x80]][(i % 3) as usize];
                    rect(out, stride, w, h, x, y, 3, 3, colour);
                }
                // HUD: health bar, score, minimap, a line of small text.
                rect(out, stride, w, h, 20, 20, 244, 24, [0x10, 0x10, 0x10]);
                let health = 120 + (100.0 * (0.5 * t).sin()) as usize;
                rect(out, stride, w, h, 22, 22, health, 20, [0x30, 0x30, 0xE0]);
                text(out, stride, w, h, w - 260, 20, 3, &format!("SCORE {:06}", ms / 16 * 5), [0xFF, 0xFF, 0xFF]);
                rect(out, stride, w, h, w - 180, h - 180, 160, 160, [0x20, 0x20, 0x20]);
                for i in 0..20u32 {
                    let (dx, dy) = ((hash(i, 30) % 150) as usize, (hash(i, 31) % 150) as usize);
                    rect(out, stride, w, h, w - 175 + dx, h - 175 + dy, 3, 3, [0x40, 0xE0, 0x40]);
                }
                text(out, stride, w, h, 20, h - 30, 2, "OBJECTIVE REACH THE NORTH GATE", [0xFF, 0xFF, 0xFF]);
            }
            Scene::Video => {
                // Each channel: three sine fields drifting at their own
                // speed. A cut every 2.5 s jumps the phases.
                let cut = (ms / 2500) as u32;
                let drift = |speed: f64, salt: u32| ((t * speed) as usize + (hash(cut, salt) % 1024) as usize) & 1023;
                let phases: [[usize; 3]; 3] = [
                    [drift(90.0, 1), drift(60.0, 2), drift(140.0, 3)],
                    [drift(70.0, 4), drift(110.0, 5), drift(50.0, 6)],
                    [drift(120.0, 7), drift(40.0, 8), drift(80.0, 9)],
                ];
                let sine = &self.sine;
                for y in 0..h {
                    // Film grain: a fresh luma offset per pixel per frame.
                    let mut state = hash(n, y as u32) | 1;
                    let row = &mut out[y * stride..][..w * 4];
                    for (x, px) in row.chunks_exact_mut(4).enumerate() {
                        state ^= state << 13;
                        state ^= state >> 17;
                        state ^= state << 5;
                        let grain = (state & 15) as i32 - 8;
                        for (ch, p) in phases.iter().enumerate() {
                            let v = sine[(x / 2 + p[0]) & 1023] as i32 + sine[(y + p[1]) & 1023] as i32 + sine[((x + y) / 3 + p[2]) & 1023] as i32;
                            px[ch] = (128 + v * 2 / 5 + grain).clamp(0, 255) as u8;
                        }
                        px[3] = 0xFF;
                    }
                }
            }
        }
    }

    /// A still slide into `layer`: a diagonal gradient, a title, bullets,
    /// small print, and a chart of thin coloured lines.
    fn render_slide(&mut self, slide: u32) {
        let (w, h) = (self.w as usize, self.h as usize);
        let s = w * 4;
        let (a, b) = (
            [[0x60u8, 0x20, 0x18], [0x20, 0x40, 0x10], [0x30, 0x18, 0x50]][(slide % 3) as usize],
            [[0x18u8, 0x10, 0x30], [0x50, 0x20, 0x20], [0x10, 0x30, 0x38]][(slide % 3) as usize],
        );
        let buf = &mut self.layer;
        for y in 0..h {
            for x in 0..w {
                let k = (x * 1000 / w + y * 1000 / h) / 2;
                let mix = |p: u8, q: u8| ((p as usize * (1000 - k) + q as usize * k) / 1000) as u8;
                put(buf, s, x, y, [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]);
            }
        }
        text(buf, s, w, h, 60, 60, 6, &format!("RESULTS Q{} 2026", slide % 4 + 1), [0xFF, 0xFF, 0xFF]);
        for i in 0..5usize {
            rect(buf, s, w, h, 64, 190 + i * 56, 12, 12, [0x40, 0xC0, 0xFF]);
            words(buf, s, w, h, 76, w / 2 - 20, 184 + i * 56, 3, slide * 16 + i as u32, &|_| [0xF0, 0xF0, 0xF0]);
        }
        for i in 0..4usize {
            words(buf, s, w, h, 60, w - 60, h - 70 + i * 12, 1, slide * 64 + i as u32, &|_| [0xC8, 0xC8, 0xC8]);
        }
        // The chart: a white panel, grid, three polylines.
        let (cx, cy, cw, ch) = (w / 2 + 20, 180, w / 2 - 80, h - 300);
        rect(buf, s, w, h, cx, cy, cw, ch, [0xF4, 0xF4, 0xF4]);
        for i in 1..8 {
            rect(buf, s, w, h, cx, cy + i * ch / 8, cw, 1, [0xD0, 0xD0, 0xD0]);
            rect(buf, s, w, h, cx + i * cw / 8, cy, 1, ch, [0xD0, 0xD0, 0xD0]);
        }
        for (series, colour) in [[0x20u8, 0x20, 0xE0], [0xE0, 0x40, 0x20], [0x30, 0xA0, 0x20]].into_iter().enumerate() {
            let value = |i: usize| ch / 8 + (hash(slide * 8 + series as u32, i as u32) as usize % (ch * 3 / 4));
            let step = cw / 16;
            for i in 0..16usize {
                let (y0, y1) = (value(i) as i64, value(i + 1) as i64);
                for dx in 0..step {
                    let y = y0 + (y1 - y0) * dx as i64 / step as i64;
                    // 1 px lines for two series, 2 px for the third.
                    for thick in 0..(1 + series / 2) {
                        put(buf, s, cx + i * step + dx, cy + y as usize + thick, colour);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(a: &[u8], b: &[u8]) -> usize {
        a.chunks(4).zip(b.chunks(4)).filter(|(p, q)| p != q).count()
    }

    #[test]
    fn scenes_are_deterministic_and_move_as_described() {
        const W: u32 = 640;
        const H: u32 = 360;
        for scene in Scene::ALL {
            let mut content = Content::new(scene, W, H, 30);
            let mut again = Content::new(scene, W, H, 30);
            let mut a = vec![0u8; content.frame_bytes()];
            let mut b = vec![0u8; content.frame_bytes()];
            let mut c = vec![0u8; content.frame_bytes()];
            content.render(10, &mut a);
            content.render(11, &mut b);
            again.render(10, &mut c);
            assert_eq!(a, c, "{}: deterministic", scene.name());
            let moved = changed(&a, &b);
            let pixels = (W * H) as usize;
            match scene {
                // Scrolling, panning, grain: most of the picture changes.
                Scene::Text => assert!(moved > pixels / 20, "{moved}"),
                Scene::Game | Scene::Video => assert!(moved > pixels / 2, "{moved}"),
                // A pointer and a caret.
                Scene::Desktop => assert!(moved > 0 && moved < pixels / 50, "{moved}"),
                // Still until the slide changes (3 s).
                Scene::Slides => {
                    assert_eq!(moved, 0);
                    content.render(95, &mut b);
                    assert!(changed(&a, &b) > pixels / 4);
                }
            }
        }
    }

    #[test]
    fn the_same_moment_looks_the_same_at_30_and_60_fps() {
        for scene in [Scene::Text, Scene::Game, Scene::Desktop] {
            let mut slow = Content::new(scene, 640, 360, 30);
            let mut fast = Content::new(scene, 640, 360, 60);
            let mut a = vec![0u8; slow.frame_bytes()];
            let mut b = vec![0u8; fast.frame_bytes()];
            slow.render(45, &mut a);
            fast.render(90, &mut b);
            assert_eq!(a, b, "{}", scene.name());
        }
    }
}
