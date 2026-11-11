//! The drawable objects, as Love2D has them: `Image`, `Canvas`, `Font` and
//! `Quad`, and the `Graphics.*` functions that make, select and draw them.
//!
//! Each object owns its slot in the renderer's registries. When the last
//! Saule value holding one goes away its `Drop` queues the slot to be freed at
//! the next `Graphics.present` (see [`state::release_later`]), so nothing is
//! freed while a draw call is using it, and nothing has to be released by
//! hand.
//!
//! The objects a program *binds* — the canvas being drawn into, the selected
//! font — are also held here, so a binding keeps its object alive however
//! the program's own references come and go. That is Love2D's rule too.

use std::cell::RefCell;

use saule_sdk::prelude::*;
use saule_sdk::saule_export;

use crate::batch::SpriteBatch;
use crate::particles::ParticleSystem;
use crate::raster::Rect;
use crate::state::{self, Res};

thread_local! {
    /// What `Graphics.setCanvas` bound, so `getCanvas` hands back the same
    /// object and the binding keeps it alive.
    static CANVAS: RefCell<Option<SObject<Canvas>>> = const { RefCell::new(None) };
    /// What `Graphics.setFont` selected; `None` is the default face.
    static FONT: RefCell<Option<SObject<Font>>> = const { RefCell::new(None) };
}

/// Forget every binding: a new window starts with fresh registries, and an
/// object of the old window's names a slot that no longer exists.
pub(crate) fn reset_bindings() {
    let _ = CANVAS.try_with(|c| c.borrow_mut().take());
    let _ = FONT.try_with(|f| f.borrow_mut().take());
}

// ---------------------------------------------------------------------------
// Image
// ---------------------------------------------------------------------------

/// A picture loaded from a PNG, drawn with `Graphics.draw(image, …)`.
#[saule_class]
pub struct Image {
    pub(crate) handle: i64,
    w: i64,
    h: i64,
}

impl Drop for Image {
    fn drop(&mut self) {
        state::release_later(Res::Surface(self.handle));
    }
}

impl Image {
    fn from_handle(handle: i64) -> Result<Self, String> {
        let (w, h) = state::draw(|gfx| gfx.image_size(handle))??;
        Ok(Image { handle, w, h })
    }
}

#[saule_methods]
impl Image {
    /// Width in pixels.
    pub fn get_width(&self) -> i64 {
        self.w
    }

    /// Height in pixels.
    pub fn get_height(&self) -> i64 {
        self.h
    }

    /// `local w, h = image.getDimensions()`.
    pub fn get_dimensions(&self) -> (i64, i64) {
        (self.w, self.h)
    }

    /// Write the image to `path` as a PNG.
    pub fn save(&self, path: &str) -> Result<(), String> {
        state::draw(|gfx| gfx.save_image(self.handle, path))?
    }
}

// ---------------------------------------------------------------------------
// Canvas
// ---------------------------------------------------------------------------

/// An offscreen surface to draw into — bind it with `Graphics.setCanvas`, or
/// fill it with `canvas.renderTo(fn)` — and then draw like an image.
#[saule_class]
pub struct Canvas {
    pub(crate) handle: i64,
    w: i64,
    h: i64,
}

impl Drop for Canvas {
    fn drop(&mut self) {
        state::release_later(Res::Surface(self.handle));
    }
}

#[saule_methods]
impl Canvas {
    /// Width in pixels.
    pub fn get_width(&self) -> i64 {
        self.w
    }

    /// Height in pixels.
    pub fn get_height(&self) -> i64 {
        self.h
    }

    /// `local w, h = canvas.getDimensions()`.
    pub fn get_dimensions(&self) -> (i64, i64) {
        (self.w, self.h)
    }

    /// Draw into this canvas for the length of `f`, then go back to whatever
    /// target was bound before.
    #[saule(sig(f = "fn() -> nil"))]
    pub fn render_to(&self, f: SFunction) -> Result<(), String> {
        let previous = state::draw(|gfx| gfx.get_canvas())?;
        state::draw(|gfx| gfx.set_canvas(Some(self.handle)))??;
        // The engine is not borrowed while `f` runs, so it may draw freely.
        let outcome = f.call(&[]);
        state::draw(|gfx| gfx.set_canvas(Some(previous)))??;
        outcome.map(|_| ())
    }

