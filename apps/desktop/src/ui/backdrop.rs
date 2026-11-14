//! The sign-in screen's backdrop (see `state::backdrop`): the sampled light coloured along a
//! ramp of the palette's blues, a soft glow underneath, and characters from ` =+xX0S8#@` picked
//! by brightness, flickering, and scrambled where the pointer passed. One text element per row
//! of cells, its characters coloured by spans; the whole is memoized per tick.

use rune::{Color, El, Span, TextStyle, col, el, memo, oklch, text};

use crate::state::State;
use crate::state::backdrop::{Backdrop, CELL_HEIGHT, Field, Sample, hash};

const CHARACTERS: &[u8] = b" =+xX0S8#@";
/// The characters' font: a cell's height is its line.
const STYLE: TextStyle = TextStyle::new(14.0, 500.0).lh(CELL_HEIGHT).mono();
/// How strongly the glow shows under the characters.
const GLOW: f32 = 0.9;
/// Share of cells that may show a character, and how opaque characters are.
const DENSITY: f32 = 0.8;
const OPACITY: f32 = 0.85;
const FLICKER: f32 = 0.2;
/// Characters the font's advance is measured over.
const MEASURE: &str = "00000000000000000000";

/// The ramp the light is coloured along, dim to bright: the palette's blues, in Oklab.
fn ramp(dark: bool) -> [[f32; 4]; 4] {
    let deep = oklch(0.3791, 0.1378, 265.5222);
    let primary = oklch(0.6231, 0.1880, 259.8145);
    let pale = oklch(0.8823, 0.0571, 254.1284);
    let white = oklch(0.97, 0.015, 255.0);
    // On light, the same story in ink: a strong rim, the sky paling toward the sun.
    let light = oklch(0.80, 0.10, 255.0);
    let colors = if dark { [deep, white, pale, primary] } else { [primary, white, pale, light] };
    colors.map(Color::to_oklab)
}

