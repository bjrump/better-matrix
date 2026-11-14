//! A glow along the bottom of the pane a voice memo records in that moves like water, stirred
//! by the voice: loudness sloshes it from wall to wall, where it runs up and splashes, and sharp
//! sounds (a peak well above the loudness) throw sparks off the walls. It is light, not a body
//! of water: brightest at the bottom, fading to nothing a little above its wavy surface. A height field across the width, stepped on
//! Rune's clock from `poll` and drawn into a `Pixels` image at half resolution, so it moves
//! without rebuilding the view and runs the same headless.

use rune::Pixels;

/// The water's area: the bottom of the pane, in points.
pub const WATER_HEIGHT: f32 = 340.0;
/// Pixels drawn per point; the image is upscaled smoothly, which suits water.
const RESOLUTION: f32 = 0.5;
/// Points between two columns of the height field.
const COLUMN: f32 = 4.0;
/// Where the water rests, in points above the bottom.
const REST: f32 = 120.0;
/// How far above its surface the glow fades out, as a share of the surface's height.
const HALO: f32 = 0.45;
/// The simulation's fixed step, in seconds.
const STEP: f64 = 1.0 / 60.0;
/// Most spray drops in the air at once.
const MAX_DROPS: usize = 90;

/// How far peak may rise above loudness, in dB, before a sound counts as sharp.
const SHARP_DB: f32 = 9.0;

// The glow's blues, straight sRGB, all saturated: blended over a dark background, any light or
// pale blue turns to a grey haze. The primary blue where it is brightest, a deeper blue above
// it, and a light blue only for the sparks' cores.
const PALE: [f32; 3] = [0.376, 0.647, 0.980];
const LIGHT: [f32; 3] = [0.231, 0.510, 0.965];
const BODY: [f32; 3] = [0.145, 0.388, 0.922];

#[derive(Clone, Copy, Debug)]
struct Drop {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    size: f32,
}

#[derive(Clone, Debug)]
pub struct Water {
    /// The area's size in points, once laid out.
    size: Option<[f32; 2]>,
    /// Each column's height above the rest level, and its speed, in points and points a second.
    height: Vec<f32>,
    speed: Vec<f32>,
    /// Per-step working space, kept so a step allocates nothing.
    scratch: Vec<f32>,
    drops: Vec<Drop>,
    /// The clock the simulation has reached.
    clock: Option<f64>,
    /// Seconds the voice has stirred the water, which sets the sloshing's phase.
    time: f64,
    /// The loudness the water follows, eased so the sloshing swells and settles.
    energy: f32,
    seed: u32,
    pub pixels: Pixels,
}

impl Default for Water {
    fn default() -> Self {
        Self {
            size: None,
            height: Vec::new(),
            speed: Vec::new(),
            scratch: Vec::new(),
            drops: Vec::new(),
            clock: None,
            time: 0.0,
            energy: 0.0,
            seed: 0x9e37_79b9,
            pixels: Pixels::new(),
        }
    }
}

impl Water {
    /// The area was laid out at `size` points.
    pub fn resized(&mut self, size: [f32; 2]) {
        if self.size == Some(size) {
            return;
        }
        self.size = Some(size);
        let columns = ((size[0] / COLUMN).ceil() as usize).max(8);
        self.height = vec![0.0; columns];
        self.speed = vec![0.0; columns];
        self.scratch = vec![0.0; columns];
        self.drops.clear();
        self.draw();
    }

    /// How long from `clock` until the next step is due: waking then, rather than a step after
    /// each wake, keeps the steps on the simulation's own beat.
    pub fn wait(&self, clock: f64) -> std::time::Duration {
        let next = self.clock.map_or(clock, |at| at + STEP);
        std::time::Duration::from_secs_f64((next - clock).max(0.001))
    }

    /// Stops following the clock (an unfocused window, reduced motion): the water holds where
    /// it is, and picks up from there without replaying the time missed.
    pub fn pause(&mut self) {
        self.clock = None;
    }

    /// Runs the water on to `clock` (seconds), stirred by the voice's loudness (`meter`, 0 to
    /// 1) and how far its peak stands above that (`crest`, dB). Draws the result.
    pub fn advance(&mut self, clock: f64, meter: f32, crest: f32) {
        if self.height.is_empty() {
            return;
        }
        let from = *self.clock.get_or_insert(clock);
        // A stall (a hidden window) doesn't replay seconds of water.
        let steps = (((clock - from) / STEP).floor() as usize).min(4);
        if steps == 0 {
            return;
        }
        self.clock = Some(if steps == 4 { clock } else { from + steps as f64 * STEP });
        for _ in 0..steps {
            self.step(meter, crest);
        }
        self.draw();
    }

    fn random(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed as f32 / u32::MAX as f32
    }

