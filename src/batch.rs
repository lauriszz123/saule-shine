//! `SpriteBatch` — many copies of one texture, drawn with one call.
//!
//! Love2D batches to cut GPU draw calls. Here the win is on the Saule side:
//! a tile map of a few thousand cells is built once, and each frame costs one
//! `Graphics.draw(batch)` instead of a few thousand native calls.

use saule_sdk::prelude::*;
use saule_sdk::saule_export;

use crate::objects::{Place, Quad, texture_handle};
use crate::raster::Rect;
use crate::state;

/// One sprite: where it goes, which part of the texture, and its tint.
#[derive(Clone, Copy)]
struct Sprite {
    frame: Option<Rect>,
    at: Place,
    color: [f64; 4],
}

/// Many copies of one `Image` or `Canvas`, each placed, rotated, scaled,
/// tinted and optionally cut by a `Quad`, drawn in one call with
/// `Graphics.draw(batch)`.
#[saule_class]
pub struct SpriteBatch {
    /// Keeps the texture alive for as long as the batch is.
    _texture: SAnyObject,
    handle: i64,
    sprites: Vec<Sprite>,
    color: [f64; 4],
    limit: usize,
}

impl SpriteBatch {
    fn place(&mut self, frame: Option<Rect>, at: Place) -> Result<i64, String> {
        if self.sprites.len() >= self.limit {
            return Err(format!(
                "SpriteBatch.add: the batch is full ({} sprites); give newSpriteBatch a larger size",
                self.limit
            ));
        }
        self.sprites.push(Sprite {
            frame,
            at,
            color: self.color,
        });
        Ok(self.sprites.len() as i64)
    }

    fn replace(&mut self, id: i64, frame: Option<Rect>, at: Place) -> Result<(), String> {
        let color = self.color;
        let slot = usize::try_from(id - 1)
            .ok()
            .and_then(|i| self.sprites.get_mut(i))
            .ok_or_else(|| format!("SpriteBatch.set: no sprite {id}"))?;
        *slot = Sprite { frame, at, color };
        Ok(())
    }

    /// Draw every sprite, with the whole batch placed at `at`.
    pub(crate) fn draw(&self, at: Place) -> Result<(), String> {
        state::draw(|gfx| {
            gfx.push(false);
            gfx.translate(at.x, at.y);
            gfx.rotate(at.r);
            gfx.scale(at.sx, at.sy);
            gfx.translate(-at.ox, -at.oy);
            let (r, g, b, a) = gfx.color();
            let mut outcome = Ok(());
            for s in &self.sprites {
                gfx.set_color(
                    r * s.color[0],
                    g * s.color[1],
                    b * s.color[2],
                    a * s.color[3],
                );
                let p = s.at;
                outcome = match s.frame {
                    Some(f) => {
                        gfx.draw_frame(self.handle, f, p.x, p.y, p.r, p.sx, p.sy, p.ox, p.oy)
                    }
                    None => gfx.draw_canvas(self.handle, p.x, p.y, p.r, p.sx, p.sy, p.ox, p.oy),
                };
                if outcome.is_err() {
                    break;
                }
            }
            gfx.set_color(r, g, b, a);
            gfx.pop()?;
            outcome
        })?
    }
}

#[saule_methods]
impl SpriteBatch {
    /// Add the whole texture at `x, y` and return the sprite's id.
    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        x: Option<f64>,
        y: Option<f64>,
        r: Option<f64>,
        sx: Option<f64>,
        sy: Option<f64>,
        ox: Option<f64>,
        oy: Option<f64>,
    ) -> Result<i64, String> {
        self.place(None, Place::new(x, y, r, sx, sy, ox, oy))
    }

    /// Add the part of the texture `quad` selects, and return the sprite's
    /// id. Love2D's `add(quad, …)`.
    #[allow(clippy::too_many_arguments)]
    pub fn addq(
        &mut self,
        quad: &Quad,
        x: Option<f64>,
        y: Option<f64>,
        r: Option<f64>,
        sx: Option<f64>,
        sy: Option<f64>,
        ox: Option<f64>,
        oy: Option<f64>,
    ) -> Result<i64, String> {
        self.place(Some(quad.rect()), Place::new(x, y, r, sx, sy, ox, oy))
    }

    /// Move sprite `id` to a new place, showing the whole texture.
    #[allow(clippy::too_many_arguments)]
    pub fn set(
        &mut self,
        id: i64,
        x: Option<f64>,
        y: Option<f64>,
        r: Option<f64>,
        sx: Option<f64>,
        sy: Option<f64>,
        ox: Option<f64>,
        oy: Option<f64>,
    ) -> Result<(), String> {
        self.replace(id, None, Place::new(x, y, r, sx, sy, ox, oy))
    }

    /// Move sprite `id` to a new place, showing `quad`.
    #[allow(clippy::too_many_arguments)]
    pub fn setq(
        &mut self,
        id: i64,
        quad: &Quad,
        x: Option<f64>,
        y: Option<f64>,
        r: Option<f64>,
        sx: Option<f64>,
        sy: Option<f64>,
        ox: Option<f64>,
        oy: Option<f64>,
    ) -> Result<(), String> {
        self.replace(id, Some(quad.rect()), Place::new(x, y, r, sx, sy, ox, oy))
    }

    /// Remove every sprite.
    pub fn clear(&mut self) {
        self.sprites.clear();
    }

    /// How many sprites the batch holds.
    pub fn get_count(&self) -> i64 {
        self.sprites.len() as i64
    }

    /// How many it can hold.
    pub fn get_buffer_size(&self) -> i64 {
        self.limit as i64
    }

    /// The tint for sprites added from now on, multiplied with the colour
    /// current when the batch is drawn. No arguments goes back to white.
    pub fn set_color(&mut self, r: Option<f64>, g: Option<f64>, b: Option<f64>, a: Option<f64>) {
        self.color = [
            r.unwrap_or(1.0),
            g.unwrap_or(1.0),
            b.unwrap_or(1.0),
            a.unwrap_or(1.0),
        ];
    }

    /// `local r, g, b, a = batch.getColor()`.
    pub fn get_color(&self) -> (f64, f64, f64, f64) {
        let [r, g, b, a] = self.color;
        (r, g, b, a)
    }
}

/// `Graphics.newSpriteBatch(texture [, size])` — a batch of an `Image` or
/// `Canvas` that holds up to `size` sprites (1000 by default).
#[saule_export(class = "Graphics", name = "newSpriteBatch")]
fn graphics_new_sprite_batch(texture: SValue, size: Option<i64>) -> Result<SpriteBatch, String> {
    let handle = texture_handle(&texture, "Graphics.newSpriteBatch")?;
    let obj = texture
        .as_object()
        .cloned()
        .ok_or("Graphics.newSpriteBatch: expected an Image or a Canvas")?;
    let limit = size.unwrap_or(1000);
    if limit <= 0 {
        return Err("Graphics.newSpriteBatch: the size must be positive".into());
    }
    Ok(SpriteBatch {
        _texture: obj,
        handle,
        sprites: Vec::with_capacity(limit.min(4096) as usize),
        color: [1.0; 4],
        limit: limit as usize,
    })
}