    /// Write the canvas's pixels to `path` as a PNG.
    pub fn save(&self, path: &str) -> Result<(), String> {
        state::draw(|gfx| gfx.save_image(self.handle, path))?
    }
}

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

/// A typeface at one size. `Graphics.setFont(font)` selects it for `print`.
#[saule_class]
pub struct Font {
    pub(crate) handle: i64,
    size: f64,
}

impl Drop for Font {
    fn drop(&mut self) {
        // Handle 0 is the default face, which nothing allocated.
        if self.handle != 0 {
            state::release_later(Res::Font(self.handle));
        }
    }
}

#[saule_methods]
impl Font {
    /// Baseline-to-baseline line height in pixels.
    pub fn get_height(&self) -> Result<f64, String> {
        state::draw(|gfx| gfx.with_font(self.handle, |f| f.height()))?
    }

    /// The width `text` would take, in pixels — the longest line if it has
    /// several.
    pub fn get_width(&self, text: &str) -> Result<f64, String> {
        state::draw(|gfx| gfx.with_font(self.handle, |f| f.measure(text)))?
    }

    /// The size it was loaded at.
    pub fn get_size(&self) -> f64 {
        self.size
    }
}

// ---------------------------------------------------------------------------
// Quad
// ---------------------------------------------------------------------------

/// A rectangle of a texture — one cell of a sprite sheet or a tile set —
/// drawn with `Graphics.drawq` or added to a `SpriteBatch`.
#[saule_class]
#[derive(Clone, Copy)]
pub struct Quad {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
    sw: f64,
    sh: f64,
}

impl Quad {
    pub(crate) fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }
}

#[saule_methods]
impl Quad {
    /// `Quad(x, y, width, height, sw, sh)`: the rectangle `x, y, width,
    /// height` of a `sw` × `sh` texture.
    pub fn new(x: f64, y: f64, width: f64, height: f64, sw: f64, sh: f64) -> Result<Self, String> {
        if width <= 0.0 || height <= 0.0 {
            return Err("Quad: width and height must be positive".into());
        }
        Ok(Quad {
            x,
            y,
            w: width,
            h: height,
            sw,
            sh,
        })
    }

    /// Move or resize the rectangle.
    pub fn set_viewport(&mut self, x: f64, y: f64, width: f64, height: f64) {
        self.x = x;
        self.y = y;
        self.w = width;
        self.h = height;
    }

    /// `local x, y, w, h = quad.getViewport()`.
    pub fn get_viewport(&self) -> (f64, f64, f64, f64) {
        (self.x, self.y, self.w, self.h)
    }

    /// The texture size the quad was made for.
    pub fn get_texture_dimensions(&self) -> (f64, f64) {
        (self.sw, self.sh)
    }
}

// ---------------------------------------------------------------------------
// Making them
// ---------------------------------------------------------------------------

/// `Graphics.newImage(path)` — load a PNG.
#[saule_export(class = "Graphics", name = "newImage")]
fn graphics_new_image(path: String) -> Result<Image, String> {
    let handle = state::draw(|gfx| gfx.new_image(&path))??;
    Image::from_handle(handle)
}

/// `Graphics.loadImage(path)` — `newImage`, with `nil` for a file that cannot
/// be loaded instead of an error.
#[saule_export(class = "Graphics", name = "loadImage")]
fn graphics_load_image(path: String) -> Result<Option<Image>, String> {
    match state::draw(|gfx| gfx.try_new_image(&path))? {
        Some(handle) => Image::from_handle(handle).map(Some),
        None => Ok(None),
    }
}

/// `Graphics.newImageFromBase64(data)` — decode a base64 PNG, for an asset
/// embedded in the source.
#[saule_export(class = "Graphics", name = "newImageFromBase64")]
fn graphics_new_image_from_base64(data: String) -> Result<Image, String> {
    let handle = state::draw(|gfx| gfx.new_image_from_base64(&data))??;
    Image::from_handle(handle)
}

