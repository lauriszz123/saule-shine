//! The GPU draws what the CPU draws: each scene below runs twice, on a
//! renderer with no GPU and on one with a GPU attached (a headless EGL
//! device: the machine's GPU, or Mesa's software one), and the screens are
//! compared pixel by pixel.
//!
//! The two can differ by a level or two of 255 per channel: the CPU blends
//! in integers where the GPU blends in floats, and a GPU's bilinear filter
//! weighs its taps to a few bits less than the CPU's. Anything more is a
//! real difference.
//!
//! Skipped, with a note, where the machine has no EGL at all.

use std::rc::Rc;

use super::*;
use crate::raster::{Gradient, GradientKind, Rect, Stop};
use crate::gpu::Device;

/// How far apart a channel may be.
const TOLERANCE: i32 = 3;

/// A scene: what to draw, on a 160×120 screen.
type Scene = fn(&mut Renderer) -> Result<(), String>;

/// A 40×30 image with something in every pixel: a colour ramp across, an
/// alpha ramp down, and an opaque border.
fn pattern(r: &mut Renderer) -> Result<i64, String> {
    let image = r.new_blank_image(40, 30)?;
    for y in 0..30 {
        for x in 0..40 {
            let edge = x == 0 || y == 0 || x == 39 || y == 29;
            let a = if edge { 255 } else { 60 + (y as u32 * 195 / 29) };
            let red = x as u32 * 255 / 39;
            let green = y as u32 * 255 / 29;
            let blue = 255 - red;
            r.set_pixel(image, x, y, (a << 24) | (red << 16) | (green << 8) | blue)?;
        }
    }
    Ok(image)
}

fn backdrop(r: &mut Renderer) {
    r.clear(Some((0.2, 0.3, 0.4, 1.0)));
}

