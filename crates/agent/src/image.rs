//! look's picture (a1-plan section 7, amendments 16 and 19): tiles in a
//! grid whose shape comes closest to the aspect asked for, each under a
//! two-line label band, white on near-black and outside the picture so
//! that a label never hides what is in it. The whole is encoded as JPEG
//! through FFmpeg's mjpeg, at the finest quantizer whose base64 fits the
//! inline budget, or as PNG.

use spherecut_render::{Image, Pixels};

use crate::font::{self, CELL};
use crate::limits::MIN_TILE_WIDTH;

/// The label bands' and gutters' colour: near-black.
pub const BAND: [u8; 3] = [16, 16, 16];
/// The labels' and grid lines' colour.
pub const INK: [u8; 3] = [240, 240, 240];
/// Pixels between tiles, side by side and between rows.
pub const GUTTER: u32 = 2;
/// The tile width from which labels are drawn twice the size.
pub const BIG_LABELS: u32 = 320;
/// Padding around and between a band's two lines, at scale 1.
const PAD: u32 = 2;

/// The band's height at `scale`: two lines of text with padding.
pub fn band(scale: u32) -> u32 {
    scale * (3 * PAD + 2 * CELL)
}

/// Where a set of tiles goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub columns: u32,
    pub rows: u32,
    /// A tile's picture, without its band.
    pub tile: (u32, u32),
    /// The band's height, above each tile.
    pub band: u32,
    /// The labels' scale: 1, or 2 on tiles of [`BIG_LABELS`] or wider.
    pub scale: u32,
    /// The whole image.
    pub size: (u32, u32),
}

impl Layout {
    /// The row and column of tile `i`, filled a row at a time.
    pub fn cell(&self, i: usize) -> (u32, u32) {
        let i = i as u32;
        (i / self.columns, i % self.columns)
    }

    /// The top left of tile `i`'s band; its picture is `band` below.
    pub fn origin(&self, i: usize) -> (u32, u32) {
        let (row, column) = self.cell(i);
        (
            column * (self.tile.0 + GUTTER),
            row * (self.band + self.tile.1 + GUTTER),
        )
    }
}

/// `n` tiles of `aspect` (width over height) laid out with their long
/// edge `edge` pixels long, at `columns` columns and labels at `scale`.
fn fit(n: u32, aspect: f64, edge: u32, columns: u32, scale: u32) -> Layout {
    let rows = n.div_ceil(columns);
    let band = band(scale);
    let row_room = |rows: u32| edge.saturating_sub((rows - 1) * GUTTER) / rows;
    // As wide as the edge allows, unless that is too tall.
    let w = edge.saturating_sub((columns - 1) * GUTTER) / columns;
    let h = ((f64::from(w) / aspect).round() as u32).max(1);
    let (w, h) = if rows * (band + h) + (rows - 1) * GUTTER <= edge {
        (w, h)
    } else {
        let h = row_room(rows).saturating_sub(band).max(1);
        (
            ((f64::from(h) * aspect).round() as u32).clamp(1, w.max(1)),
            h,
        )
    };
    Layout {
        columns,
        rows,
        tile: (w, h),
        band,
        scale,
        size: (
            columns * w + (columns - 1) * GUTTER,
            rows * (band + h) + (rows - 1) * GUTTER,
        ),
    }
}

/// `n` tiles of `aspect`, in the image whose long edge is `edge` pixels:
/// the column count whose image comes closest to the aspect `target`,
/// among those whose tiles are [`MIN_TILE_WIDTH`] wide or wider (a
/// label's second line needs it). Labels are twice the size on tiles of
/// [`BIG_LABELS`] or wider. `None` when no column count leaves tiles that
/// wide.
pub fn layout(n: u32, aspect: f64, edge: u32, target: f64) -> Option<Layout> {
    let best = |scale: u32| {
        (1..=n.max(1))
            .map(|c| fit(n.max(1), aspect, edge, c, scale))
            .filter(|l| l.tile.0 >= MIN_TILE_WIDTH)
            .min_by(|a, b| {
                let off = |l: &Layout| {
                    (f64::from(l.size.0) / f64::from(l.size.1) / target)
                        .ln()
                        .abs()
                };
                off(a).total_cmp(&off(b))
            })
    };
    match best(2) {
        Some(l) if l.tile.0 >= BIG_LABELS => Some(l),
        _ => best(1),
    }
}

/// The most tiles of `aspect` that fit in an image whose long edge is
/// `edge`, for the error that says how many do.
pub fn most_tiles(aspect: f64, edge: u32, target: f64, limit: u32) -> u32 {
    (1..=limit)
        .rev()
        .find(|&n| layout(n, aspect, edge, target).is_some())
        .unwrap_or(0)
}

/// An 8-bit RGB picture being drawn.
#[derive(Debug, Clone)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    rgb: Vec<u8>,
}

impl Canvas {
    /// A canvas of `width` × `height`, all [`BAND`].
    pub fn new(width: u32, height: u32) -> Canvas {
        let n = width as usize * height as usize;
        Canvas {
            width,
            height,
            rgb: BAND.repeat(n),
        }
    }

