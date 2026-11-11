//! `ParticleSystem` — Love2D's particle emitter, simulated on the CPU.
//!
//! The API follows `love.graphics.newParticleSystem`: an emitter with a
//! position, a rate and a lifetime, spawning particles whose speed,
//! direction, acceleration, size, spin and colour are given as ranges and
//! sequences. `update(dt)` advances it; `Graphics.draw(ps)` draws it.

use saule_sdk::prelude::*;
use saule_sdk::saule_export;

use crate::objects::{Place, texture_handle};
use crate::random::Rng;
use crate::state;

/// How `setEmissionArea` scatters new particles around the emitter.
#[saule_enum]
#[derive(Clone, Copy, PartialEq)]
pub enum AreaSpread {
    /// Every particle starts at the emitter's position.
    None,
    /// Evenly inside a `dx` × `dy` rectangle.
    Uniform,
    /// Clustered, with `dx`, `dy` as standard deviations.
    Normal,
    /// Evenly inside an ellipse of radii `dx`, `dy`.
    Ellipse,
}

#[derive(Clone, Copy)]
struct Particle {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    /// Where it was emitted from: radial and tangential acceleration push
    /// away from and around this point.
    ox: f64,
    oy: f64,
    age: f64,
    lifetime: f64,
    rotation: f64,
    spin: f64,
    damping: f64,
    radial: f64,
    tangential: f64,
    ax: f64,
    ay: f64,
    /// Per-particle offset into the size sequence, from `setSizeVariation`.
    size_jitter: f64,
}

/// A particle emitter. Without a texture each particle is a filled square
/// `8 × size` pixels across; with one, the texture is drawn scaled by size.
#[saule_class]
pub struct ParticleSystem {
    texture: Option<(SAnyObject, i64, f64, f64)>,
    particles: Vec<Particle>,
    limit: usize,
    rng: Rng,

    x: f64,
    y: f64,
    active: bool,
    paused: bool,
    rate: f64,
    accum: f64,
    emitter_lifetime: f64,
    emitter_left: f64,

    life: (f64, f64),
    speed: (f64, f64),
    direction: f64,
    spread: f64,
    accel: (f64, f64, f64, f64),
    damping: (f64, f64),
    radial: (f64, f64),
    tangential: (f64, f64),
    sizes: Vec<f64>,
    size_variation: f64,
    rotation: (f64, f64),
    spin: (f64, f64),
    relative_rotation: bool,
    colors: Vec<[f64; 4]>,
    area: (AreaSpread, f64, f64),
    offset: Option<(f64, f64)>,
}

/// Linear interpolation through a sequence, at `t` in `[0, 1]`.
fn through<T: Copy>(seq: &[T], t: f64, mix: impl Fn(T, T, f64) -> T) -> T {
    if seq.len() == 1 {
        return seq[0];
    }
    let pos = t.clamp(0.0, 1.0) * (seq.len() - 1) as f64;
    let i = (pos.floor() as usize).min(seq.len() - 2);
    mix(seq[i], seq[i + 1], pos - i as f64)
}

impl ParticleSystem {
    fn spawn(&mut self) {
        if self.particles.len() >= self.limit {
            return;
        }
        let r = &mut self.rng;
        let (mut x, mut y) = (self.x, self.y);
        match self.area {
            (AreaSpread::None, _, _) => {}
            (AreaSpread::Uniform, dx, dy) => {
                x += r.range(-dx, dx);
                y += r.range(-dy, dy);
            }
            (AreaSpread::Normal, dx, dy) => {
                x += r.normal(dx, 0.0);
                y += r.normal(dy, 0.0);
            }
            (AreaSpread::Ellipse, dx, dy) => {
                let a = r.range(0.0, std::f64::consts::TAU);
                let d = r.float().sqrt();
                x += a.cos() * dx * d;
                y += a.sin() * dy * d;
            }
        }
        let angle = self.direction + r.range(-self.spread / 2.0, self.spread / 2.0);
        let speed = r.range(self.speed.0, self.speed.1);
        let (vx, vy) = (angle.cos() * speed, angle.sin() * speed);
        let rotation = r.range(self.rotation.0, self.rotation.1)
            + if self.relative_rotation { angle } else { 0.0 };
        let p = Particle {
            x,
            y,
            vx,
            vy,
            ox: self.x,
            oy: self.y,
            age: 0.0,
            lifetime: r.range(self.life.0, self.life.1).max(1e-6),
            rotation,
            spin: r.range(self.spin.0, self.spin.1),
            damping: r.range(self.damping.0, self.damping.1),
            radial: r.range(self.radial.0, self.radial.1),
            tangential: r.range(self.tangential.0, self.tangential.1),
            ax: r.range(self.accel.0, self.accel.2),
            ay: r.range(self.accel.1, self.accel.3),
            size_jitter: r.range(-self.size_variation, self.size_variation),
        };
        self.particles.push(p);
    }

