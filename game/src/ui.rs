//! GBA-style UI drawn as screen-space quads: Emerald's own latin_normal font
//! (extracted into the world pack) plus solid fills from the font strip's
//! extra white cell. Everything is laid out in GBA-logical pixels and drawn
//! at one integer scale — `scale_for` picks the largest whole multiple of the
//! 240x160 screen that fits, so the UI stays crisp on any window.

use types::{Flip, Quad, Rect, Rgba, TextureId};

/// Slot of the solid-white 16x16 cell appended to the font strip.
const SOLID: usize = 95;
const LAYER_UI: i16 = 100;

/// Emerald's standard text colors: dark gray fill, light gray shadow (the
/// shadow is baked into the glyphs; this is the reference for other UI).
pub const TEXT: Rgba = Rgba { r: 255, g: 255, b: 255, a: 255 }; // glyphs pre-colored
const WINDOW_FILL: Rgba = Rgba { r: 255, g: 255, b: 255, a: 255 };
const WINDOW_BORDER: Rgba = Rgba { r: 88, g: 96, b: 120, a: 255 };
const CURSOR: Rgba = Rgba { r: 96, g: 96, b: 104, a: 255 };
const DISABLED: Rgba = Rgba { r: 255, g: 255, b: 255, a: 96 };

pub struct Ui<'a> {
    pub tex: TextureId,
    pub widths: &'a [u8],
    pub scale: f32,
}

impl Ui<'_> {
    /// Largest integer scale at which a 240x160 GBA layout fits the screen.
    pub fn scale_for(screen_w: f32, screen_h: f32) -> f32 {
        (screen_w / 240.0).min(screen_h / 160.0).floor().max(1.0)
    }

    fn advance(&self, ch: u8) -> f32 {
        let slot = (ch as usize).wrapping_sub(32);
        self.widths.get(slot).copied().unwrap_or(6).max(3) as f32
    }

    /// Text width in GBA-logical pixels.
    pub fn text_width(&self, s: &str) -> f32 {
        s.bytes().map(|c| self.advance(c)).sum()
    }

    /// Draw text at logical (x, y); returns the logical end x.
    pub fn text(&self, out: &mut Vec<Quad>, s: &str, x: f32, y: f32, tint: Rgba) -> f32 {
        let mut cx = x;
        for ch in s.bytes() {
            let slot = (ch as usize).wrapping_sub(32);
            if slot < SOLID && ch != b' ' {
                out.push(Quad {
                    tex: self.tex,
                    src: Rect::new(slot as f32 * 16.0, 0.0, 16.0, 16.0),
                    dst: Rect::new(
                        cx * self.scale,
                        y * self.scale,
                        16.0 * self.scale,
                        16.0 * self.scale,
                    ),
                    layer: LAYER_UI,
                    tint,
                    flip: Flip::None,
                });
            }
            cx += self.advance(ch);
        }
        cx
    }

    /// Solid rectangle in logical pixels (the font strip's white cell).
    pub fn solid(&self, out: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32, tint: Rgba) {
        out.push(Quad {
            tex: self.tex,
            src: Rect::new(SOLID as f32 * 16.0 + 4.0, 4.0, 8.0, 8.0),
            dst: Rect::new(x * self.scale, y * self.scale, w * self.scale, h * self.scale),
            layer: LAYER_UI,
            tint,
            flip: Flip::None,
        });
    }

    /// A GBA-style window: white body with a thin dark border.
    pub fn window(&self, out: &mut Vec<Quad>, x: f32, y: f32, w: f32, h: f32) {
        self.solid(out, x - 1.0, y - 1.0, w + 2.0, h + 2.0, WINDOW_BORDER);
        self.solid(out, x, y, w, h, WINDOW_FILL);
    }

    /// Emerald's right-pointing menu cursor, built from pixel columns.
    pub fn cursor(&self, out: &mut Vec<Quad>, x: f32, y: f32) {
        for i in 0..5 {
            let fi = i as f32;
            self.solid(out, x + fi, y + fi, 1.0, 9.0 - 2.0 * fi, CURSOR);
        }
    }

    /// The start menu: top-right window, one row per item, cursor on
    /// `selected`. Items render dimmed when disabled.
    pub fn menu(
        &self,
        out: &mut Vec<Quad>,
        items: &[(&str, bool)],
        selected: u8,
        screen_w: f32,
    ) {
        const ROW: f32 = 16.0;
        let text_w = items.iter().map(|(s, _)| self.text_width(s)).fold(0.0, f32::max);
        let w = 10.0 + text_w + 6.0;
        let h = items.len() as f32 * ROW + 6.0;
        let x = screen_w / self.scale - w - 5.0;
        let y = 5.0;

        self.window(out, x, y, w, h);
        for (i, (label, enabled)) in items.iter().enumerate() {
            let ry = y + 3.0 + i as f32 * ROW;
            self.text(out, label, x + 10.0, ry, if *enabled { TEXT } else { DISABLED });
            if i as u8 == selected {
                self.cursor(out, x + 3.0, ry + 3.0);
            }
        }
    }
}