    fn at(&self, x: u32, y: u32) -> usize {
        (y as usize * self.width as usize + x as usize) * 3
    }

    /// Fill a rectangle, clipped to the canvas.
    pub fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, color: [u8; 3]) {
        for yy in y..(y + h).min(self.height) {
            for xx in x..(x + w).min(self.width) {
                let i = self.at(xx, yy);
                self.rgb[i..i + 3].copy_from_slice(&color);
            }
        }
    }

    /// `image`'s pixels with their top left at (`x`, `y`), clipped to the
    /// canvas. Only 8-bit pictures are drawn; look renders nothing else.
    pub fn put(&mut self, x: u32, y: u32, image: &Image) {
        let Pixels::Rgb8(px) = &image.pixels else {
            return;
        };
        let w = image.width.min(self.width.saturating_sub(x)) as usize;
        for row in 0..image.height.min(self.height.saturating_sub(y)) {
            let src = row as usize * image.width as usize * 3;
            let dst = self.at(x, y + row);
            self.rgb[dst..dst + w * 3].copy_from_slice(&px[src..src + w * 3]);
        }
    }

    /// `text` in [`INK`] from (`x`, `y`), its glyphs `scale` times their
    /// size, cut at the last glyph that ends within `max_width`.
    pub fn text(&mut self, x: u32, y: u32, scale: u32, text: &str, max_width: u32) {
        let step = CELL * scale;
        for (i, c) in text.chars().enumerate() {
            let x0 = x + i as u32 * step;
            if (i as u32 + 1) * step > max_width {
                break;
            }
            let g = font::glyph(c);
            for gy in 0..CELL {
                for gx in 0..CELL {
                    if font::ink(g, gx, gy) {
                        self.fill(x0 + gx * scale, y + gy * scale, scale, scale, INK);
                    }
                }
            }
        }
    }

    /// `text` as [`text`](Self::text) draws it, on a [`BAND`] backing one
    /// pixel larger all round, so that it reads over any picture.
    pub fn label(&mut self, x: u32, y: u32, scale: u32, text: &str, max_width: u32) {
        let glyphs = (max_width / (CELL * scale)).min(text.chars().count() as u32);
        if glyphs == 0 {
            return;
        }
        let (bx, by) = (x.saturating_sub(1), y.saturating_sub(1));
        self.fill(bx, by, glyphs * CELL * scale + 2, CELL * scale + 2, BAND);
        self.text(x, y, scale, text, max_width);
    }

    /// Blend the pixel at (`x`, `y`) `alpha` of the way to [`INK`].
    fn blend(&mut self, x: u32, y: u32, alpha: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = self.at(x, y);
        for (c, ink) in self.rgb[i..i + 3].iter_mut().zip(INK) {
            *c = (f32::from(*c) + (f32::from(ink) - f32::from(*c)) * alpha).round() as u8;
        }
    }

    /// A line `alpha` of the way to [`INK`] across `w` pixels from
    /// (`x`, `y`).
    pub fn hline(&mut self, x: u32, y: u32, w: u32, alpha: f32) {
        for xx in x..x + w {
            self.blend(xx, y, alpha);
        }
    }

    /// The same, down `h` pixels.
    pub fn vline(&mut self, x: u32, y: u32, h: u32, alpha: f32) {
        for yy in y..y + h {
            self.blend(x, yy, alpha);
        }
    }

    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }

    pub fn into_image(self) -> Image {
        Image {
            width: self.width,
            height: self.height,
            pixels: Pixels::Rgb8(self.rgb),
        }
    }
}

/// A tile's label band, its top left at (`x`, `y`) and `width` wide: two
/// lines of text at `scale`, each cut at the band's width.
pub fn caption(c: &mut Canvas, (x, y): (u32, u32), width: u32, scale: u32, lines: [&str; 2]) {
    let pad = PAD * scale;
    let room = width.saturating_sub(2 * pad);
    c.text(x + pad, y + pad, scale, lines[0], room);
    c.text(x + pad, y + 2 * pad + CELL * scale, scale, lines[1], room);
}

/// The lines of an equirect's yaw and pitch, every 30°.
pub const GRID_STEP: i32 = 30;