/// `Graphics.newCanvas(width, height)` — a transparent offscreen surface.
#[saule_export(class = "Graphics", name = "newCanvas")]
fn graphics_new_canvas(width: i64, height: i64) -> Result<Canvas, String> {
    let handle = state::draw(|gfx| gfx.new_canvas(width, height))??;
    Ok(Canvas {
        handle,
        w: width,
        h: height,
    })
}

/// `Graphics.newFont(size [, path])` — a TrueType face at `size`. Without a
/// path, the system's UI face.
#[saule_export(class = "Graphics", name = "newFont")]
fn graphics_new_font(size: f64, path: Option<String>) -> Result<Font, String> {
    let handle = state::draw(|gfx| gfx.new_font(size, path.as_deref()))?
        .map_err(|e| format!("Graphics.newFont: {e}"))?;
    Ok(Font { handle, size })
}

/// `Graphics.loadFont(size [, path])` — `newFont`, with `nil` instead of an
/// error for a face that cannot be loaded.
#[saule_export(class = "Graphics", name = "loadFont")]
fn graphics_load_font(size: f64, path: Option<String>) -> Result<Option<Font>, String> {
    Ok(state::draw(|gfx| gfx.try_new_font(size, path.as_deref()))?
        .map(|handle| Font { handle, size }))
}

/// `Graphics.newQuad(x, y, width, height, sw, sh)` — the same as
/// `Quad(…)`, under Love2D's name.
#[saule_export(class = "Graphics", name = "newQuad")]
fn graphics_new_quad(
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    sw: f64,
    sh: f64,
) -> Result<Quad, String> {
    Quad::new(x, y, width, height, sw, sh)
}

// ---------------------------------------------------------------------------
// Binding them
// ---------------------------------------------------------------------------

/// `Graphics.setCanvas([canvas])` — draw into `canvas` from now on; no
/// argument goes back to the screen.
#[saule_export(class = "Graphics", name = "setCanvas")]
fn graphics_set_canvas(canvas: Option<SObject<Canvas>>) -> Result<(), String> {
    let handle = match &canvas {
        Some(c) => Some(c.borrow()?.handle),
        None => None,
    };
    state::draw(|gfx| gfx.set_canvas(handle))??;
    CANVAS.with(|c| *c.borrow_mut() = canvas);
    Ok(())
}

/// `Graphics.getCanvas()` — the canvas being drawn into, or `nil` for the
/// screen.
#[saule_export(class = "Graphics", name = "getCanvas")]
fn graphics_get_canvas() -> Option<SObject<Canvas>> {
    CANVAS.with(|c| c.borrow().clone())
}

/// `Graphics.setFont([font])` — the face `print` and `printf` use. No
/// argument goes back to the default face.
#[saule_export(class = "Graphics", name = "setFont")]
fn graphics_set_font(font: Option<SObject<Font>>) -> Result<(), String> {
    let handle = match &font {
        Some(f) => f.borrow()?.handle,
        None => 0,
    };
    state::draw(|gfx| gfx.set_font(handle))??;
    FONT.with(|f| *f.borrow_mut() = font);
    Ok(())
}

/// `Graphics.setNewFont(size [, path])` — `newFont` and `setFont` in one.
#[saule_export(class = "Graphics", name = "setNewFont")]
fn graphics_set_new_font(size: f64, path: Option<String>) -> Result<SObject<Font>, String> {
    let font = SObject::new(graphics_new_font(size, path)?);
    graphics_set_font(Some(font.clone()))?;
    Ok(font)
}

/// `Graphics.getFont()` — the selected face.
#[saule_export(class = "Graphics", name = "getFont")]
fn graphics_get_font() -> SObject<Font> {
    FONT.with(|f| {
        f.borrow_mut()
            .get_or_insert_with(|| {
                SObject::new(Font {
                    handle: 0,
                    size: crate::font::DEFAULT_SIZE,
                })
            })
            .clone()
    })
}

// ---------------------------------------------------------------------------
// Drawing them
// ---------------------------------------------------------------------------