    fn step(&mut self, meter: f32, crest: f32) {
        /// Substeps per step: the wave term is stiff, and a quarter step keeps it stable.
        const SUBSTEPS: usize = 4;
        let dt = STEP as f32;
        let ease = if meter > self.energy { 0.12 } else { 0.03 };
        self.energy += (meter - self.energy) * ease;
        self.time += STEP;
        let n = self.height.len();
        let last = (n - 1) as f32;
        // The voice pushes the water at the walls: one side, then the other (sloshing), and both
        // at once (surging), so it runs up the walls and waves roll in from them. The push
        // follows the voice; a breath of it stays while quiet.
        let stir = 0.03 + self.energy;
        let slosh = (self.time * 2.6).sin() as f32 * stir * 1500.0;
        let surge = ((self.time * 4.1).sin() as f32 * 0.5 + 0.5) * stir * 900.0;
        // How the push falls off from each wall: within a few percent of the width.
        let near = |d: f32| (-d / 0.06).exp();
        let top = WATER_HEIGHT - REST - 12.0;
        let sub = dt / SUBSTEPS as f32;
        let mut accel = std::mem::take(&mut self.scratch);
        for _ in 0..SUBSTEPS {
            for (i, a) in accel.iter_mut().enumerate() {
                let left = self.height[i.saturating_sub(1)];
                let right = self.height[(i + 1).min(n - 1)];
                let x = i as f32 / last;
                let wave = 20_000.0 * (left + right - 2.0 * self.height[i]);
                let pull = -14.0 * self.height[i] - 1.6 * self.speed[i];
                let drive = slosh * (near(x) - near(1.0 - x)) + surge * (near(x) + near(1.0 - x));
                *a = wave + pull + drive;
            }
            for (speed, a) in self.speed.iter_mut().zip(&accel) {
                *speed += a * sub;
            }
            // A little viscosity: neighbouring columns share speed, so the surface stays smooth.
            // `accel` is free again, so it holds the smoothed speeds.
            for (i, smoothed) in accel.iter_mut().enumerate() {
                let (left, right) = (self.speed[i.saturating_sub(1)], self.speed[(i + 1).min(n - 1)]);
                *smoothed = self.speed[i] + 0.12 * (left + right - 2.0 * self.speed[i]);
            }
            std::mem::swap(&mut self.speed, &mut accel);
            for i in 0..n {
                self.height[i] = (self.height[i] + self.speed[i] * sub).clamp(-REST, top);
            }
        }
        self.scratch = accel;
        // Ripples across the surface while speaking.
        if self.energy > 0.05 && self.random() < self.energy * 0.5 {
            let centre = self.random() * last;
            let push = (self.random() - 0.5) * 500.0 * self.energy;
            for (i, speed) in self.speed.iter_mut().enumerate() {
                let d = (i as f32 - centre) / 4.0;
                *speed += push * (-d * d).exp();
            }
        }
        // A sharp sound throws spray off both walls, where the water meets them.
        let sharp = ((crest - SHARP_DB) / 10.0).clamp(0.0, 1.0) * meter;
        if sharp > 0.05 && self.random() < sharp * 0.8 {
            self.splash(sharp);
        }
        self.fly_drops(dt);
    }

    fn splash(&mut self, strength: f32) {
        let Some([width, _]) = self.size else { return };
        let last = self.height.len() - 1;
        for wall in [0, last] {
            let inward = if wall == 0 { 1.0 } else { -1.0 };
            self.speed[wall] += 260.0 * strength;
            let surface = REST + self.height[wall];
            for _ in 0..3 {
                if self.drops.len() >= MAX_DROPS {
                    break;
                }
                let drop = Drop {
                    x: if wall == 0 { 2.0 } else { width - 2.0 } + inward * self.random() * 10.0,
                    y: surface,
                    vx: inward * (40.0 + self.random() * 160.0) * strength,
                    vy: (220.0 + self.random() * 380.0) * strength,
                    size: 1.5 + self.random() * 2.5,
                };
                self.drops.push(drop);
            }
        }
    }

    /// Spray rises, falls, and goes back into the water.
    fn fly_drops(&mut self, dt: f32) {
        let width = self.size.map_or(0.0, |[w, _]| w);
        let (height, last) = (&self.height, self.height.len().saturating_sub(1));
        self.drops.retain_mut(|drop| {
            drop.vy -= 900.0 * dt;
            drop.x += drop.vx * dt;
            drop.y += drop.vy * dt;
            let column = ((drop.x / COLUMN).max(0.0) as usize).min(last);
            let surface = REST + height.get(column).copied().unwrap_or(0.0);
            (drop.vy > 0.0 || drop.y > surface) && (0.0..=width).contains(&drop.x)
        });
    }

    /// The surface's height in points at `x` points from the left, smoothed over a few
    /// columns so it reads as water rather than a graph.
    fn surface(&self, x: f32) -> f32 {
        let n = self.height.len();
        let smoothed = |i: isize| {
            let at = |k: isize| self.height[k.clamp(0, n as isize - 1) as usize];
            (at(i - 2) + 2.0 * at(i - 1) + 3.0 * at(i) + 2.0 * at(i + 1) + at(i + 2)) / 9.0
        };
        let at = (x / COLUMN).max(0.0);
        let i = (at as isize).min(n as isize - 1);
        let f = at - at.floor();
        REST + smoothed(i) * (1.0 - f) + smoothed(i + 1) * f
    }

