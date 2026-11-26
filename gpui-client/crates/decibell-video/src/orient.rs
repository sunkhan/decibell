//! How a file's pictures are turned when they are drawn.
//!
//! A phone records portrait video on its side and says so in the container (the track's display
//! matrix). Chromium applies it (0 / 90 / 180 / 270 degrees, and a mirror when the matrix has
//! one); so does this crate, in the surface's draw: the renderer turns the texture coordinates
//! (GPUI patch `0030`), no pixel is copied, and everything that asks a stream for its picture's
//! size ([`crate::Stream::picture_size`]) gets the turned one. Live streams are never turned.

/// Three bits. With `p` a point of the drawn picture (0..1, y down), the stored picture is
/// sampled at `p` with x and y exchanged if [`Self::TRANSPOSE`] is set, then with x mirrored if
/// [`Self::FLIP_X`] and y mirrored if [`Self::FLIP_Y`] (the renderer's `surface_orient`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Orientation(u8);

impl Orientation {
    pub const FLIP_X: u8 = 1;
    pub const FLIP_Y: u8 = 2;
    pub const TRANSPOSE: u8 = 4;

    pub const IDENTITY: Self = Self(0);
    /// The stored picture is drawn turned a quarter clockwise (a phone held upright).
    pub const ROTATE_90: Self = Self(Self::TRANSPOSE | Self::FLIP_Y);
    pub const ROTATE_180: Self = Self(Self::FLIP_X | Self::FLIP_Y);
    /// The stored picture is drawn turned a quarter counter-clockwise.
    pub const ROTATE_270: Self = Self(Self::TRANSPOSE | Self::FLIP_X);

    pub fn from_bits(bits: u8) -> Self {
        Self(bits & 7)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn is_identity(self) -> bool {
        self.0 == 0
    }

    /// Whether the drawn picture's width is the stored picture's height.
    pub fn transposes(self) -> bool {
        self.0 & Self::TRANSPOSE != 0
    }

    /// The drawn size of a picture stored as `size`.
    pub fn apply(self, size: (u32, u32)) -> (u32, u32) {
        if self.transposes() { (size.1, size.0) } else { size }
    }

    /// From a display matrix as FFmpeg hands it over (`AV_PKT_DATA_DISPLAYMATRIX`: nine 16.16 /
    /// 2.30 fixed-point numbers in the MP4 layout, `x' = m[0] x + m[3] y`, `y' = m[1] x + m[4] y`
    /// with y down). The identity unless the matrix is one of the eight axis-aligned ones
    /// (a quarter turn, a mirror, or both): anything else (a shear, 45 degrees) is drawn as
    /// stored, which is what Chromium does with a matrix it does not recognise.
    pub fn from_display_matrix(matrix: &[i32; 9]) -> Self {
        let (a, b, c, d) = (matrix[0] as f64, matrix[1] as f64, matrix[3] as f64, matrix[4] as f64);
        let scale = a.abs().max(b.abs()).max(c.abs()).max(d.abs());
        if scale <= 0.0 {
            return Self::IDENTITY;
        }
        let sign = |value: f64| {
            let value = value / scale;
            if value > 0.99 {
                Some(1)
            } else if value < -0.99 {
                Some(-1)
            } else if value.abs() < 0.01 {
                Some(0)
            } else {
                None
            }
        };
        let (Some(a), Some(b), Some(c), Some(d)) = (sign(a), sign(b), sign(c), sign(d)) else {
            return Self::IDENTITY;
        };
        let flips = |x: i32, y: i32| {
            (if x < 0 { Self::FLIP_X } else { 0 }) | (if y < 0 { Self::FLIP_Y } else { 0 })
        };
        // Drawn from stored: x' = a u + c v, y' = b u + d v. Not transposed: u = a x', v = d y'.
        // Transposed: u = b y', v = c x'.
        match (a, b, c, d) {
            (a, 0, 0, d) if a != 0 && d != 0 => Self(flips(a, d)),
            (0, b, c, 0) if b != 0 && c != 0 => Self(Self::TRANSPOSE | flips(b, c)),
            _ => Self::IDENTITY,
        }
    }

    /// Where the point `(x, y)` of the drawn picture (0..1) is in the stored one: what the
    /// renderer's shader computes.
    pub fn stored_point(self, x: f64, y: f64) -> (f64, f64) {
        let (mut u, mut v) = if self.transposes() { (y, x) } else { (x, y) };
        if self.0 & Self::FLIP_X != 0 {
            u = 1.0 - u;
        }
        if self.0 & Self::FLIP_Y != 0 {
            v = 1.0 - v;
        }
        (u, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: i32 = 1 << 16;

    fn matrix(a: i32, b: i32, c: i32, d: i32) -> [i32; 9] {
        [a * ONE, b * ONE, 0, c * ONE, d * ONE, 0, 0, 0, 1 << 30]
    }

    #[test]
    fn the_four_rotations_and_the_mirrors_are_read_from_the_matrix() {
        // What FFmpeg writes for `-display_rotation` / a phone's `rotate` tag.
        assert_eq!(Orientation::from_display_matrix(&matrix(1, 0, 0, 1)), Orientation::IDENTITY);
        assert_eq!(Orientation::from_display_matrix(&matrix(0, 1, -1, 0)), Orientation::ROTATE_90);
        assert_eq!(Orientation::from_display_matrix(&matrix(-1, 0, 0, -1)), Orientation::ROTATE_180);
        assert_eq!(Orientation::from_display_matrix(&matrix(0, -1, 1, 0)), Orientation::ROTATE_270);
        assert_eq!(Orientation::from_display_matrix(&matrix(-1, 0, 0, 1)).bits(), Orientation::FLIP_X);
        assert_eq!(Orientation::from_display_matrix(&matrix(1, 0, 0, -1)).bits(), Orientation::FLIP_Y);
        assert_eq!(Orientation::from_display_matrix(&matrix(0, 1, 1, 0)).bits(), Orientation::TRANSPOSE);
        // Any scale; a matrix that is none of the eight is drawn as stored.
        assert_eq!(Orientation::from_display_matrix(&[0, 3, 0, -3, 0, 0, 0, 0, 1]), Orientation::ROTATE_90);
        assert_eq!(Orientation::from_display_matrix(&matrix(1, 1, -1, 1)), Orientation::IDENTITY);
        assert_eq!(Orientation::from_display_matrix(&[0; 9]), Orientation::IDENTITY);
    }

    #[test]
    fn a_quarter_turn_clockwise_puts_the_stored_top_left_at_the_top_right() {
        // The stored picture's top-left corner (0, 0) is drawn at the top right: the drawn
        // top-right corner samples the stored top-left.
        let near = |(u, v): (f64, f64), (x, y): (f64, f64)| (u - x).abs() < 1e-9 && (v - y).abs() < 1e-9;
        assert!(near(Orientation::ROTATE_90.stored_point(1.0, 0.0), (0.0, 0.0)));
        assert!(near(Orientation::ROTATE_90.stored_point(0.0, 0.0), (0.0, 1.0)));
        assert!(near(Orientation::ROTATE_180.stored_point(1.0, 1.0), (0.0, 0.0)));
        assert!(near(Orientation::ROTATE_270.stored_point(0.0, 1.0), (0.0, 0.0)));
        assert_eq!(Orientation::ROTATE_90.apply((1920, 1080)), (1080, 1920));
        assert_eq!(Orientation::ROTATE_180.apply((1920, 1080)), (1920, 1080));
    }
}