/// A yaw and pitch grid every [`GRID_STEP`] degrees over the equirect
/// tile at (`x`, `y`), `w` × `h`: yaw 0 at its centre and growing to the
/// right, pitch 90 at its top (D33). With `labels`, each line's angle is
/// written on it, yaw along the top and pitch down the left.
pub fn grid(c: &mut Canvas, (x, y): (u32, u32), (w, h): (u32, u32), alpha: f32, labels: bool) {
    let col = |yaw: i32| x + ((f64::from(yaw + 180) / 360.0) * f64::from(w)).round() as u32;
    let row = |pitch: i32| y + ((f64::from(90 - pitch) / 180.0) * f64::from(h)).round() as u32;
    let yaws = (-180 / GRID_STEP + 1..180 / GRID_STEP).map(|k| k * GRID_STEP);
    let pitches = (-90 / GRID_STEP + 1..90 / GRID_STEP).map(|k| k * GRID_STEP);
    for yaw in yaws.clone() {
        c.vline(col(yaw), y, h, alpha);
    }
    for pitch in pitches.clone() {
        c.hline(x, row(pitch), w, alpha);
    }
    if !labels {
        return;
    }
    let room = |s: &str| s.len() as u32 * CELL;
    for yaw in yaws {
        let s = yaw.to_string();
        let left = col(yaw) + 2;
        if left + room(&s) <= x + w {
            c.label(left, y + 2, 1, &s, room(&s));
        }
    }
    for pitch in pitches.filter(|&p| p != 0) {
        let s = pitch.to_string();
        let top = row(pitch).saturating_sub(CELL + 2);
        if top > y + CELL + 3 {
            c.label(x + 2, top, 1, &s, room(&s));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_of_24_at_768_has_5_columns() {
        // Amendment 16: 5 columns come closer to 16:9 than 6.
        let l = layout(24, 2.0, 768, 16.0 / 9.0).expect("a layout");
        assert_eq!((l.columns, l.rows), (5, 5), "{l:?}");
        assert!(l.tile.0 >= MIN_TILE_WIDTH);
        assert_eq!(l.scale, 1);
        assert_eq!(l.size.0, 768, "{l:?}");
        assert!(l.size.1 < 768, "{l:?}");
    }

    #[test]
    fn one_tile_fills_the_edge_with_big_labels() {
        let l = layout(1, 16.0 / 9.0, 768, 16.0 / 9.0).expect("a layout");
        assert_eq!((l.columns, l.rows, l.tile.0, l.scale), (1, 1, 768, 2));
        assert_eq!(l.tile.1, 432);
        assert_eq!(l.size, (768, 432 + band(2)));
    }

    #[test]
    fn tall_layouts_are_held_to_the_edge_too() {
        for n in 1..=24 {
            for aspect in [1.0, 16.0 / 9.0, 2.0, 0.5] {
                for edge in [256, 768, 1568, 4096] {
                    if let Some(l) = layout(n, aspect, edge, 16.0 / 9.0) {
                        assert!(
                            l.size.0 <= edge && l.size.1 <= edge,
                            "{n} {aspect} {edge}: {l:?}"
                        );
                        assert!(l.columns * l.rows >= n);
                        let last = l.origin(n as usize - 1);
                        assert!(last.0 + l.tile.0 <= l.size.0);
                        assert!(last.1 + l.band + l.tile.1 <= l.size.1);
                    }
                }
            }
        }
    }

    #[test]
    fn too_many_tiles_say_how_many_fit() {
        // At 256 pixels a 112-pixel tile fits twice a row.
        assert!(layout(24, 2.0, 256, 16.0 / 9.0).is_none());
        let most = most_tiles(2.0, 256, 16.0 / 9.0, 24);
        assert!(most > 0 && layout(most, 2.0, 256, 16.0 / 9.0).is_some());
        assert!(layout(most + 1, 2.0, 256, 16.0 / 9.0).is_none());
    }

    #[test]
    fn text_is_ink_on_the_band_and_cut_at_its_width() {
        let mut c = Canvas::new(40, 10);
        c.text(0, 0, 1, "11111", 20);
        let inked = |c: &Canvas, x0: u32, x1: u32| {
            (x0..x1).any(|x| (0..8).any(|y| c.rgb()[c.at(x, y)] == INK[0]))
        };
        assert!(inked(&c, 0, 8) && inked(&c, 8, 16), "two glyphs fit in 20");
        assert!(!inked(&c, 16, 40), "the third doesn't");
    }

    #[test]
    fn a_caption_stays_in_its_band() {
        for scale in [1, 2] {
            let (w, h) = (120 * scale, band(scale) + 10);
            let mut c = Canvas::new(w, h);
            caption(
                &mut c,
                (0, 0),
                w,
                scale,
                ["12.512s f375", "y90 p-20 f100 and on"],
            );
            let ink = |x: u32, y: u32| c.rgb()[c.at(x, y)] == INK[0];
            assert!((0..w).any(|x| (0..band(scale)).any(|y| ink(x, y))));
            for y in band(scale)..h {
                assert!((0..w).all(|x| !ink(x, y)), "ink below the band at {y}");
            }
            // Cut at the band's width, less its padding.
            let edge = w - PAD * scale;
            for x in edge..w {
                assert!((0..h).all(|y| !ink(x, y)), "ink past the padding at {x}");
            }
        }
    }

    #[test]
    fn the_grid_crosses_at_the_centre() {
        let mut c = Canvas::new(360, 180);
        c.fill(0, 0, 360, 180, [0, 0, 0]);
        grid(&mut c, (0, 0), (360, 180), 1.0, false);
        // yaw 0 and pitch 0 are lines; yaw 15 and pitch 15 are not.
        assert_eq!(c.rgb()[c.at(180, 45) + 1], INK[1]);
        assert_eq!(c.rgb()[c.at(45, 90) + 1], INK[1]);
        assert_eq!(c.rgb()[c.at(195, 75) + 1], 0);
        // ±180 is the edge, not a line.
        assert_eq!(c.rgb()[c.at(0, 75) + 1], 0);
    }
}
