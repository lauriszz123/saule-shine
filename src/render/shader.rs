//! `Graphics.newShader`, `setShader` and `capture`: a program's own
//! effects (see `gpu::shader` for what one is).
//!
//! A shader is the GPU's: there is none where the CPU draws, and
//! `new_shader` says so, for the program to do without the effect. While
//! one is set, images go through it; shapes and text are drawn as ever.

use std::rc::Rc;

use super::mirror::Which;
use super::*;
use crate::gpu;
use crate::geom::Transform;
use crate::raster::{self, BlendMode, Paint, Rect};

/// A shader, and what its uniforms were last sent.
pub(crate) struct ShaderRes {
    program: Rc<gpu::shader::UserShader>,
    values: Vec<(String, Vec<f32>)>,
}

impl Renderer {
    /// Compile `source` on the GPU drawing goes to. The error says why
    /// not: the CPU is drawing, or the GPU's own complaint.
    pub(crate) fn new_shader(&mut self, source: &str) -> Result<i64, String> {
        let Some(gpu) = self.gpu.as_ref() else {
            return Err("the processor is drawing: shaders are the graphics chip's".into());
        };
        let program = Rc::new(gpu::shader::UserShader::new(&gpu.dev, source)?);
        let res = ShaderRes { program, values: Vec::new() };
        Ok(insert(&mut self.shaders, &mut self.free_shaders, res))
    }

    fn shader_index(&self, handle: i64, func: &str) -> Result<usize, String> {
        resolve(&self.shaders, handle, func, "shader")
    }

    pub(crate) fn release_shader(&mut self, handle: i64) -> Result<(), String> {
        let idx = self.shader_index(handle, "Graphics.releaseShader")?;
        if self.shader == Some(idx) {
            return Err("Graphics.releaseShader: the shader is currently selected".into());
        }
        self.shaders[idx] = Slot::free();
        self.free_shaders.push(idx);
        Ok(())
    }

    /// Draw images through the shader `handle` from now on; `None` for as
    /// they are.
    pub(crate) fn set_shader(&mut self, handle: Option<i64>) -> Result<(), String> {
        self.shader = match handle {
            Some(handle) => Some(self.shader_index(handle, "Graphics.setShader")?),
            None => None,
        };
        Ok(())
    }

    /// Set the shader's uniform `name` to one to four numbers.
    pub(crate) fn shader_send(&mut self, handle: i64, name: &str, value: &[f32]) -> Result<(), String> {
        let idx = self.shader_index(handle, "Shader.send")?;
        if value.is_empty() || value.len() > 4 {
            return Err("Shader.send: one to four numbers".into());
        }
        let Some(res) = self.shaders[idx].value.as_mut() else {
            return Err("Shader.send: the shader has been released".into());
        };
        match res.values.iter_mut().find(|(known, _)| known == name) {
            Some((_, old)) => *old = value.to_vec(),
            None => res.values.push((name.to_string(), value.to_vec())),
        }
        Ok(())
    }

    /// Draw `frame` of canvas `idx` through the selected shader. An error
    /// where that cannot be: the GPU went away since the shader was made.
    pub(crate) fn shader_blit(
        &mut self,
        shader: usize,
        idx: usize,
        frame: Rect,
        xform: &Transform,
        paint: &Paint,
    ) -> Result<(), String> {
        let lost = || String::from("Graphics.draw: the shader's graphics chip is no longer drawing");
        if !self.on_gpu() {
            return Err(lost());
        }
        if matches!(paint.blend, BlendMode::Multiply | BlendMode::Screen) {
            return Err("Graphics.draw: a shader's pixels are not blended by multiply or screen".into());
        }
        let Some((sw, sh)) = self.canvases[idx].value.as_ref().map(|s| (s.w, s.h)) else {
            return Err("Graphics.draw: the image is on loan to a draw call".into());
        };
        let sub = frame.intersect(&Rect::surface(sw, sh));
        let (rw, rh) = (sub.x1 - sub.x0, sub.y1 - sub.y0);
        let (w, h) = self.target_size();
        let (x0, y0, x1, y1) = paint.clip.pixel_bounds(w, h);
        if rw <= 0.0 || rh <= 0.0 || x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        let source = Which::Canvas(idx);
        let target = self.which_target();
        self.prepare_target(target);
        self.sync_gpu(source, false).map_err(|_| lost())?;
        self.sync_gpu(target, false).map_err(|_| lost())?;
        let Some(res) = self.shaders[shader].value.as_ref() else {
            return Err("Graphics.draw: the shader has been released".into());
        };
        let (Some(src), Some(dst)) = (self.texture(source), self.texture(target)) else {
            return Err(lost());
        };
        gpu::shader::draw(
            dst,
            src,
            &res.program,
            &res.values,
            &gpu::shader::ShaderDraw {
                sub: (sub.x0, sub.y0, rw, rh),
                xform: *xform,
                tint: paint.color,
                clip: gpu::Clip { x0, y0, x1, y1 },
                blend: paint.blend,
            },
        );
        self.touched_gpu(target);
        Ok(())
    }

