//! Font loading and text rendering.

use crate::font::{self, Align, FontRes};
use crate::geom::Transform;
use crate::raster;

use super::*;

impl Renderer {
    /// Load a typeface. `path` of `None` uses the system default face.
    /// Returns the new font's handle.
    pub fn new_font(&mut self, size: f64, path: Option<&str>) -> Result<i64, String> {
        let res = match path {
            Some(p) => FontRes::from_file(p, size)?,
            None => font::load_default(size).ok_or_else(no_system_font)?,
        };
        Ok(insert(&mut self.fonts, &mut self.free_fonts, res))
    }

    /// [`Renderer::new_font`], reporting failure as `None` rather than an
    /// error.
    ///
    /// A native error ends the Saule program, so a caller that wants to degrade
    /// gracefully — fall back to the default face, log and carry on — otherwise
    /// has to stat the file first and hope nothing changes in between. This is
    /// the same load with the outcome as a value.
    pub fn try_new_font(&mut self, size: f64, path: Option<&str>) -> Option<i64> {
        self.new_font(size, path).ok()
    }

    pub fn set_font(&mut self, handle: i64) -> Result<(), String> {
        self.st.font = self.font_index(handle, "Graphics.setFont")?;
        Ok(())
    }

    /// Make sure the selected font slot is populated, loading the system
    /// default on first use.
    pub(crate) fn ensure_font(&mut self) -> Result<(), String> {
        let i = self.st.font;
        if self.fonts.get(i).is_some_and(|f| f.value.is_some()) {
            return Ok(());
        }
        if i != 0 {
            return Err(format!("no font with handle {i}"));
        }
        self.fonts[0].value =
            Some(font::load_default(font::DEFAULT_SIZE).ok_or_else(no_system_font)?);
        Ok(())
    }

    /// Run `f` against the font `handle` names, loading the default face
    /// first if that is the one asked for. The selection is untouched.
    pub(crate) fn with_font<R>(
        &mut self,
        handle: i64,
        f: impl FnOnce(&mut FontRes) -> R,
    ) -> Result<R, String> {
        let idx = self.font_index(handle, "Font")?;
        let saved = self.st.font;
        self.st.font = idx;
        let loaded = self.ensure_font();
        self.st.font = saved;
        loaded?;
        let font = self.fonts[idx]
            .value
            .as_mut()
            .ok_or("Font: the font has been released")?;
        Ok(f(font))
    }

    pub fn font_height(&mut self) -> Result<f64, String> {
        self.ensure_font()?;
        Ok(self.fonts[self.st.font]
            .value
            .as_ref()
            .expect("ensure_font ran before this call")
            .height())
    }

    pub fn text_width(&mut self, text: &str) -> Result<f64, String> {
        self.ensure_font()?;
        let i = self.st.font;
        Ok(self.fonts[i]
            .value
            .as_mut()
            .expect("ensure_font ran before this call")
            .measure(text))
    }

    /// Where each line of `text` starts in the current font, broken at
    /// `limit` pixels (see `Font::wrap_starts`).
    pub fn wrap_starts(&mut self, text: &str, limit: f64, soft: bool) -> Result<Vec<i64>, String> {
        self.ensure_font()?;
        let i = self.st.font;
        Ok(self.fonts[i]
            .value
            .as_mut()
            .expect("ensure_font ran before this call")
            .wrap_starts(text, limit, soft))
    }

    /// Draw a single line of text with its top-left corner at `(x, y)`.
    pub fn print(&mut self, text: &str, x: f64, y: f64) -> Result<(), String> {
        self.draw_text(text, x, y, None)
    }

    /// Draw wrapped, aligned text inside a `limit`-wide box anchored at
    /// `(x, y)`.
    pub fn printf(
        &mut self,
        text: &str,
        x: f64,
        y: f64,
        limit: f64,
        align: &str,
    ) -> Result<(), String> {
        let align = Align::parse(align)?;
        self.draw_text(text, x, y, Some((limit, align)))
    }

