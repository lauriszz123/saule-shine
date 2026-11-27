//! The glyph atlas: every glyph the GPU has drawn, rasterized once by the
//! CPU (fontdue, through `FontRes`) and kept in one texture, so a line of
//! text is one draw from one texture.
//!
//! Glyphs are packed in shelves (rows as tall as their tallest glyph), each
//! with a border of empty texels: a glyph drawn scaled or turned is
//! filtered, and the filter must read nothing but zero past its edges, as
//! the CPU's sampler does. When the atlas is full it grows, up to a limit,
//! and past that starts again empty: glyphs come back as they are drawn.

use std::collections::HashMap;

use glow::HasContext;

/// Empty texels around each glyph.
pub(crate) const BORDER: usize = 2;

const START: usize = 512;
/// As large as an atlas is ever made, where the chip takes textures that
/// large; on one that does not (2048 a side on Intel's third generation)
/// the chip's own limit, asked for when the atlas is made. Grown past
/// what the chip takes, an atlas could not be made anew at all, and
/// nothing more was ever kept in it.
const LARGEST: usize = 4096;

/// Which glyph: its font (slot and tag, so a released font's glyphs are
/// never taken for a new font's) and its character.
pub(crate) type GlyphKey = (u64, char);

pub(crate) struct Atlas {
    pub(crate) texture: glow::Texture,
    pub(crate) size: usize,
    /// The largest it may grow to: [`LARGEST`], or less on a chip whose
    /// textures stop short of that.
    largest: usize,
    /// Where each glyph is: the corner of its pixels (inside the border).
    places: HashMap<GlyphKey, (usize, usize)>,
    /// The shelf being filled: its top, how far along it is, and its height.
    shelf_y: usize,
    shelf_x: usize,
    shelf_h: usize,
}

impl Atlas {
    pub(crate) fn new(gl: &glow::Context) -> Result<Atlas, String> {
        let texture = make_texture(gl, START)?;
        // SAFETY: a GL call on the current context.
        let limit = unsafe { gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE) };
        Ok(Atlas {
            texture,
            size: START,
            largest: usize::try_from(limit).map_or(START, |limit| limit.clamp(START, LARGEST)),
            places: HashMap::new(),
            shelf_y: 0,
            shelf_x: 0,
            shelf_h: 0,
        })
    }

    pub(crate) fn delete(&self, gl: &glow::Context) {
        // SAFETY: made on this context.
        unsafe { gl.delete_texture(self.texture) };
    }

    /// Where `key` is, if it has been placed.
    pub(crate) fn get(&self, key: GlyphKey) -> Option<(usize, usize)> {
        self.places.get(&key).copied()
    }

    /// Where glyph `key` is, `mask` (`w` × `h` coverage bytes) uploaded the
    /// first time. `None` for a glyph too large for any atlas, which the
    /// caller then draws another way. `Some(true)` in the second field when
    /// making room emptied the atlas: places handed out before are gone.
    pub(crate) fn place(
        &mut self,
        gl: &glow::Context,
        key: GlyphKey,
        mask: &[u8],
        w: usize,
        h: usize,
    ) -> Option<((usize, usize), bool)> {
        if let Some(&at) = self.places.get(&key) {
            return Some((at, false));
        }
        let (cw, ch) = (w + 2 * BORDER, h + 2 * BORDER);
        if cw > self.largest || ch > self.largest {
            return None;
        }
        let mut emptied = false;
        if !self.fits(cw, ch) {
            // Bigger if it can be, else the same size, empty.
            let size = if self.size < self.largest { self.size * 2 } else { self.size };
            let texture = make_texture(gl, size).ok()?;
            self.delete(gl);
            self.texture = texture;
            self.size = size;
            self.places.clear();
            self.shelf_y = 0;
            self.shelf_x = 0;
            self.shelf_h = 0;
            emptied = true;
            if !self.fits(cw, ch) {
                return self.place(gl, key, mask, w, h).map(|(at, _)| (at, true));
            }
        }
        if self.shelf_x + cw > self.size {
            self.shelf_y += self.shelf_h;
            self.shelf_x = 0;
            self.shelf_h = 0;
        }
        let corner = (self.shelf_x, self.shelf_y);
        self.shelf_x += cw;
        self.shelf_h = self.shelf_h.max(ch);

        // The glyph with its border, zeroes all round.
        let mut padded = vec![0u8; cw * ch];
        for row in 0..h {
            let from = &mask[row * w..row * w + w];
            let at = (row + BORDER) * cw + BORDER;
            padded[at..at + w].copy_from_slice(from);
        }
        // SAFETY: GL calls on the current context; the region is inside the
        // texture (`fits` checked it).
        unsafe {
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                corner.0 as i32,
                corner.1 as i32,
                cw as i32,
                ch as i32,
                super::one_channel(gl).1,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&padded)),
            );
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.active_texture(glow::TEXTURE0);
        }
        let at = (corner.0 + BORDER, corner.1 + BORDER);
        self.places.insert(key, at);
        Some((at, emptied))
    }

    /// Whether a `w` × `h` glyph or shape fits without the atlas being
    /// made anew (grown or emptied), which would move what is in it.
    pub(crate) fn has_room(&self, w: usize, h: usize) -> bool {
        self.fits(w + 2 * BORDER, h + 2 * BORDER)
    }

    /// Whether a `cw` × `ch` cell fits on this shelf or a new one.
    fn fits(&self, cw: usize, ch: usize) -> bool {
        if cw > self.size {
            return false;
        }
        let y = if self.shelf_x + cw > self.size {
            self.shelf_y + self.shelf_h
        } else {
            self.shelf_y
        };
        y + ch <= self.size
    }
}

fn make_texture(gl: &glow::Context, size: usize) -> Result<glow::Texture, String> {
    // SAFETY: GL calls on the current context.
    unsafe {
        let texture = gl.create_texture().map_err(|e| format!("GL texture: {e}"))?;
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (name, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, name, value as i32);
        }
        // Zeroed, so the borders between glyphs read as nothing.
        let zeros = vec![0u8; size * size];
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        let (internal, format) = super::one_channel(gl);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            internal,
            size as i32,
            size as i32,
            0,
            format,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&zeros)),
        );
        super::spread_red(gl);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        gl.active_texture(glow::TEXTURE0);
        Ok(texture)
    }
}