const SCENES: &[(&str, Scene)] = &[
    ("clear, then a scissored clear", |r| {
        backdrop(r);
        r.set_scissor(Some((10.0, 20.0, 50.0, 40.0)));
        r.clear(Some((0.9, 0.1, 0.5, 1.0)));
        Ok(())
    }),
    ("an image at a whole-pixel place", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_canvas(image, 10.0, 12.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("an image tinted and faded", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.set_color(1.0, 0.6, 0.3, 0.5);
        r.draw_canvas(image, 20.0, 5.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("an image scaled up, smooth", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_canvas(image, 3.5, 7.25, 0.0, 2.5, 1.75, 0.0, 0.0)
    }),
    ("an image scaled up, nearest", |r| {
        backdrop(r);
        r.set_default_filter("nearest")?;
        let image = pattern(r)?;
        r.draw_canvas(image, 3.0, 7.0, 0.0, 3.0, 2.0, 0.0, 0.0)
    }),
    ("an image turned", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_canvas(image, 80.0, 60.0, 0.6, 1.5, 1.5, 20.0, 15.0)
    }),
    ("an image with rounded corners", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_rounded(image, 30.0, 40.0, 9.0)
    }),
    ("a frame of a sheet, doubled", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_frame(image, Rect::new(10.0, 5.0, 16.0, 12.0), 50.0, 30.0, 0.0, 2.0, 2.0, 0.0, 0.0)
    }),
    ("an image clipped by the scissor", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.set_scissor(Some((15.0, 20.0, 22.0, 13.0)));
        r.draw_canvas(image, 10.0, 12.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("into a canvas, then the canvas onto the screen", |r| {
        backdrop(r);
        let image = pattern(r)?;
        let canvas = r.new_canvas(100, 80)?;
        r.set_canvas(Some(canvas))?;
        r.clear(Some((1.0, 1.0, 1.0, 0.25)));
        r.draw_canvas(image, 5.0, 5.0, 0.0, 2.0, 2.0, 0.0, 0.0)?;
        r.set_canvas(None)?;
        r.draw_canvas(canvas, 30.0, 20.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("the GPU and the CPU taking turns", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_canvas(image, 10.0, 10.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
        r.set_color(1.0, 1.0, 0.0, 0.7);
        r.rectangle("fill", 20.0, 20.0, 60.0, 30.0, 0.0, 0.0)?;
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.draw_canvas(image, 40.0, 30.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
        r.circle("line", 100.0, 60.0, 25.0, None)
    }),
    ("a pixel read back after the GPU drew it", |r| {
        backdrop(r);
        let image = pattern(r)?;
        let canvas = r.new_canvas(50, 40)?;
        r.set_canvas(Some(canvas))?;
        r.draw_canvas(image, 2.0, 3.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
        r.set_canvas(None)?;
        // The CPU copies a pixel it read, so a stale read shows on screen.
        let pixel = r.get_pixel(canvas, 12, 13)?;
        let copy = r.new_blank_image(20, 20)?;
        for y in 0..20 {
            for x in 0..20 {
                r.set_pixel(copy, x, y, pixel)?;
            }
        }
        r.draw_canvas(copy, 120.0, 90.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("every blend mode, over the screen and over a see-through canvas", |r| {
        backdrop(r);
        let image = pattern(r)?;
        let canvas = r.new_canvas(160, 40)?;
        r.set_canvas(Some(canvas))?;
        r.clear(Some((0.8, 0.2, 0.6, 0.4)));
        r.set_canvas(None)?;
        for (i, mode) in ["alpha", "add", "subtract", "multiply", "screen", "replace"]
            .iter()
            .enumerate()
        {
            r.set_blend_mode(mode)?;
            r.set_color(0.9, 0.7, 1.0, 0.8);
            let x = 5.0 + i as f64 * 25.0;
            r.draw_canvas(image, x, 2.0, 0.0, 0.6, 1.0, 0.0, 0.0)?;
            r.draw_canvas(image, x + 3.0, 70.0, 0.3, 0.5, 1.2, 0.0, 0.0)?;
            r.set_canvas(Some(canvas))?;
            r.draw_canvas(image, x, 5.0, 0.0, 0.6, 1.0, 0.0, 0.0)?;
            r.set_canvas(None)?;
        }
        r.set_blend_mode("alpha")?;
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.draw_canvas(canvas, 0.0, 40.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("rounded corners in every mode but replace", |r| {
        backdrop(r);
        let image = pattern(r)?;
        for (i, mode) in ["replace", "add", "subtract", "multiply", "screen"].iter().enumerate() {
            r.set_blend_mode(mode)?;
            r.draw_rounded(image, 5.0 + i as f64 * 30.0, 50.0, 8.0)?;
        }
        Ok(())
    }),
    ("a window's frame, as a display server takes it", |r| {
        backdrop(r);
        let window = r.new_blank_image(50, 40)?;
        // Little-endian 0x00RRGGBB with junk where alpha would be, which
        // must read as opaque either way.
        let mut frame = Vec::new();
        for y in 0..40u32 {
            for x in 0..50u32 {
                let px = ((x * 5) << 16) | ((y * 6) << 8) | (255 - x * 5);
                frame.extend_from_slice(&(px | ((x * 3) << 24)).to_le_bytes());
            }
        }
        r.upload_opaque(window, &frame)?;
        r.draw_rounded(window, 10.0, 10.0, 6.0)?;
        r.set_color(1.0, 1.0, 1.0, 0.5);
        r.draw_canvas(window, 70.0, 30.0, 0.2, 1.3, 1.3, 0.0, 0.0)?;
        // And read back by the CPU, alpha and all.
        let pixel = r.get_pixel(window, 20, 20)?;
        let copy = r.new_blank_image(10, 10)?;
        for y in 0..10 {
            for x in 0..10 {
                r.set_pixel(copy, x, y, pixel)?;
            }
        }
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.draw_canvas(copy, 140.0, 100.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("rectangles on and off the pixel grid", |r| {
        backdrop(r);
        r.set_color(0.9, 0.8, 0.1, 1.0);
        r.rectangle("fill", 10.0, 10.0, 40.0, 20.0, 0.0, 0.0)?;
        r.set_color(0.1, 0.9, 0.8, 0.6);
        r.rectangle("fill", 30.3, 22.7, 50.45, 31.1, 0.0, 0.0)?;
        r.rectangle("fill", 100.2, 5.6, 0.7, 40.3, 0.0, 0.0)?;
        r.set_scissor(Some((60.5, 60.25, 40.0, 30.0)));
        r.set_color(1.0, 0.3, 0.3, 0.9);
        r.rectangle("fill", 50.0, 50.0, 80.0, 60.0, 0.0, 0.0)?;
        Ok(())
    }),
    ("outlines, rounded and square", |r| {
        backdrop(r);
        r.set_color(1.0, 1.0, 1.0, 0.8);
        r.set_line_width(1.0);
        r.rectangle("line", 10.5, 10.5, 60.0, 40.0, 0.0, 0.0)?;
        r.rectangle("line", 80.5, 10.5, 60.0, 40.0, 8.0, 8.0)?;
        r.set_line_width(3.5);
        r.rectangle("line", 20.0, 60.0, 120.0, 50.0, 12.0, 12.0)?;
        r.set_color(0.2, 0.9, 0.4, 1.0);
        r.rectangle("fill", 40.0, 70.0, 80.0, 30.0, 10.0, 10.0)
    }),
    ("circles, an arc, a polygon and thick lines", |r| {
        backdrop(r);
        r.set_color(0.95, 0.6, 0.2, 1.0);
        r.circle("fill", 40.0, 40.0, 25.3, None)?;
        r.set_color(0.6, 0.7, 1.0, 0.7);
        r.set_line_width(2.5);
        r.circle("line", 50.0, 50.0, 30.0, None)?;
        r.arc("fill", 120.0, 40.0, 30.0, 0.3, 2.4, "pie")?;
        r.polygon("fill", &[(90.0, 80.0), (150.0, 90.0), (110.0, 115.0), (95.0, 100.0)])?;
        r.set_line_width(4.0);
        r.set_line_join("bevel")?;
        r.polyline(&[(10.0, 110.0), (40.0, 80.0), (70.0, 112.0), (85.0, 70.0)])?;
        r.set_line_width(1.0);
        r.line(5.0, 5.0, 150.0, 118.0)
    }),
    ("shapes without antialiasing, and points", |r| {
        backdrop(r);
        r.set_line_style("rough")?;
        r.set_color(1.0, 1.0, 0.4, 1.0);
        r.circle("fill", 40.0, 60.0, 30.6, None)?;
        r.rectangle("fill", 80.3, 20.6, 40.4, 30.2, 0.0, 0.0)?;
        r.set_line_style("smooth")?;
        r.set_color(1.0, 0.2, 0.9, 1.0);
        r.points(&[(5.5, 5.5), (100.2, 100.7), (150.0, 10.0), (3.0, 117.9)]);
        Ok(())
    }),
    ("gradients, straight and round", |r| {
        backdrop(r);
        let stops = [
            Stop { at: 0.0, color: [1.0, 0.2, 0.2, 1.0] },
            Stop { at: 0.4, color: [0.2, 1.0, 0.3, 0.6] },
            Stop { at: 1.0, color: [0.1, 0.2, 1.0, 1.0] },
        ];
        let linear = GradientKind::Linear { x0: 10.0, y0: 0.0, x1: 150.0, y1: 40.0 };
        r.set_gradient(Gradient::new(linear, &stops)?);
        r.rectangle("fill", 5.0, 5.0, 150.0, 50.0, 0.0, 0.0)?;
        let round = GradientKind::Radial { cx: 80.0, cy: 90.0, radius: 35.0 };
        r.set_gradient(Gradient::new(round, &stops)?);
        r.circle("fill", 80.0, 90.0, 28.0, None)?;
        r.rectangle("fill", 120.3, 70.2, 30.0, 40.0, 6.0, 6.0)
    }),
    ("shapes in every blend mode", |r| {
        backdrop(r);
        let image = pattern(r)?;
        r.draw_canvas(image, 0.0, 0.0, 0.0, 4.0, 4.0, 0.0, 0.0)?;
        for (i, mode) in ["alpha", "add", "subtract", "multiply", "screen"].iter().enumerate() {
            r.set_blend_mode(mode)?;
            r.set_color(0.8, 0.5, 0.3, 0.7);
            let x = 5.0 + i as f64 * 30.0;
            r.rectangle("fill", x, 10.0, 22.5, 40.0, 0.0, 0.0)?;
            r.circle("fill", x + 12.0, 80.0, 12.5, None)?;
        }
        Ok(())
    }),
    ("text: plain, faded and coloured", |r| {
        backdrop(r);
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.print("Saule GPU text: ÄŠž 123", 4.0, 4.0)?;
        r.set_color(1.0, 0.8, 0.2, 0.6);
        r.print("faded, two\nlines", 10.3, 30.6)?;
        let font = r.new_font(22.0, None)?;
        r.set_font(font)?;
        r.set_color(0.3, 1.0, 0.6, 1.0);
        r.print("Big Ag", 20.0, 70.0)
    }),
    ("text wrapped and justified, and clipped", |r| {
        backdrop(r);
        r.set_color(0.9, 0.9, 1.0, 1.0);
        r.printf(
            "A paragraph of words that wraps across the box and is justified to both edges.",
            5.0,
            5.0,
            150.0,
            "justify",
        )?;
        r.set_scissor(Some((20.0, 70.0, 60.0, 14.0)));
        r.printf("cut by the scissor, centred", 0.0, 68.0, 160.0, "center")
    }),
    ("text scaled and turned, smooth and nearest", |r| {
        backdrop(r);
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.push(false);
        r.translate(10.0, 10.0);
        r.scale(1.7, 1.7);
        r.print("Scaled", 0.0, 0.0)?;
        r.pop()?;
        r.push(false);
        r.translate(80.0, 60.0);
        r.rotate(0.5);
        r.print("Turned text", 0.0, 0.0)?;
        r.pop()?;
        r.set_default_filter("nearest")?;
        r.push(false);
        r.translate(10.0, 90.0);
        r.scale(2.0, 1.5);
        r.print("Near", 0.0, 0.0)?;
        r.pop()
    }),
    ("text in every blend mode, onto a canvas", |r| {
        backdrop(r);
        let canvas = r.new_canvas(160, 120)?;
        r.set_canvas(Some(canvas))?;
        r.clear(Some((0.5, 0.2, 0.7, 0.5)));
        for (i, mode) in ["alpha", "add", "subtract", "multiply", "screen"].iter().enumerate() {
            r.set_blend_mode(mode)?;
            r.set_color(0.9, 0.6, 0.3, 0.8);
            r.print(mode, 5.0, 5.0 + i as f64 * 20.0)?;
        }
        r.set_blend_mode("alpha")?;
        r.set_canvas(None)?;
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.draw_canvas(canvas, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
        r.print("end", 120.0, 100.0)
    }),
    ("glyphs too many for a small atlas", |r| {
        backdrop(r);
        // Large glyphs, so the atlas has to grow while drawing one run.
        let font = r.new_font(250.0, None)?;
        r.set_font(font)?;
        r.set_color(1.0, 1.0, 1.0, 1.0);
        r.print("WMQ@#", -20.0, -10.0)?;
        r.print("gjpqy", -10.0, 30.0)
    }),
    ("runs of shapes gathered into batches, broken by text, modes and targets", |r| {
        backdrop(r);
        for i in 0..40 {
            let (x, y) = ((i % 8) as f64 * 19.5 + 2.3, (i / 8) as f64 * 11.0 + 1.6);
            r.set_color(0.1 + i as f64 / 50.0, 0.8 - i as f64 / 60.0, 0.5, 0.6 + (i % 3) as f64 * 0.15);
            r.rectangle("fill", x, y, 16.25, 8.5, 0.0, 0.0)?;
            r.rectangle("fill", x + 2.0, y + 1.0, 12.0, 6.0, 3.0, 3.0)?;
            if i == 13 {
                r.print("text", x, y)?;
            }
            if i == 21 {
                r.set_blend_mode("add")?;
            }
            if i == 29 {
                r.set_blend_mode("alpha")?;
                let canvas = r.new_canvas(40, 30)?;
                r.set_canvas(Some(canvas))?;
                r.rectangle("fill", 5.0, 5.0, 20.0, 10.0, 4.0, 4.0)?;
                r.set_canvas(None)?;
                r.draw_canvas(canvas, 110.0, 80.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
            }
        }
        r.set_color(1.0, 1.0, 1.0, 0.5);
        r.rectangle("fill", 0.0, 100.0, 160.0, 20.0, 0.0, 0.0)
    }),
    ("shapes on an image made where a released one was, past the old one's size", |r| {
        backdrop(r);
        let small = r.new_canvas(40, 30)?;
        r.set_canvas(Some(small))?;
        r.set_color(0.9, 0.3, 0.2, 1.0);
        r.rectangle("fill", 4.0, 4.0, 30.0, 20.0, 6.0, 6.0)?;
        r.set_canvas(None)?;
        r.draw_canvas(small, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
        r.release_canvas(small)?;
        // Most likely the same framebuffer name, for a texture four times
        // the size: shapes all over it must land.
        let large = r.new_canvas(150, 110)?;
        r.set_canvas(Some(large))?;
        r.set_color(0.2, 0.7, 0.9, 0.8);
        for (x, y) in [(4.0, 4.0), (60.0, 50.0), (110.0, 80.0), (20.0, 85.0)] {
            r.rectangle("fill", x, y, 34.0, 22.0, 6.0, 6.0)?;
        }
        r.set_canvas(None)?;
        r.draw_canvas(large, 5.0, 5.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
    ("a blur over stripes and an image", |r| {
        stripes(r)?;
        r.blur(20.0, 15.0, 110.0, 80.0, 12.0, 0.0, 1.0, 0.0)
    }),
    ("a rounded blur, faded in, under a tint", |r| {
        stripes(r)?;
        r.blur(15.5, 10.25, 120.0, 90.0, 20.0, 14.0, 0.7, 0.0)?;
        r.set_color(0.1, 0.1, 0.15, 0.4);
        r.rectangle("fill", 15.5, 10.25, 120.0, 90.0, 14.0, 14.0)
    }),
    ("a blur at the screen's edges, as wide as it goes", |r| {
        stripes(r)?;
        r.blur(-10.0, -10.0, 200.0, 60.0, 64.0, 0.0, 1.0, 0.0)?;
        r.blur(100.0, 70.0, 100.0, 100.0, 3.0, 8.0, 1.0, 0.0)
    }),
    ("a blur cut by the scissor, moved and scaled", |r| {
        stripes(r)?;
        r.set_scissor(Some((30.0, 20.0, 70.0, 60.0)));
        r.push(false);
        r.translate(10.0, 5.0);
        r.scale(1.5, 1.5);
        r.blur(5.0, 5.0, 60.0, 50.0, 6.0, 5.0, 1.0, 0.0)?;
        r.pop()?;
        Ok(())
    }),
    ("a translucent layer faded in, premultiplied", |r| {
        stripes(r)?;
        let layer = r.new_canvas(100, 70)?;
        r.set_canvas(Some(layer))?;
        r.clear(Some((0.0, 0.0, 0.0, 0.0)));
        r.set_color(0.1, 0.1, 0.14, 0.55);
        r.rectangle("fill", 5.0, 5.0, 90.0, 60.0, 10.0, 10.0)?;
        r.set_color(1.0, 0.9, 0.3, 0.8);
        r.rectangle("fill", 20.0, 20.0, 30.0, 20.0, 0.0, 0.0)?;
        r.set_canvas(None)?;
        r.set_blend_mode("premultiplied")?;
        r.set_color(1.0, 1.0, 1.0, 0.6);
        r.draw_canvas(layer, 30.0, 25.0, 0.0, 1.0, 1.0, 0.0, 0.0)?;
        r.draw_canvas(layer, 100.0, 60.0, 0.3, 0.8, 0.8, 0.0, 0.0)?;
        r.set_blend_mode("alpha")
    }),
    ("a bar's blur fading out toward its bottom", |r| {
        stripes(r)?;
        r.blur(0.0, 0.0, 160.0, 40.0, 16.0, 0.0, 1.0, 28.0)
    }),
    ("a blur on an image with see-through pixels", |r| {
        backdrop(r);
        let canvas = r.new_canvas(80, 60)?;
        r.set_canvas(Some(canvas))?;
        r.set_color(1.0, 0.8, 0.2, 0.6);
        r.rectangle("fill", 10.0, 10.0, 30.0, 40.0, 0.0, 0.0)?;
        r.blur(0.0, 0.0, 80.0, 60.0, 8.0, 10.0, 1.0, 0.0)?;
        r.set_canvas(None)?;
        r.draw_canvas(canvas, 30.0, 20.0, 0.0, 1.0, 1.0, 0.0, 0.0)
    }),
];

/// Hard-edged stripes of colour under a picture: what shows a blur best.
fn stripes(r: &mut Renderer) -> Result<(), String> {
    backdrop(r);
    for i in 0..16 {
        let f = i as f64 / 15.0;
        r.set_color(f, 1.0 - f, 0.5 + f / 2.0, 1.0);
        r.rectangle("fill", i as f64 * 10.0, 0.0, 5.0, 120.0, 0.0, 0.0)?;
    }
    let image = pattern(r)?;
    r.draw_canvas(image, 60.0, 40.0, 0.0, 1.0, 1.0, 0.0, 0.0)
}

#[test]
fn the_gpu_draws_what_the_cpu_draws() {
    let device = match Device::headless() {
        Ok(device) => Rc::new(device),
        Err(e) => {
            eprintln!("skipped: no GPU to compare with ({e})");
            return;
        }
    };
    eprintln!("comparing with {}", device.description);

    let mut failures = Vec::new();
    for (name, scene) in SCENES {
        let mut cpu = Renderer::headless(160, 120);
        scene(&mut cpu).unwrap();

        let mut gpu = Renderer::headless(160, 120);
        gpu.attach_gpu_always(Rc::clone(&device));
        scene(&mut gpu).unwrap();
        // Every scene ends with something the GPU draws: a frame left on
        // the CPU would mean an operation quietly fell back.
        if gpu.screen_texture().is_none() {
            failures.push(format!("{name}: the last thing drawn was not drawn on the GPU"));
        }
        gpu.sync_cpu(Which::Screen);

        let mut worst = 0;
        let mut at = (0, 0);
        let mut differ = 0;
        for (i, (a, b)) in cpu.screen.buf.iter().zip(&gpu.screen.buf).enumerate() {
            let far = (0..4)
                .map(|k| ((a >> (k * 8)) & 0xFF) as i32 - ((b >> (k * 8)) & 0xFF) as i32)
                .map(i32::abs)
                .max()
                .unwrap_or(0);
            if far > TOLERANCE {
                differ += 1;
            }
            if far > worst {
                worst = far;
                at = (i % 160, i / 160);
            }
        }
        if differ > 0 {
            failures.push(format!(
                "{name}: {differ} pixels differ, by up to {worst} at {at:?} \
                 (CPU {:08x}, GPU {:08x})",
                cpu.screen.buf[at.1 * 160 + at.0],
                gpu.screen.buf[at.1 * 160 + at.0]
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