    /// `print` (no `wrap`) and `printf`: lay the glyphs out, then draw them
    /// on the GPU if it can, else on the CPU.
    fn draw_text(
        &mut self,
        text: &str,
        x: f64,
        y: f64,
        wrap: Option<(f64, Align)>,
    ) -> Result<(), String> {
        self.ensure_font()?;
        let paint = self.blit_paint();
        let xform = self.st.transform;

        let mut placed = std::mem::take(&mut self.scratch.placed);
        placed.clear();
        {
            let Renderer {
                fonts, st, scratch, ..
            } = self;
            let font = fonts[st.font]
                .value
                .as_mut()
                .expect("ensure_font ran before this call");
            let mut cursor_y = y;
            match wrap {
                None => {
                    for line in text.split('\n') {
                        place_line(font, &mut scratch.glyphs, line, x, cursor_y, 0.0, &xform, &mut placed);
                        cursor_y += font.height();
                    }
                }
                Some((limit, align)) => {
                    font.wrap_into(text, limit, &mut scratch.wrap);
                    for line in scratch.wrap.lines() {
                        let width = font.measure_line(&line.text);
                        place_line(
                            font,
                            &mut scratch.glyphs,
                            &line.text,
                            x + align.offset(width, limit),
                            cursor_y,
                            align.word_spacing(line, width, limit),
                            &xform,
                            &mut placed,
                        );
                        cursor_y += font.height();
                    }
                }
            }
        }

        if !self.gpu_text(&paint, &placed) {
            self.touch_cpu(self.which_target());
            let Renderer {
                screen,
                screen_dirty,
                canvases,
                target,
                fonts,
                st,
                ..
            } = self;
            let surf = surface_of(screen, screen_dirty, canvases, *target);
            let font = fonts[st.font]
                .value
                .as_ref()
                .expect("ensure_font ran before this call");
            for (ch, t) in &placed {
                if let Some(glyph) = font.glyph(*ch) {
                    raster::blit_mask(surf, &glyph.mask, t, &paint);
                }
            }
        }
        self.scratch.placed = placed;
        Ok(())
    }

    /// The font slot and tag of the selected font, as the GPU's glyph atlas
    /// knows its glyphs: a released font's never pass for a new one's.
    pub(crate) fn font_key(&self) -> u64 {
        let i = self.st.font;
        ((i as u64) << 32) | u64::from(self.fonts[i].tag)
    }
}

/// Lay one line out, and add each glyph that has pixels, with where its
/// mask goes on the target, to `placed`. `y` is the line's *top*, matching
/// how Love2D's `print` anchors text.
///
/// `word_spacing` is extra advance added at each space, which is what turns a
/// left-aligned line into a justified one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn place_line(
    font: &mut FontRes,
    glyphs: &mut Vec<(char, f64)>,
    text: &str,
    x: f64,
    y: f64,
    word_spacing: f64,
    xform: &Transform,
    placed: &mut Vec<(char, Transform)>,
) {
    if text.is_empty() {
        return;
    }
    let baseline = y + font.ascent();
    font.layout_into(text, glyphs);

    // On an unscaled target, start the line itself on a whole pixel, so the
    // glyphs (snapped one by one below) keep the same spacing wherever the
    // line is drawn — moving a label by 0.3px moves none of its letters
    // rather than some of them.
    let (x, baseline) = if is_unit(xform) {
        let (dx, dy) = xform.apply(x, baseline);
        (x + dx.round() - dx, baseline + dy.round() - dy)
    } else {
        (x, baseline)
    };

    // Spaces seen so far, so each following glyph carries the justification
    // slack of every gap to its left.
    let mut spread = 0.0;
    for &(ch, pen) in glyphs.iter() {
        if ch == ' ' {
            spread += word_spacing;
            continue; // a space has advance but no pixels
        }
        let Some(glyph) = font.glyph(ch) else {
            continue;
        };
        if glyph.mask.w == 0 || glyph.mask.h == 0 {
            continue; // whitespace carries advance but no pixels
        }
        let placement = Transform::translation(x + pen + spread + glyph.left, baseline + glyph.top);
        placed.push((ch, snap_to_pixel(xform.then(&placement))));
    }
}

/// Move an unscaled, unrotated glyph placement onto whole pixels.
///
/// A glyph mask is rasterized on the pixel grid, so placing it at a
/// fractional position gains nothing but blur — and costs a great deal: a
/// fractional offset sends `blit_mask` down its general path, which inverts
/// the transform and samples the mask bilinearly for every pixel. Advances
/// are fractional, so that was nearly every glyph, and a 17-character label
/// cost ~37 µs to draw. Snapped, the same label is a straight row-by-row
/// blend. Scaled or rotated text keeps its exact transform.
fn snap_to_pixel(mut t: Transform) -> Transform {
    if is_unit(&t) {
        t.tx = t.tx.round();
        t.ty = t.ty.round();
    }
    t
}

/// No scale, rotation or shear: only a translation.
fn is_unit(t: &Transform) -> bool {
    const EPS: f64 = 1e-6;
    (t.a - 1.0).abs() < EPS && (t.d - 1.0).abs() < EPS && t.b.abs() < EPS && t.c.abs() < EPS
}

pub(crate) fn no_system_font() -> String {
    "no font available — the engine found no system typeface to fall back on; \
     load one explicitly with Graphics.newFont(size, path)"
        .to_string()
}
