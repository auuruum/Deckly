//! The Deckly logo: a key grid that shifts from blue to violet. Drawn in
//! code so the window icon needs no image files; `assets/logo/*.svg` holds
//! the same design as vector art.

use eframe::egui::IconData;

/// The design is laid out on a 120×120 canvas, like the SVG files.
const CANVAS: f32 = 120.0;
const TILE_RADIUS: f32 = 26.0;
const TILE: [u8; 3] = [0x1A, 0x1A, 0x1D];
const FROM: [u8; 3] = [0x3D, 0x8B, 0xFD];
const TO: [u8; 3] = [0x8B, 0x5C, 0xF6];
/// Below this size the 3×3 grid blurs, so a 2×2 grid is drawn instead.
const SMALL: u32 = 24;

pub fn icon(size: u32) -> IconData {
    let size = size.clamp(8, 1024);
    IconData {
        rgba: rgba(size),
        width: size,
        height: size,
    }
}

/// Straight (non-premultiplied) RGBA pixels, `size`×`size`.
fn rgba(size: u32) -> Vec<u8> {
    let unit = CANVAS / size as f32;
    let cells = cells(size);
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let (px, py) = ((x as f32 + 0.5) * unit, (y as f32 + 0.5) * unit);
            let alpha = coverage(px, py, 0.0, 0.0, CANVAS, TILE_RADIUS, unit);
            let mut rgb = TILE.map(f32::from);
            for cell in &cells {
                let c = coverage(px, py, cell.x, cell.y, cell.size, cell.radius, unit);
                for (channel, target) in rgb.iter_mut().zip(cell.color) {
                    *channel += (f32::from(target) - *channel) * c;
                }
            }
            out.extend(rgb.map(|v| v.round() as u8));
            out.push((alpha * 255.0).round() as u8);
        }
    }
    out
}

struct Cell {
    x: f32,
    y: f32,
    size: f32,
    radius: f32,
    color: [u8; 3],
}

fn cells(size: u32) -> Vec<Cell> {
    let (n, cell, gap) = if size < SMALL {
        (2u8, 32.0, 8.0)
    } else {
        (3u8, 22.0, 6.0)
    };
    let span = f32::from(n) * cell + f32::from(n - 1) * gap;
    let offset = (CANVAS - span) / 2.0;
    let steps = f32::from(2 * (n - 1));
    let mut cells = Vec::new();
    for row in 0..n {
        for col in 0..n {
            cells.push(Cell {
                x: offset + f32::from(col) * (cell + gap),
                y: offset + f32::from(row) * (cell + gap),
                size: cell,
                radius: cell * 0.23,
                color: mix(FROM, TO, f32::from(row + col) / steps),
            });
        }
    }
    cells
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let mut out = a;
    for (o, (a, b)) in out.iter_mut().zip(a.into_iter().zip(b)) {
        *o = (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    }
    out
}

/// How much of the pixel centred at (`px`, `py`) a rounded square covers,
/// from its signed distance; `unit` is the pixel size in canvas units.
fn coverage(px: f32, py: f32, x: f32, y: f32, side: f32, radius: f32, unit: f32) -> f32 {
    let half = side / 2.0;
    let qx = (px - x - half).abs() - half + radius;
    let qy = (py - y - half).abs() - half + radius;
    let distance = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
    (0.5 - distance / unit).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(icon: &IconData, x: u32, y: u32) -> &[u8] {
        let i = ((y * icon.width + x) * 4) as usize;
        &icon.rgba[i..i + 4]
    }

    #[test]
    fn icon_size_is_clamped_and_buffer_matches() {
        for (asked, size) in [(0, 8), (16, 16), (256, 256), (u32::MAX, 1024)] {
            let icon = icon(asked);
            assert_eq!((icon.width, icon.height), (size, size));
            assert_eq!(icon.rgba.len(), (size * size * 4) as usize);
        }
    }

    #[test]
    fn rounded_corners_are_transparent() {
        let icon = icon(128);
        assert_eq!(pixel(&icon, 0, 0)[3], 0);
        assert_eq!(pixel(&icon, 127, 127)[3], 0);
        assert_eq!(pixel(&icon, 64, 2)[3], 255);
    }

    #[test]
    fn grid_runs_from_blue_to_violet() {
        let icon = icon(120);
        assert_eq!(pixel(&icon, 32, 32), [0x3D, 0x8B, 0xFD, 255]);
        assert_eq!(pixel(&icon, 88, 88), [0x8B, 0x5C, 0xF6, 255]);
        assert_eq!(pixel(&icon, 46, 46), [0x1A, 0x1A, 0x1D, 255]);
    }

    #[test]
    fn colours_match_the_svg() {
        let colours: Vec<_> = cells(120).iter().map(|c| c.color).collect();
        assert_eq!(colours[1], [0x51, 0x7F, 0xFB]);
        assert_eq!(colours[4], [0x64, 0x74, 0xFA]);
        assert_eq!(colours[5], [0x78, 0x68, 0xF8]);
        assert_eq!(cells(16).len(), 4);
    }
}
