//! The scaled blit: an image drawn larger, smaller or flipped, but square to
//! the screen — no rotation, no shear.
//!
//! The general blit maps every destination pixel back through the inverse
//! transform and samples the source in floating point. That is the price of
//! rotation, and on a slow machine it is too much to pay every frame for the
//! most common transformed draw there is: an overlay growing into place,
//! a sprite drawn at twice its size. Without rotation, a destination column
//! reads the same source column on every row and a destination row the same
//! source row across, so both are worked out once per draw. What is left per
//! pixel is integer arithmetic on packed pixels, the same way
//! [`lerp_argb`] blends.
//!
//! The results are the general path's to within one unit per channel, from
//! rounding once to 8 bits where the general path rounds its floats — below
//! what the framebuffer can show, and the same allowance
//! [`Surface::blend_alpha8`] makes.

use crate::geom::Transform;

use super::*;

/// Where one destination column or row reads from: the two neighbouring
/// source pixels a linear filter mixes (the same one twice when filtering
/// is nearest), and how far towards the second, in [`ONE`]ths.
#[derive(Clone, Copy)]
struct Tap {
    near: usize,
    far: usize,
    weight: u64,
}

/// Draw `sub` of `src` onto `dst` through `xform` when `xform` has no
/// rotation or shear, the paint blends with [`BlendMode::Alpha`] and has no
/// rounded corners. False, having drawn nothing, for anything else, which
/// the general path then draws.
pub(crate) fn blit_scaled(
    dst: &mut Surface,
    src: &Surface,
    sub: Rect,
    xform: &Transform,
    paint: &Paint,
    clip: Rect,
) -> bool {
    const EPS: f64 = 1e-9;
    let square = xform.b.abs() < EPS && xform.c.abs() < EPS;
    if !square || paint.blend != BlendMode::Alpha || paint.corner_radius > 0.0 {
        return false;
    }
    let Some(inv) = xform.inverse() else {
        return true;
    };

    let tint = Src8::new(paint.color);
    if tint.a == 0 {
        return true;
    }

    let (sub_w, sub_h) = (sub.x1 - sub.x0, sub.y1 - sub.y0);
    let bounds = transformed_bounds(xform, sub_w, sub_h);
    let (x0, y0, x1, y1) = bounds.intersect(&clip).pixel_bounds(dst.w, dst.h);
    if x0 >= x1 || y0 >= y1 {
        return true;
    }

    // With no rotation the inverse is square too: a destination x gives a
    // source x whatever the row, and a destination y a source y.
    let linear = paint.linear_filter;
    let columns: Vec<Option<Tap>> = (x0..x1)
        .map(|px| {
            let u = inv.a * (px as f64 + 0.5) + inv.tx;
            tap(u, sub_w, sub.x0, src.w, linear)
        })
        .collect();

    // A source row with nothing to see in it, worked out the first time a
    // destination row asks: rows under a large transparent image (most of
    // an overlay) then cost one test each, not a filter per pixel.
    let mut empty: Vec<Option<bool>> = vec![None; src.h];
    let (sx0, sx1) = (sub.x0 as usize, (sub.x1.ceil() as usize).min(src.w));
    let mut row_empty = |row: usize| -> bool {
        *empty[row].get_or_insert_with(|| {
            let line = &src.buf[row * src.w + sx0..row * src.w + sx1];
            line.iter().fold(0, |any, &p| any | p) >> 24 == 0
        })
    };

    let plain = tint.is_opaque_white();
    // A fade: the image as it is, at a fraction of its strength.
    let fade = tint.r == 255 && tint.g == 255 && tint.b == 255;

    for py in y0..y1 {
        let v = inv.d * (py as f64 + 0.5) + inv.ty;
        let Some(row) = tap(v, sub_h, sub.y0, src.h, linear) else {
            continue;
        };
        // The near row always counts; the far one only when mixed in.
        let seen = !row_empty(row.near) || (row.weight > 0 && !row_empty(row.far));
        if !seen {
            continue;
        }

        let near_line = &src.buf[row.near * src.w..(row.near + 1) * src.w];
        let far_line = &src.buf[row.far * src.w..(row.far + 1) * src.w];
        let dst_base = py * dst.w;

        for (k, column) in columns.iter().enumerate() {
            let Some(col) = column else { continue };
            let s = bilinear(
                [near_line[col.near], near_line[col.far]],
                [far_line[col.near], far_line[col.far]],
                col.weight,
                row.weight,
            );

            let sa = s >> 24;
            if sa == 0 {
                continue;
            }
            let at = dst_base + x0 + k;
            if plain {
                dst.buf[at] = if sa == 255 {
                    s
                } else {
                    lerp_argb(s | 0xFF00_0000, dst.buf[at], sa)
                };
            } else if fade {
                let a = div255(sa * tint.a);
                if a > 0 {
                    dst.buf[at] = lerp_argb(s | 0xFF00_0000, dst.buf[at], a);
                }
            } else {
                dst.blend_alpha8(at, Src8::from_pixel(s).modulate(tint), 255);
            }
        }
    }
    true
}

