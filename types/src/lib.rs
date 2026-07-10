//! The seam between the game and every backend: plain data we own, plus the two
//! port traits. See README.md — this crate is deliberately tiny and must stay so.
//! No backend crate's vocabulary may appear here, and `Quad`/`Backend` do not grow
//! as the game grows richer; only the code that *produces* quads does.

/// Handle to an atlas previously uploaded via [`Backend::load_atlas`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(pub u32);

impl TextureId {
    /// Reserved id: in a frame's screen pass, this samples the offscreen
    /// target the offscreen pass just rendered.
    pub const TARGET: TextureId = TextureId(u32::MAX);
}

/// An axis-aligned rectangle. For `Quad::dst` this is a screen rect with camera
/// pan + zoom already applied, so fractional values are expected and fine.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// Color multiply applied to a quad; [`Rgba::WHITE`] leaves the texture untouched.
/// One field buys day/night shifts, weather darkening, and fade transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const WHITE: Rgba = Rgba { r: 255, g: 255, b: 255, a: 255 };
    pub const BLACK: Rgba = Rgba { r: 0, g: 0, b: 0, a: 255 };
}

impl Default for Rgba {
    fn default() -> Self {
        Rgba::WHITE
    }
}

/// Core GBA tile/sprite semantics: a left-facing sprite is the right frame flipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Flip {
    #[default]
    None,
    H,
    V,
    Both,
}

/// Everything Emerald draws is one of these: a textured quad from an atlas, placed
/// at a screen rect, on a layer, optionally tinted and/or flipped. The game fills a
/// `Vec<Quad>`; the backend stable-sorts by `layer` and blits. Do not add a field
/// until the pret data forces it (README: "Why each field earns its place").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    /// An uploaded atlas.
    pub tex: TextureId,
    /// Sub-rectangle of that atlas, in pixels.
    pub src: Rect,
    /// Screen rect — camera (pan + zoom) already applied; may be fractional.
    pub dst: Rect,
    /// Draw order / GBA BG-OAM priority; the backend stable-sorts by this, so vec
    /// order breaks ties within a layer.
    pub layer: i16,
    /// Color multiply; WHITE = untouched.
    pub tint: Rgba,
    pub flip: Flip,
}

/// One frame's worth of input, in the game's own words (GBA pad + the zoom axis).
/// Fields are "held this frame"; the game derives presses/releases by comparing
/// against the previous frame's input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Input {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub a: bool,
    pub b: bool,
    pub start: bool,
    pub select: bool,
    /// Smooth continuous overworld zoom is a day-one feature (README).
    pub zoom_in: bool,
    pub zoom_out: bool,
}

/// One frame, as plain data: an offscreen pass rendered into a target of
/// `target_size` pixels, then a screen pass composited to the window — the
/// "offscreen render target + one post pass" the README requires for
/// crisp+smooth zoom, realm transitions, and full-screen effects. In the
/// screen pass, [`TextureId::TARGET`] samples the offscreen target.
///
/// The world is drawn at an integer pixel scale into the target (nearest
/// sampling stays crisp) and the screen pass maps it to the continuous zoom
/// (linear minification stays smooth). UI quads ride in the screen pass.
#[derive(Debug, Default, PartialEq)]
pub struct Frame {
    pub offscreen: Vec<Quad>,
    pub target_size: (u32, u32),
    pub screen: Vec<Quad>,
}

impl Frame {
    pub fn clear(&mut self) {
        self.offscreen.clear();
        self.screen.clear();
        self.target_size = (0, 0);
    }
}

/// The only rendering seam. Adapters implement this in one file under `backends/`
/// and own the event loop; the game never sees the implementing crate.
pub trait Backend {
    /// Upload an RGBA8 atlas (indexed pret graphics are baked to RGBA *before*
    /// this call — the backend never learns what a palette is).
    fn load_atlas(&mut self, rgba: &[u8], w: u32, h: u32) -> TextureId;
    fn poll_input(&mut self) -> Input;
    /// Per pass: stable-sort by layer, clear, blit all; then present.
    fn draw_frame(&mut self, frame: &Frame);
}

/// The only audio seam: a PCM sink the m4a engine pushes samples into.
/// Adapters: cpal (native), WebAudio (web).
pub trait AudioSink {
    fn queue(&mut self, samples: &[i16]);
}
