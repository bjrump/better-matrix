//! The sign-in screen's backdrop: a soft glow rising over a horizon, drawn in ASCII characters
//! that flicker and scramble where the pointer passes. This is the light field, sampled once
//! per character cell on each tick; `ui::backdrop` colours it with the palette and picks the
//! characters.
//!
//! Ticks follow Rune's clock (virtual when headless), a dozen a second while the backdrop shows
//! in a focused window, so the view rebuilds that often and no more. The glow underneath is an
//! image streamed to the GPU, upscaled smoothly, which costs no view rebuild.

use std::collections::VecDeque;
use std::time::Duration;

/// Seconds between ticks.
pub const TICK: f64 = 1.0 / 12.0;
/// A character cell's height, in points; its width is the monospace font's advance.
pub const CELL_HEIGHT: f32 = 18.0;
/// Ticks the backdrop takes to rise in.
const INTRO_TICKS: u64 = 29;
/// Ticks a trail point scrambles the characters under it.
const TRAIL_TICKS: u64 = 8;
/// The most trail points kept.
const MAX_TRAIL: usize = 24;
/// How far a trail point scrambles, as a share of the backdrop's height.
const SCRAMBLE_RADIUS: f32 = 0.1;

/// The light at one character cell.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// The rim of the horizon and the sky above it, each 0 to 1, coloured along the palette's
    /// ramp, and how much of the sky shows (0 below the horizon, 1 above).
    pub rim: f32,
    pub sky: f32,
    pub sky_share: f32,
    /// How far the intro has revealed the cell, 0 to 1.
    pub reveal: f32,
    /// How strongly the pointer's trail scrambles the cell's character, 0 to 1.
    pub scramble: f32,
}

/// The light sampled over the grid of character cells, rows top first.
#[derive(Clone, Debug, Default)]
pub struct Field {
    pub columns: usize,
    pub rows: usize,
    pub cells: Vec<Sample>,
    /// The tick it was sampled at, which also seeds the characters' flicker.
    pub tick: u64,
}

impl Field {
    pub fn at(&self, column: usize, row: usize) -> Sample {
        self.cells.get(row * self.columns + column).copied().unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct Backdrop {
    /// The backdrop's size in points, once laid out.
    size: Option<[f32; 2]>,
    /// The monospace font's advance, measured by the view.
    advance: Option<f32>,
    /// Ticks since the backdrop began showing.
    tick: u64,
    /// The clock's tick it began showing at; `None` while it doesn't show.
    started: Option<u64>,
    /// Where the pointer passed, in field coordinates, and when.
    trail: VecDeque<([f32; 2], u64)>,
    pub field: Field,
    /// The glow under the characters, one pixel per cell.
    pub glow: rune::Pixels,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self {
            size: None,
            advance: None,
            tick: 0,
            started: None,
            trail: VecDeque::new(),
            field: Field::default(),
            glow: rune::Pixels::new(),
        }
    }
}

impl Backdrop {
    /// The backdrop was laid out at `size` points.
    pub fn resized(&mut self, size: [f32; 2]) {
        if self.size != Some(size) {
            self.size = Some(size);
            self.sample();
        }
    }

    /// The view measured the monospace font's advance.
    pub fn measured(&mut self, advance: f32) {
        if advance > 0.0 && self.advance != Some(advance) {
            self.advance = Some(advance);
            self.sample();
        }
    }

    /// The pointer moved to `at` points within the backdrop.
    pub fn pointer(&mut self, at: [f32; 2]) {
        let Some(point) = self.size.map(|size| to_field(at, size)) else { return };
        let close = self.trail.back().is_some_and(|(last, _)| distance(*last, point) < 0.015);
        if !close {
            self.trail.push_back((point, self.tick));
            if self.trail.len() > MAX_TRAIL {
                self.trail.pop_front();
            }
        }
    }