/// The registry handle behind an `Image` or a `Canvas`, which are the two
/// textures.
pub(crate) fn texture_handle(value: &SValue, func: &str) -> Result<i64, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("{func}: expected an Image or a Canvas"))?;
    if let Some(img) = obj.downcast::<Image>() {
        return Ok(img.borrow()?.handle);
    }
    if let Some(canvas) = obj.downcast::<Canvas>() {
        return Ok(canvas.borrow()?.handle);
    }
    Err(format!(
        "{func}: expected an Image or a Canvas, got a {}",
        obj.class_name()
    ))
}

/// Where and how to draw: Love2D's `x, y, r, sx, sy, ox, oy`.
#[derive(Clone, Copy)]
pub(crate) struct Place {
    pub x: f64,
    pub y: f64,
    pub r: f64,
    pub sx: f64,
    pub sy: f64,
    pub ox: f64,
    pub oy: f64,
}

impl Place {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        x: Option<f64>,
        y: Option<f64>,
        r: Option<f64>,
        sx: Option<f64>,
        sy: Option<f64>,
        ox: Option<f64>,
        oy: Option<f64>,
    ) -> Self {
        let sx = sx.unwrap_or(1.0);
        Place {
            x: x.unwrap_or(0.0),
            y: y.unwrap_or(0.0),
            r: r.unwrap_or(0.0),
            sx,
            sy: sy.unwrap_or(sx),
            ox: ox.unwrap_or(0.0),
            oy: oy.unwrap_or(0.0),
        }
    }
}

/// `Graphics.draw(drawable [, x, y, r, sx, sy, ox, oy])` — draw an `Image`,
/// `Canvas`, `SpriteBatch` or `ParticleSystem`, tinted by the current colour.
/// `r` is a rotation in radians, `sx, sy` a scale (`sy` defaults to `sx`),
/// and `ox, oy` the origin within the drawable that lands on `x, y`.
#[allow(clippy::too_many_arguments)]
#[saule_export(class = "Graphics", name = "draw")]
fn graphics_draw(
    drawable: SValue,
    x: Option<f64>,
    y: Option<f64>,
    r: Option<f64>,
    sx: Option<f64>,
    sy: Option<f64>,
    ox: Option<f64>,
    oy: Option<f64>,
) -> Result<(), String> {
    let at = Place::new(x, y, r, sx, sy, ox, oy);
    let obj = drawable
        .as_object()
        .ok_or("Graphics.draw: expected an Image, Canvas, SpriteBatch or ParticleSystem")?;
    if let Some(batch) = obj.downcast::<SpriteBatch>() {
        return batch.borrow()?.draw(at);
    }
    if let Some(ps) = obj.downcast::<ParticleSystem>() {
        return ps.borrow()?.draw(at);
    }
    let handle = texture_handle(&drawable, "Graphics.draw")?;
    state::draw(|gfx| gfx.draw_canvas(handle, at.x, at.y, at.r, at.sx, at.sy, at.ox, at.oy))?
}

/// `Graphics.drawq(texture, quad [, x, y, r, sx, sy, ox, oy])` — draw the
/// part of an `Image` or `Canvas` that `quad` selects. Love2D 11 spells this
/// `draw(texture, quad, …)`; Saule has no overloading, so it keeps Love2D
/// 0.8's name.
#[allow(clippy::too_many_arguments)]
#[saule_export(class = "Graphics", name = "drawq")]
fn graphics_drawq(
    texture: SValue,
    quad: &Quad,
    x: Option<f64>,
    y: Option<f64>,
    r: Option<f64>,
    sx: Option<f64>,
    sy: Option<f64>,
    ox: Option<f64>,
    oy: Option<f64>,
) -> Result<(), String> {
    let at = Place::new(x, y, r, sx, sy, ox, oy);
    let handle = texture_handle(&texture, "Graphics.drawq")?;
    state::draw(|gfx| {
        gfx.draw_frame(
            handle,
            quad.rect(),
            at.x,
            at.y,
            at.r,
            at.sx,
            at.sy,
            at.ox,
            at.oy,
        )
    })?
}

/// `Graphics.captureScreenshot(path)` — write what is on screen to `path` as
/// a PNG.
#[saule_export(class = "Graphics", name = "captureScreenshot")]
fn graphics_capture_screenshot(path: String) -> Result<(), String> {
    state::draw(|gfx| gfx.save_image(0, &path))?
}