    pub(crate) fn draw(&self, at: Place) -> Result<(), String> {
        state::draw(|gfx| {
            gfx.push(false);
            gfx.translate(at.x, at.y);
            gfx.rotate(at.r);
            gfx.scale(at.sx, at.sy);
            gfx.translate(-at.ox, -at.oy);
            let (cr, cg, cb, ca) = gfx.color();
            let mut outcome = Ok(());
            for p in &self.particles {
                let t = p.age / p.lifetime;
                let size = (through(&self.sizes, t, |a, b, k| a + (b - a) * k)
                    * (1.0 + p.size_jitter))
                    .max(0.0);
                let c = through(&self.colors, t, |a, b, k| {
                    [
                        a[0] + (b[0] - a[0]) * k,
                        a[1] + (b[1] - a[1]) * k,
                        a[2] + (b[2] - a[2]) * k,
                        a[3] + (b[3] - a[3]) * k,
                    ]
                });
                gfx.set_color(cr * c[0], cg * c[1], cb * c[2], ca * c[3]);
                outcome = match &self.texture {
                    Some((_, handle, w, h)) => {
                        let (ox, oy) = self.offset.unwrap_or((w / 2.0, h / 2.0));
                        gfx.draw_canvas(*handle, p.x, p.y, p.rotation, size, size, ox, oy)
                    }
                    None => {
                        let side = 8.0 * size;
                        gfx.rectangle(
                            "fill",
                            p.x - side / 2.0,
                            p.y - side / 2.0,
                            side,
                            side,
                            0.0,
                            0.0,
                        )
                    }
                };
                if outcome.is_err() {
                    break;
                }
            }
            gfx.set_color(cr, cg, cb, ca);
            gfx.pop()?;
            outcome
        })?
    }
}

fn range_of(min: f64, max: Option<f64>) -> (f64, f64) {
    (min, max.unwrap_or(min))
}

#[saule_methods]
impl ParticleSystem {
    /// Advance every particle by `dt` seconds and emit new ones.
    pub fn update(&mut self, dt: f64) {
        if self.paused || dt <= 0.0 {
            return;
        }
        if self.active {
            self.accum += self.rate * dt;
            while self.accum >= 1.0 {
                self.accum -= 1.0;
                self.spawn();
            }
            if self.emitter_lifetime >= 0.0 {
                self.emitter_left -= dt;
                if self.emitter_left <= 0.0 {
                    self.active = false;
                }
            }
        }
        for p in &mut self.particles {
            p.age += dt;
            let (dx, dy) = (p.x - p.ox, p.y - p.oy);
            let len = (dx * dx + dy * dy).sqrt();
            let (nx, ny) = if len > 1e-9 {
                (dx / len, dy / len)
            } else {
                (0.0, 0.0)
            };
            let ax = p.ax + nx * p.radial - ny * p.tangential;
            let ay = p.ay + ny * p.radial + nx * p.tangential;
            p.vx += ax * dt;
            p.vy += ay * dt;
            let damp = 1.0 / (1.0 + p.damping * dt);
            p.vx *= damp;
            p.vy *= damp;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            p.rotation += p.spin * dt;
        }
        self.particles.retain(|p| p.age < p.lifetime);
    }

    /// Emit `count` particles at once, whatever the rate.
    pub fn emit(&mut self, count: i64) {
        for _ in 0..count.max(0) {
            self.spawn();
        }
    }

    /// Start (or restart) emitting.
    pub fn start(&mut self) {
        self.active = true;
        self.paused = false;
        self.emitter_left = self.emitter_lifetime;
    }