/// The tap for source coordinate `u` along one axis of a `span`-long region
/// starting at `start`, in a source `size` pixels across; `None` outside the
/// region, which the general path leaves undrawn rather than smearing its
/// edge.
fn tap(u: f64, span: f64, start: f64, size: usize, linear: bool) -> Option<Tap> {
    if !(0.0..span).contains(&u) {
        return None;
    }
    // Half a pixel inside the region, as the general path keeps its taps.
    let u = start + clamp_inside(u, span);
    if !linear {
        let at = (u.floor() as usize).min(size - 1);
        return Some(Tap {
            near: at,
            far: at,
            weight: 0,
        });
    }
    let f = u - 0.5;
    let mut base = f.floor();
    let mut weight = ((f - base) * ONE as f64).round() as u64;
    // So close to the next pixel that it rounds to all of it: that pixel.
    if weight == ONE {
        base += 1.0;
        weight = 0;
    }
    let near = (base.max(0.0) as usize).min(size - 1);
    // The last pixel's far neighbour is past the edge, but only ever at
    // weight 0, so the pixel itself stands in.
    let far = (near + 1).min(size - 1);
    Some(Tap { near, far, weight })
}

/// The filter weights' whole: 12 bits, so finely divided that the weights
/// round off nothing the 8-bit result could show.
const ONE: u64 = 1 << 12;

/// The four pixels around a sample point, `top` and `bottom` each left then
/// right, mixed `across` of the way right and `down` of the way down (in
/// [`ONE`]ths), with straight colours as the general path mixes them.
///
/// Two channels share each 64-bit word, 32 bits apart: a channel times
/// both weights tops out at 255 · 2²⁴, which fits in its 32, so the two never
/// carry into each other and the sum is rounded only once, at the end.
#[inline]
fn bilinear(top: [u32; 2], bottom: [u32; 2], across: u64, down: u64) -> u32 {
    if top[0] == top[1] && bottom[0] == bottom[1] && top[0] == bottom[0] {
        return top[0];
    }
    // Red and blue in one word, alpha and green in the other.
    let rb = |p: u32| ((p as u64 >> 16) & 0xFF) << 32 | (p as u64 & 0xFF);
    let ag = |p: u32| (p as u64 >> 24) << 32 | ((p as u64 >> 8) & 0xFF);
    let rb = mix_words(
        rb(top[0]),
        rb(top[1]),
        rb(bottom[0]),
        rb(bottom[1]),
        across,
        down,
    );
    let ag = mix_words(
        ag(top[0]),
        ag(top[1]),
        ag(bottom[0]),
        ag(bottom[1]),
        across,
        down,
    );
    ((ag >> 32) << 24 | (rb >> 32) << 16 | (ag & 0xFF) << 8 | (rb & 0xFF)) as u32
}

/// [`bilinear`] for one word of two channels, rounded back to 8 bits each.
#[inline]
fn mix_words(tl: u64, tr: u64, bl: u64, br: u64, across: u64, down: u64) -> u64 {
    const HALF: u64 = (1 << 23) << 32 | 1 << 23;
    const LANES: u64 = 0xFF << 32 | 0xFF;
    let lerp = |a: u64, b: u64, w: u64| a * (ONE - w) + b * w;
    let upper = lerp(tl, tr, across);
    let lower = lerp(bl, br, across);
    ((lerp(upper, lower, down) + HALF) >> 24) & LANES
}