    fn draw(&self) {
        let Some([width, height]) = self.size else { return };
        if self.height.is_empty() {
            return;
        }
        let (w, h) = (((width * RESOLUTION).ceil() as usize).max(1), ((height * RESOLUTION).ceil() as usize).max(1));
        let mut rgba = vec![0u8; w * h * 4];
        let put = |rgba: &mut [u8], x: usize, y: usize, color: [f32; 3], alpha: f32| {
            let at = (y * w + x) * 4;
            let under = f32::from(rgba[at + 3]) / 255.0;
            let out = alpha + under * (1.0 - alpha);
            if out <= 0.0 {
                return;
            }
            for c in 0..3 {
                let below = f32::from(rgba[at + c]) / 255.0;
                rgba[at + c] = (((color[c] * alpha + below * under * (1.0 - alpha)) / out) * 255.0).round() as u8;
            }
            rgba[at + 3] = (out * 255.0).round() as u8;
        };
        // Brighter while speaking; a quiet glow stays.
        let strength = 0.38 + 0.47 * self.energy.min(1.0);
        for x in 0..w {
            let points = (x as f32 + 0.5) / RESOLUTION;
            let surface = self.surface(points).max(1.0);
            // Light gathers where the water meets a wall.
            let wall = (-points.min(width - points) / 70.0).exp();
            for y in 0..h {
                // How far up the glow the pixel sits: 0 at the bottom, 1 at the surface.
                let t = (height - (y as f32 + 0.5) / RESOLUTION) / surface;
                if t >= 1.0 + HALO {
                    continue;
                }
                let fall = 1.0 - smoothstep(0.0, 1.0 + HALO, t);
                let alpha = fall * fall * strength * (0.85 + 0.3 * wall);
                let color = mix3(LIGHT, BODY, smoothstep(0.0, 0.5, t));
                put(&mut rgba, x, y, color, alpha.min(1.0));
            }
        }
        // Sparks: a bright point in a soft halo.
        for drop in &self.drops {
            let (cx, cy) = (drop.x * RESOLUTION, (height - drop.y) * RESOLUTION);
            let r = (drop.size * RESOLUTION).max(0.8);
            let reach = r * 3.0;
            let (x0, x1) = ((cx - reach).max(0.0) as usize, ((cx + reach).max(0.0) as usize).min(w - 1));
            let (y0, y1) = ((cy - reach).max(0.0) as usize, ((cy + reach).max(0.0) as usize).min(h - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                    let core = (r + 0.5 - d).clamp(0.0, 1.0);
                    let halo = (1.0 - d / reach).clamp(0.0, 1.0).powi(2) * 0.45;
                    let alpha = core.max(halo) * 0.9;
                    if alpha > 0.0 {
                        put(&mut rgba, x, y, if core > halo { PALE } else { LIGHT }, alpha);
                    }
                }
            }
        }
        self.pixels.set(w as u32, h as u32, rgba);
    }

    /// The highest point of the surface, in points above the bottom.
    #[cfg(test)]
    fn highest(&self) -> f32 {
        self.height.iter().fold(f32::MIN, |most, h| most.max(REST + h))
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(water: &mut Water, seconds: f64, meter: f32, crest: f32) {
        let start = water.clock.unwrap_or(0.0);
        let mut clock = start;
        while clock < start + seconds {
            clock += STEP;
            water.advance(clock, meter, crest);
        }
    }

    fn laid_out() -> Water {
        let mut water = Water::default();
        water.resized([800.0, WATER_HEIGHT]);
        water
    }

    #[test]
    fn a_loud_voice_should_lift_the_water_at_the_walls_more_than_in_the_middle() {
        let mut water = laid_out();
        run(&mut water, 3.0, 0.9, 0.0);
        let walls = water.height[0].abs().max(water.height[water.height.len() - 1].abs());
        let middle = water.height[water.height.len() / 2].abs();
        assert!(walls > 20.0 && walls > middle, "walls {walls}, middle {middle}");
    }

    #[test]
    fn silence_should_leave_the_water_nearly_calm() {
        let mut water = laid_out();
        run(&mut water, 3.0, 0.0, 0.0);
        assert!(water.highest() < REST + 10.0);
    }

    #[test]
    fn a_sharp_sound_should_throw_spray() {
        let mut water = laid_out();
        run(&mut water, 1.0, 0.8, 20.0);
        assert!(!water.drops.is_empty());
    }

    #[test]
    fn a_steady_tone_should_throw_no_spray() {
        let mut water = laid_out();
        run(&mut water, 1.0, 0.8, 3.0);
        assert!(water.drops.is_empty());
    }

    #[test]
    fn the_water_should_stay_inside_its_area() {
        let mut water = laid_out();
        run(&mut water, 5.0, 1.0, 20.0);
        assert!(water.highest() < WATER_HEIGHT);
    }
}