/// A colour `t` of the way along the ramp (in Oklab), with its alpha.
fn tone(ramp: &[[f32; 4]; 4], t: f32) -> ([f32; 3], f32) {
    let x = smoothstep(0.3, 1.0, t) * (ramp.len() - 1) as f32;
    let index = (x.floor() as usize).min(ramp.len() - 1);
    let (a, b) = (ramp[index], ramp[(index + 1).min(ramp.len() - 1)]);
    let f = x - x.floor();
    ([mix(a[0], b[0], f), mix(a[1], b[1], f), mix(a[2], b[2], f)], smoothstep(0.0, 0.38, t))
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Linear light to sRGB, 0 to 1.
fn srgb_encode(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB to linear light, 0 to 1.
fn srgb_decode(c: f32) -> f32 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A cell's light as a colour and its coverage.
fn light(ramp: &[[f32; 4]; 4], cell: Sample) -> Color {
    let (rim, rim_a) = tone(ramp, cell.rim);
    let (sky, sky_a) = tone(ramp, cell.sky);
    let t = cell.sky_share;
    let lab = [mix(rim[0], sky[0], t), mix(rim[1], sky[1], t), mix(rim[2], sky[2], t), 1.0];
    Color::from_oklab(lab).alpha(mix(rim_a, sky_a, t) * cell.reveal)
}

/// The character a cell shows at `tick` and its colour, if any.
fn glyph(cell: Sample, color: Color, column: usize, row: usize, tick: u64, dark: bool) -> Option<(u8, Color)> {
    let (x, y) = (column as f32, row as f32);
    let shown = Color { r: color.r * color.a, g: color.g * color.a, b: color.b * color.a, a: color.a };
    let visible = if dark {
        shown
    } else {
        Color { r: shown.r + 1.0 - color.a, g: shown.g + 1.0 - color.a, b: shown.b + 1.0 - color.a, a: 1.0 }
    };
    let luma = visible.luminance();
    let value = if dark { luma } else { ((1.0 - luma) * 1.5).clamp(0.0, 1.0) };
    let pick = hash(x, y);
    let level = hash(x * 1.73 + 4.1, y * 1.73 + 4.1);
    // The reference's glyph clock runs nine flickers a second; ticks come twelve.
    let time = tick as f32 / 12.0;
    let beat = (time * 9.0 + level * 9.0).floor();
    let wobble = (hash(x * 2.31 + beat * 17.7, y * 2.31 + beat * 17.7) - 0.5) * FLICKER * 0.4 + (level - 0.5) * 0.1;
    let tone = (value.powf(0.75) + wobble).clamp(0.0, 1.0);
    let last = (CHARACTERS.len() - 1) as f32;
    let mut index = (tone * last + 0.5).floor();
    let decode = cell.scramble.max(1.0 - cell.reveal);
    if decode > 0.02 {
        let frame = (time * 20.0).floor();
        if hash(x * 3.7 + frame * 5.3, y * 3.7 + frame * 5.3) < decode * 1.25 {
            index = 1.0 + (hash(x + frame * 1.37, y + frame * 1.37) * last).floor();
        }
    }
    let present = pick < DENSITY * mix(1.0, 0.3 + tone * 1.1, 0.55);
    let alpha = OPACITY * smoothstep(0.03, 0.1, value) * mix(1.0, 0.35 + 0.65 * level, 0.55);
    if !present || alpha < 0.02 || index < 0.5 {
        return None;
    }
    // On dark, a character adds light over the glow under it; on light, it is ink.
    let ink = if dark {
        let base = GLOW;
        let add = |c: f32| (c * color.a * base + c * color.a * 0.55 + 0.03 * color.a).min(1.0);
        Color { r: add(color.r), g: add(color.g), b: add(color.b), a: alpha }
    } else {
        Color { r: visible.r * 0.8, g: visible.g * 0.8, b: visible.b * 0.8, a: alpha }
    };
    Some((CHARACTERS[(index as usize).min(CHARACTERS.len() - 1)], ink))
}

/// A colour rounded to what a span needs, so neighbours of nearly the same colour share one.
fn quantized(color: Color) -> [u8; 4] {
    let channel = |c: f32| (srgb_encode(c) * 31.0).round() as u8;
    [channel(color.r), channel(color.g), channel(color.b), (color.a.clamp(0.0, 1.0) * 8.0).round() as u8]
}

fn unquantized([r, g, b, a]: [u8; 4]) -> Color {
    let channel = |c: u8| srgb_decode(f32::from(c) / 31.0);
    Color { r: channel(r), g: channel(g), b: channel(b), a: f32::from(a) / 8.0 }
}

/// One row of cells as text, its characters coloured by spans.
fn row_text(field: &Field, colors: &[Color], row: usize, dark: bool) -> El {
    let mut line = String::with_capacity(field.columns);
    let mut spans: Vec<Span> = Vec::new();
    let mut run: Option<(usize, [u8; 4])> = None;
    for column in 0..field.columns {
        let cell = field.at(column, row);
        let color = colors[row * field.columns + column];
        let shown = glyph(cell, color, column, row, field.tick, dark);
        line.push(shown.map_or(' ', |(c, _)| char::from(c)));
        let key = shown.map(|(_, ink)| quantized(ink));
        match (run, key) {
            (Some((_, open)), Some(key)) if open == key => {}
            _ => {
                if let Some((start, open)) = run.take() {
                    spans.push(Span::new(start, column).mono().color(unquantized(open)));
                }
                run = key.map(|key| (column, key));
            }
        }
    }
    if let Some((start, open)) = run {
        spans.push(Span::new(start, field.columns).mono().color(unquantized(open)));
    }
    text(line).spans(spans).font(STYLE).color(Color::TRANSPARENT).h(CELL_HEIGHT)
}

/// Streams the glow, one pixel per cell. It runs inside the rows' memo, so once per tick,
/// appearance and grid size.
fn draw_glow(backdrop: &Backdrop, colors: &[Color]) {
    let field = &backdrop.field;
    if field.cells.is_empty() {
        return;
    }
    let mut rgba = Vec::with_capacity(colors.len() * 4);
    for color in colors {
        let encode = |c: f32| (srgb_encode(c) * 255.0).round() as u8;
        rgba.extend_from_slice(&[
            encode(color.r),
            encode(color.g),
            encode(color.b),
            (color.a * GLOW * 255.0).round() as u8,
        ]);
    }
    backdrop.glow.set(field.columns as u32, field.rows as u32, rgba);
}

/// A screen in front of the backdrop, which scrambles where the pointer passes.
pub fn behind(s: &State, front: El, dark: bool, p: &super::theme::Palette) -> El {
    el().key("backdrop-screen")
        .grow(1.0)
        .h_full()
        .fill(p.bg)
        .on_pointer_move_local(|s: &mut State, at, tx| {
            // Under reduced motion the backdrop holds still; a trail would set it moving.
            if !s.reduced_motion.get() {
                s.backdrop.pointer([at.x, at.y]);
            }
            tx.live_only();
        })
        .child(backdrop(s, dark))
        .child(front)
}

/// The backdrop, filling its parent behind whatever comes after it. Under reduced motion it
/// holds still once risen in (the state stops ticking).
fn backdrop(s: &State, dark: bool) -> El {
    let backdrop = &s.backdrop;
    let field = &backdrop.field;
    let rows = memo("backdrop-rows", (field.tick, field.columns, field.rows, dark), || {
        let ramp = ramp(dark);
        let colors: Vec<Color> = field.cells.iter().map(|cell| light(&ramp, *cell)).collect();
        draw_glow(backdrop, &colors);
        let mut rows = col().pin(Some(0.0), Some(0.0), None, None);
        for row in 0..field.rows {
            rows = rows.child(row_text(field, &colors, row, dark));
        }
        rows
    });
    el().key("backdrop")
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .clip_soft(0.0)
        .a11y_hidden()
        .on_layout(|s: &mut State, rect| s.backdrop.resized([rect[2], rect[3]]))
        .child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).pixels(&backdrop.glow))
        .child(rows)
        .child(
            text(MEASURE)
                .font(STYLE)
                .color(Color::TRANSPARENT)
                .pin(Some(0.0), Some(0.0), None, None)
                .on_layout(|s: &mut State, rect| s.backdrop.measured(rect[2] / MEASURE.len() as f32)),
        )
}
