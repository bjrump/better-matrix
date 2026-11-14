//! The glow behind a call: a few soft lights in the palette's blues that wander the stage on
//! slow, never-repeating paths and swell with the voices in the call. Each light follows one
//! participant's loudness (the user's own included), so who speaks shows as a brightening. It
//! draws into a small image the view stretches over the stage, so moving it rebuilds nothing.

use rune::Pixels;

/// Simulated steps a second: the lights drift slowly, so half the display rate is smooth.
const STEP: f64 = 1.0 / 30.0;
/// Image pixels per stage point: the lights are soft, so a coarse image upscaled is enough.
const RESOLUTION: f32 = 0.1;
/// The lights: their colour (linear-ish sRGB, 0 to 1), size (a fraction of the stage's shorter
/// side) and the frequencies and phases of their paths.
const LIGHTS: [Light; 4] = [
    Light { color: [0.145, 0.388, 0.922], size: 0.55, fx: 0.071, fy: 0.053, phase: 0.0 },
    Light { color: [0.231, 0.510, 0.965], size: 0.45, fx: 0.047, fy: 0.067, phase: 1.9 },
    Light { color: [0.376, 0.647, 0.980], size: 0.38, fx: 0.059, fy: 0.043, phase: 3.7 },
    Light { color: [0.090, 0.220, 0.620], size: 0.60, fx: 0.037, fy: 0.061, phase: 5.3 },
];
/// How bright a light rests, and how much a voice adds.
const REST: f32 = 0.30;
const SWELL: f32 = 0.55;
/// The image's most opaque point: the stage behind stays readable.
const MAX_ALPHA: f32 = 0.7;

struct Light {
    color: [f32; 3],
    size: f32,
    fx: f32,
    fy: f32,
    phase: f32,
}

#[derive(Debug)]
pub struct CallGlow {
    size: Option<[f32; 2]>,
    /// The clock of the last step, while following it.
    clock: Option<f64>,
    /// Time along the paths; voices speed it up a little.
    time: f64,
    /// Each light's smoothed loudness, 0 to 1.
    energy: [f32; LIGHTS.len()],
    pub pixels: Pixels,
}

impl Default for CallGlow {
    fn default() -> Self {
        Self { size: None, clock: None, time: 0.0, energy: [0.0; LIGHTS.len()], pixels: Pixels::new() }
    }
}

impl CallGlow {
    /// The stage was laid out at `size` points.
    pub fn resized(&mut self, size: [f32; 2]) {
        if self.size != Some(size) {
            self.size = Some(size);
            self.draw();
        }
    }

    /// How long from `clock` until the next step is due.
    pub fn wait(&self, clock: f64) -> std::time::Duration {
        let next = self.clock.map_or(clock, |at| at + STEP);
        std::time::Duration::from_secs_f64((next - clock).max(0.001))
    }

    /// Stops following the clock (an unfocused window, reduced motion): the lights hold still.
    pub fn pause(&mut self) {
        self.clock = None;
    }

    /// Runs on to `clock` (seconds), the lights swelling with `levels` (0 to 1, one per
    /// participant; lights beyond them follow the loudest). Draws when a step was due.
    pub fn advance(&mut self, clock: f64, levels: &[f32]) {
        let Some(last) = self.clock else {
            self.clock = Some(clock);
            return;
        };
        let elapsed = clock - last;
        if elapsed < STEP {
            return;
        }
        // A long gap (a hidden window) resumes rather than racing to catch up.
        let dt = elapsed.min(0.25) as f32;
        self.clock = Some(clock);
        let loudest = levels.iter().copied().fold(0.0, f32::max);
        for (index, energy) in self.energy.iter_mut().enumerate() {
            let target = levels.get(index).copied().unwrap_or(loudest).clamp(0.0, 1.0);
            // Quick to rise with a voice, slow to settle after it.
            let rate = if target > *energy { 9.0 } else { 1.6 };
            *energy += (target - *energy) * (1.0 - (-rate * dt).exp());
        }
        let average = self.energy.iter().sum::<f32>() / LIGHTS.len() as f32;
        self.time += f64::from(dt * (1.0 + 1.5 * average));
        self.draw();
    }

    /// Where each light is now and how strong, in image pixels.
    fn lights(&self, w: f32, h: f32) -> [(f32, f32, f32, f32); LIGHTS.len()] {
        let t = self.time as f32;
        let side = w.min(h);
        std::array::from_fn(|index| {
            let light = &LIGHTS[index];
            let energy = self.energy[index];
            let x = 0.5 + 0.38 * (t * light.fx * std::f32::consts::TAU + light.phase).sin();
            let y = 0.5 + 0.34 * (t * light.fy * std::f32::consts::TAU + light.phase * 1.3).cos();
            let radius = side * light.size * (1.0 + 0.35 * energy);
            (x * w, y * h, radius, REST + SWELL * energy)
        })
    }

    fn draw(&mut self) {
        let Some([width, height]) = self.size else { return };
        let (w, h) = (((width * RESOLUTION).ceil() as usize).max(2), ((height * RESOLUTION).ceil() as usize).max(2));
        let lights = self.lights(w as f32, h as f32);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let (mut color, mut weight) = ([0.0f32; 3], 0.0f32);
                for ((cx, cy, radius, strength), light) in lights.iter().zip(&LIGHTS) {
                    let d2 = ((px - cx).powi(2) + (py - cy).powi(2)) / (radius * radius);
                    let w = strength * (-2.2 * d2).exp();
                    for (sum, channel) in color.iter_mut().zip(light.color) {
                        *sum += channel * w;
                    }
                    weight += w;
                }
                if weight <= 0.001 {
                    continue;
                }
                let at = (y * w + x) * 4;
                for (out, sum) in rgba[at..at + 3].iter_mut().zip(color) {
                    *out = ((sum / weight).clamp(0.0, 1.0) * 255.0) as u8;
                }
                rgba[at + 3] = (weight.min(1.0) * MAX_ALPHA * 255.0) as u8;
            }
        }
        self.pixels.set(w as u32, h as u32, rgba);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glow() -> CallGlow {
        let mut glow = CallGlow::default();
        glow.resized([800.0, 500.0]);
        glow.advance(0.0, &[]);
        glow
    }

    #[test]
    fn a_voice_should_swell_its_light() {
        let mut glow = glow();
        for step in 1..=10 {
            glow.advance(f64::from(step) * STEP, &[1.0]);
        }
        assert!(glow.energy[0] > 0.5);
    }

    #[test]
    fn silence_should_let_the_lights_settle() {
        let mut glow = glow();
        glow.energy = [1.0; LIGHTS.len()];
        for step in 1..=90 {
            glow.advance(f64::from(step) * STEP, &[0.0]);
        }
        assert!(glow.energy.iter().all(|energy| *energy < 0.1));
    }

    #[test]
    fn the_lights_should_move_with_time() {
        let mut glow = glow();
        let before = glow.lights(80.0, 50.0)[0];
        for step in 1..=30 {
            glow.advance(f64::from(step) * STEP, &[]);
        }
        assert_ne!(glow.lights(80.0, 50.0)[0].0, before.0);
    }

    #[test]
    fn a_paused_glow_should_not_jump_ahead_when_it_resumes() {
        let mut glow = glow();
        glow.advance(STEP, &[]);
        let time = glow.time;
        glow.pause();
        glow.advance(100.0, &[]);
        assert_eq!(glow.time, time);
    }
}