    /// Stop emitting; live particles carry on to the end of their lives.
    pub fn stop(&mut self) {
        self.active = false;
        self.accum = 0.0;
    }

    /// Freeze the system, particles included.
    pub fn pause(&mut self) {
        self.paused = true;
    }

    /// Remove every particle and rewind the emitter.
    pub fn reset(&mut self) {
        self.particles.clear();
        self.accum = 0.0;
        self.emitter_left = self.emitter_lifetime;
    }

    pub fn is_active(&self) -> bool {
        self.active && !self.paused
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn is_stopped(&self) -> bool {
        !self.active
    }

    /// Live particles.
    pub fn get_count(&self) -> i64 {
        self.particles.len() as i64
    }

    /// The most particles alive at once.
    pub fn set_buffer_size(&mut self, size: i64) -> Result<(), String> {
        if size <= 0 {
            return Err("ParticleSystem.setBufferSize: the size must be positive".into());
        }
        self.limit = size as usize;
        self.particles.truncate(self.limit);
        Ok(())
    }

    pub fn get_buffer_size(&self) -> i64 {
        self.limit as i64
    }

    /// Particles emitted per second while active.
    pub fn set_emission_rate(&mut self, rate: f64) {
        self.rate = rate.max(0.0);
    }

    pub fn get_emission_rate(&self) -> f64 {
        self.rate
    }

    /// How long the emitter runs after `start`, in seconds; `-1` for ever.
    pub fn set_emitter_lifetime(&mut self, seconds: f64) {
        self.emitter_lifetime = seconds;
        self.emitter_left = seconds;
    }

    pub fn get_emitter_lifetime(&self) -> f64 {
        self.emitter_lifetime
    }

    /// How long each particle lives: between `min` and `max` seconds.
    pub fn set_particle_lifetime(&mut self, min: f64, max: Option<f64>) {
        self.life = range_of(min, max);
    }

    /// Where new particles come from.
    pub fn set_position(&mut self, x: f64, y: f64) {
        self.x = x;
        self.y = y;
    }

    /// Move the emitter. Love2D spreads the particles of a frame along the
    /// path; here they come from the new position, which reads the same at
    /// game speeds.
    pub fn move_to(&mut self, x: f64, y: f64) {
        self.set_position(x, y);
    }

    pub fn get_position(&self) -> (f64, f64) {
        (self.x, self.y)
    }

    /// The direction particles leave in, in radians (0 is right).
    pub fn set_direction(&mut self, angle: f64) {
        self.direction = angle;
    }

    pub fn get_direction(&self) -> f64 {
        self.direction
    }

    /// How far either side of the direction a particle may leave, as a full
    /// angle: `2π` is every direction.
    pub fn set_spread(&mut self, angle: f64) {
        self.spread = angle;
    }

    pub fn get_spread(&self) -> f64 {
        self.spread
    }

    /// Starting speed, in pixels per second.
    pub fn set_speed(&mut self, min: f64, max: Option<f64>) {
        self.speed = range_of(min, max);
    }

    /// A constant acceleration, picked per particle from the box
    /// `xmin, ymin` – `xmax, ymax`.
    pub fn set_linear_acceleration(
        &mut self,
        xmin: f64,
        ymin: f64,
        xmax: Option<f64>,
        ymax: Option<f64>,
    ) {
        self.accel = (xmin, ymin, xmax.unwrap_or(xmin), ymax.unwrap_or(ymin));
    }

    /// Drag: how quickly a particle's speed bleeds away.
    pub fn set_linear_damping(&mut self, min: f64, max: Option<f64>) {
        self.damping = range_of(min, max);
    }

    /// Acceleration away from the point the particle was emitted at
    /// (negative pulls it back in).
    pub fn set_radial_acceleration(&mut self, min: f64, max: Option<f64>) {
        self.radial = range_of(min, max);
    }

    /// Acceleration around the point the particle was emitted at.
    pub fn set_tangential_acceleration(&mut self, min: f64, max: Option<f64>) {
        self.tangential = range_of(min, max);
    }

    /// Sizes over a particle's life, from birth to death: 1 to 8 of them.
    pub fn set_sizes(&mut self, sizes: Vec<f64>) -> Result<(), String> {
        if sizes.is_empty() || sizes.len() > 8 {
            return Err("ParticleSystem.setSizes: give between 1 and 8 sizes".into());
        }
        self.sizes = sizes;
        Ok(())
    }

    /// How much each particle's size may differ from the sequence, `0`–`1`.
    pub fn set_size_variation(&mut self, variation: f64) {
        self.size_variation = variation.clamp(0.0, 1.0);
    }

    /// Starting rotation, in radians.
    pub fn set_rotation(&mut self, min: f64, max: Option<f64>) {
        self.rotation = range_of(min, max);
    }

    /// Spin, in radians per second.
    pub fn set_spin(&mut self, min: f64, max: Option<f64>) {
        self.spin = range_of(min, max);
    }

    /// Whether a particle starts rotated to face the way it moves.
    pub fn set_relative_rotation(&mut self, enable: bool) {
        self.relative_rotation = enable;
    }

    /// Colours over a particle's life, as a flat `{r, g, b, a, r, g, b, a,
    /// …}` table of 1 to 8 colours. Love2D takes the same numbers as
    /// separate arguments.
    pub fn set_colors(&mut self, rgba: Vec<f64>) -> Result<(), String> {
        if rgba.is_empty() || !rgba.len().is_multiple_of(4) || rgba.len() > 32 {
            return Err(
                "ParticleSystem.setColors: give 1 to 8 colours, four numbers (r, g, b, a) each"
                    .into(),
            );
        }
        self.colors = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        Ok(())
    }

    /// Scatter new particles around the emitter.
    pub fn set_emission_area(&mut self, spread: AreaSpread, dx: f64, dy: f64) {
        self.area = (spread, dx, dy);
    }

    /// The point of the texture that sits on a particle's position — its
    /// centre unless set.
    pub fn set_offset(&mut self, ox: f64, oy: f64) {
        self.offset = Some((ox, oy));
    }

    /// Draw particles as an `Image` or `Canvas` instead of squares.
    pub fn set_texture(&mut self, texture: SValue) -> Result<(), String> {
        self.texture = Some(texture_of(&texture, "ParticleSystem.setTexture")?);
        Ok(())
    }
}

fn texture_of(texture: &SValue, func: &str) -> Result<(SAnyObject, i64, f64, f64), String> {
    let handle = texture_handle(texture, func)?;
    let (w, h) = state::draw(|gfx| gfx.image_size(handle))??;
    let obj = texture
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{func}: expected an Image or a Canvas"))?;
    Ok((obj, handle, w as f64, h as f64))
}

/// `Graphics.newParticleSystem([texture, size])` — an emitter of up to
/// `size` live particles (1000 by default), drawn as `texture` or, with none,
/// as squares.
#[saule_export(class = "Graphics", name = "newParticleSystem")]
fn graphics_new_particle_system(
    texture: Option<SValue>,
    size: Option<i64>,
) -> Result<ParticleSystem, String> {
    let texture = match texture {
        Some(t) if !t.is_nil() => Some(texture_of(&t, "Graphics.newParticleSystem")?),
        _ => None,
    };
    let limit = size.unwrap_or(1000);
    if limit <= 0 {
        return Err("Graphics.newParticleSystem: the size must be positive".into());
    }
    Ok(ParticleSystem {
        texture,
        particles: Vec::with_capacity(limit.min(4096) as usize),
        limit: limit as usize,
        rng: Rng::from_clock(),
        x: 0.0,
        y: 0.0,
        active: true,
        paused: false,
        rate: 0.0,
        accum: 0.0,
        emitter_lifetime: -1.0,
        emitter_left: -1.0,
        life: (1.0, 1.0),
        speed: (0.0, 0.0),
        direction: 0.0,
        spread: 0.0,
        accel: (0.0, 0.0, 0.0, 0.0),
        damping: (0.0, 0.0),
        radial: (0.0, 0.0),
        tangential: (0.0, 0.0),
        sizes: vec![1.0],
        size_variation: 0.0,
        rotation: (0.0, 0.0),
        spin: (0.0, 0.0),
        relative_rotation: false,
        colors: vec![[1.0; 4]],
        area: (AreaSpread::None, 0.0, 0.0),
        offset: None,
    })
}