    /// Copy what is on the target under the rectangle at (`x`, `y`),
    /// `w` × `h` in the current coordinates, into canvas `handle`, made to
    /// fit it: smaller, when the canvas is, each of its pixels a mix of
    /// the ones it stands for. What an effect over what is already drawn
    /// starts from.
    pub(crate) fn capture(&mut self, handle: i64, x: f64, y: f64, w: f64, h: f64) -> Result<(), String> {
        let idx = self.canvas_index(handle, "Graphics.capture")?;
        if self.target == Some(idx) {
            return Err("Graphics.capture: a canvas cannot be captured into itself".into());
        }
        let Some((cw, ch)) = self.canvases[idx].value.as_ref().map(|s| (s.w, s.h)) else {
            return Err("Graphics.capture: the canvas is on loan to a draw call".into());
        };
        // The rectangle's box on the target, in whole pixels, inside it.
        let t = self.st.transform;
        let corners = [t.apply(x, y), t.apply(x + w, y), t.apply(x, y + h), t.apply(x + w, y + h)];
        let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
        let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (px, py) in corners {
            (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
        }
        let (tw, th) = self.target_size();
        let region = Rect { x0: x0.floor(), y0: y0.floor(), x1: x1.ceil(), y1: y1.ceil() }
            .intersect(&Rect::surface(tw, th));
        let (rw, rh) = (region.x1 - region.x0, region.y1 - region.y0);
        if rw <= 0.0 || rh <= 0.0 {
            return Ok(());
        }
        // The region stretched over the whole canvas, replacing what was
        // there, a pixel a mix of its neighbours.
        let xform = Transform::scaling(cw as f64 / rw, ch as f64 / rh);
        let paint = Paint {
            color: [1.0; 4],
            blend: BlendMode::Replace,
            clip: Rect::surface(cw, ch),
            antialias: true,
            linear_filter: true,
            gradient: None,
            corner_radius: 0.0,
        };
        let source = self.which_target();
        let into = Which::Canvas(idx);
        if self.on_gpu()
            && self.sync_gpu(source, false).is_ok()
            && self.sync_gpu(into, true).is_ok()
            && let (Some(src), Some(dst)) = (self.texture(source), self.texture(into))
        {
            let drawn = gpu::blit(
                dst,
                src,
                &gpu::Blit {
                    sub: (region.x0, region.y0, rw, rh),
                    xform,
                    tint: [1.0; 4],
                    corner_radius: 0.0,
                    linear: true,
                    clip: gpu::Clip { x0: 0, y0: 0, x1: cw, y1: ch },
                    check_inside: false,
                    opaque: false,
                    blend: BlendMode::Replace,
                },
            );
            if drawn {
                self.touched_gpu(into);
                return Ok(());
            }
        }
        // On the CPU: both surfaces brought up to date there, the canvas
        // lifted out so that the two can be held at once.
        self.sync_cpu(source);
        self.sync_cpu(into);
        let mut canvas = self.canvases[idx]
            .value
            .take()
            .ok_or("Graphics.capture: the canvas is on loan to a draw call")?;
        let from = match source {
            Which::Screen => Some(&self.screen),
            Which::Canvas(i) => self.canvases[i].value.as_ref(),
        };
        if let Some(from) = from {
            raster::blit_surface_sub(&mut canvas, from, region, &xform, &paint);
        }
        self.canvases[idx].value = Some(canvas);
        self.touch_cpu(into);
        Ok(())
    }
}