    /// Moves the backdrop on to the tick of `clock` (seconds); true when it moved. `wait` is
    /// how long until the next tick, `None` once it holds still: under reduced motion
    /// (`still`), once risen in. The first call after [`Backdrop::hidden`] starts the intro.
    pub fn advance_to(&mut self, clock: f64, still: bool) -> (bool, Option<Duration>) {
        if still && self.tick >= INTRO_TICKS && self.trail.is_empty() {
            return (false, None);
        }
        let now = (clock / TICK).floor().max(0.0) as u64;
        let tick = now - *self.started.get_or_insert(now);
        let moved = tick != self.tick || self.field.cells.is_empty();
        if moved {
            self.tick = tick;
            self.trail.retain(|(_, at)| tick.saturating_sub(*at) < TRAIL_TICKS);
            self.sample();
        }
        let wait = ((now + 1) as f64 * TICK - clock).max(0.001);
        (moved, Some(Duration::from_secs_f64(wait)))
    }

    /// The backdrop no longer shows: shown again, it rises in anew.
    pub fn hidden(&mut self) {
        if self.started.take().is_some() {
            self.tick = 0;
            self.trail.clear();
            self.field = Field::default();
        }
    }

    /// Samples the light over the grid for the current tick.
    fn sample(&mut self) {
        let (Some([width, height]), Some(advance)) = (self.size, self.advance) else { return };
        let columns = (width / advance).ceil().max(1.0) as usize;
        let rows = (height / CELL_HEIGHT).ceil().max(1.0) as usize;
        let aspect = width / height.max(1.0);
        let time = (self.tick as f64 * TICK * 0.5) as f32;
        let intro = (self.tick as f32 / INTRO_TICKS as f32).min(1.0);
        let trail: Vec<([f32; 2], f32)> = self
            .trail
            .iter()
            .map(|(at, tick)| (*at, 1.0 - self.tick.saturating_sub(*tick) as f32 / TRAIL_TICKS as f32))
            .collect();
        let mut cells = Vec::with_capacity(columns * rows);
        for row in 0..rows {
            for column in 0..columns {
                let at = [(column as f32 + 0.5) * advance, (row as f32 + 0.5) * CELL_HEIGHT];
                let p = to_field(at, [width, height]);
                cells.push(sample(p, aspect, time, intro, &trail));
            }
        }
        self.field = Field { columns, rows, cells, tick: self.tick };
    }
}

/// A point in points to field coordinates: the centre at 0, y up, the height spanning 1.
fn to_field([x, y]: [f32; 2], [width, height]: [f32; 2]) -> [f32; 2] {
    let height = height.max(1.0);
    [(x - width / 2.0) / height, 0.5 - y / height]
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The fractional part, always 0 to 1 (GLSL's `fract`).
fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// A hash of a point to 0..1, stable across platforms.
pub fn hash(x: f32, y: f32) -> f32 {
    let mut q = [fract(x * 0.1031), fract(y * 0.1031), fract(x * 0.1031)];
    let d = q[0] * (q[1] + 33.33) + q[1] * (q[2] + 33.33) + q[2] * (q[0] + 33.33);
    q = q.map(|v| v + d);
    fract((q[0] + q[1]) * q[2])
}

/// Two hashes of a point, each -1 to 1, for gradient noise.
fn hash2(x: f32, y: f32) -> [f32; 2] {
    [hash(x, y) * 2.0 - 1.0, hash(y + 19.19, x + 47.11) * 2.0 - 1.0]
}

/// Smooth gradient noise, 0 to 1.
fn noise(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let fade = |f: f32| f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let (ux, uy) = (fade(fx), fade(fy));
    let corner = |cx: f32, cy: f32| {
        let g = hash2(ix + cx, iy + cy);
        g[0] * (fx - cx) + g[1] * (fy - cy)
    };
    let top = mix(corner(0.0, 0.0), corner(1.0, 0.0), ux);
    let bottom = mix(corner(0.0, 1.0), corner(1.0, 1.0), ux);
    0.5 + 0.5 * mix(top, bottom, uy)
}

/// The light at field point `p`: a horizon arching low across the screen, its rim glowing, a
/// sky above lit away from a slowly drifting sun, the whole gently warped by noise.
fn sample(p: [f32; 2], aspect: f32, time: f32, intro: f32, trail: &[([f32; 2], f32)]) -> Sample {
    const SEED: [f32; 2] = [3.7, 8.1];
    let warp = [
        noise(p[0] * 1.6 + SEED[0], p[1] * 1.6 + SEED[1] + time * 0.3) - 0.5,
        noise(p[0] * 1.6 + SEED[1] + 4.3, p[1] * 1.6 + SEED[0] - time * 0.24) - 0.5,
    ];
    let q = [p[0] + warp[0] * 0.09, p[1] + warp[1] * 0.09];

    let span = 0.5 * aspect.max(1.0);
    let x = q[0] / span;
    let arch = -0.17 - 0.2 * x * x + 0.02 * (time * 0.6).sin();
    let soft = 0.135;
    let edge = q[1] - arch;
    let rim = 0.38 * smoothstep(-soft, 0.42, edge);
    let sun = [0.08 * (time * 0.31).sin() * span, 0.58];
    let spread = ((q[0] - sun[0]) / (0.95 * span)).hypot((q[1] - sun[1]) / 0.75);
    let sky = mix(0.55, 1.0, smoothstep(0.2, 0.95, spread));
    let order = (edge * 1.3).clamp(0.0, 1.0);

    let reveal = smoothstep(0.0, 1.0, ((intro * 1.45 - order) / 0.45).clamp(0.0, 1.0));
    let scramble = trail.iter().fold(0.0f32, |most, (at, strength)| {
        let d = distance(p, *at);
        most.max(strength * (-(d * d) / (SCRAMBLE_RADIUS * SCRAMBLE_RADIUS)).exp())
    });
    let dim = mix(0.7, 1.0, reveal);
    Sample { rim: rim * dim, sky: sky * dim, sky_share: smoothstep(0.18, 0.62, edge), reveal, scramble }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn laid_out() -> Backdrop {
        let mut backdrop = Backdrop::default();
        backdrop.resized([800.0, 600.0]);
        backdrop.measured(8.0);
        backdrop
    }

    #[test]
    fn the_field_should_cover_the_backdrop_in_cells() {
        let backdrop = laid_out();
        assert_eq!((backdrop.field.columns, backdrop.field.rows), (100, 34));
        assert_eq!(backdrop.field.cells.len(), 100 * 34);
    }

    #[test]
    fn the_backdrop_should_move_once_per_tick() {
        let mut backdrop = laid_out();
        backdrop.advance_to(1.0, false);
        assert!(!backdrop.advance_to(1.0 + TICK / 2.0, false).0);
        assert!(backdrop.advance_to(1.0 + TICK, false).0);
    }

    #[test]
    fn under_reduced_motion_it_should_hold_still_once_risen() {
        let mut backdrop = laid_out();
        backdrop.advance_to(0.0, true);
        assert!(backdrop.advance_to(TICK * f64::from(INTRO_TICKS as u32 + 1), true).0);
        assert_eq!(backdrop.advance_to(TICK * f64::from(INTRO_TICKS as u32 + 5), true), (false, None));
    }

    #[test]
    fn showing_it_again_should_rise_in_anew() {
        let mut backdrop = laid_out();
        backdrop.advance_to(0.0, false);
        backdrop.advance_to(10.0, false);
        backdrop.hidden();
        backdrop.advance_to(20.0, false);
        assert_eq!(backdrop.field.tick, 0);
    }

    #[test]
    fn the_sky_should_be_brighter_than_the_ground() {
        let mut backdrop = laid_out();
        backdrop.advance_to(0.0, false);
        backdrop.advance_to(TICK * f64::from(INTRO_TICKS as u32), false);
        let (top, bottom) = (backdrop.field.at(50, 2), backdrop.field.at(50, 33));
        assert!(top.sky_share > 0.9 && bottom.sky_share < 0.1);
    }

    #[test]
    fn the_pointer_should_scramble_the_cells_it_passes() {
        let mut backdrop = laid_out();
        backdrop.advance_to(0.0, false);
        backdrop.pointer([400.0, 300.0]);
        backdrop.advance_to(TICK, false);
        let field = &backdrop.field;
        assert!(field.at(50, 16).scramble > 0.5 && field.at(5, 2).scramble < 0.01);
    }

    #[test]
    fn noise_should_stay_between_zero_and_one() {
        let samples = (0..400).map(|i| noise(i as f32 * 0.37, i as f32 * 0.21));
        assert!(samples.into_iter().all(|n| (0.0..=1.0).contains(&n)));
    }
}
