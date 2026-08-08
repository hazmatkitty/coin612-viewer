//! Overlay drawing straight into the locked ARGB texture buffer:
//! 8x8 bitmap-font text (with drop shadow) and crosshair markers.

use crate::font::FONT8X8;

pub struct Overlay<'a> {
    pub px: &'a mut [u8],
    pub pitch: usize,
    pub w: usize,
    pub h: usize,
}

pub const GREEN: u32 = 0xFF00_FF00;
pub const RED: u32 = 0xFFFF_0000;
pub const BLUE: u32 = 0xFF00_00FF;
pub const GRAY: u32 = 0xFFC8_C8C8;
pub const BLACK: u32 = 0xFF00_0000;

impl<'a> Overlay<'a> {
    fn put(&mut self, x: i32, y: i32, color: u32) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let off = y as usize * self.pitch + x as usize * 4;
        self.px[off..off + 4].copy_from_slice(&color.to_le_bytes());
    }

    fn draw_glyphs(&mut self, x: i32, y: i32, s: &str, color: u32) {
        let mut cx = x;
        for ch in s.chars() {
            let code = ch as usize;
            if (32..127).contains(&code) {
                let glyph = &FONT8X8[code - 32];
                for (gy, bits) in glyph.iter().enumerate() {
                    for gx in 0..8i32 {
                        if bits & (1 << gx) != 0 {
                            self.put(cx + gx, y + gy as i32, color);
                        }
                    }
                }
            }
            cx += 8;
        }
    }

    /// Text with a +1,+1 black drop shadow for readability on live video.
    pub fn text(&mut self, x: i32, y: i32, s: &str, color: u32) {
        self.draw_glyphs(x + 1, y + 1, s, BLACK);
        self.draw_glyphs(x, y, s, color);
    }

    /// Integer-scaled text for banner messages ("Syncing...", disconnect).
    pub fn text_scaled(&mut self, x: i32, y: i32, s: &str, color: u32, scale: i32) {
        let mut cx = x;
        for ch in s.chars() {
            let code = ch as usize;
            if (32..127).contains(&code) {
                let glyph = &FONT8X8[code - 32];
                for (gy, bits) in glyph.iter().enumerate() {
                    for gx in 0..8i32 {
                        if bits & (1 << gx) != 0 {
                            for dy in 0..scale {
                                for dx in 0..scale {
                                    let px = cx + gx * scale + dx;
                                    let py = y + gy as i32 * scale + dy;
                                    self.put(px + 1, py + 1, BLACK);
                                    self.put(px, py, color);
                                }
                            }
                        }
                    }
                }
            }
            cx += 8 * scale;
        }
    }

    /// Cross marker, cv2.drawMarker CROSS style (size = full extent).
    pub fn cross(&mut self, cx: i32, cy: i32, size: i32, color: u32) {
        let half = size / 2;
        for d in -half..=half {
            self.put(cx + d, cy, color);
            self.put(cx, cy + d, color);
        }
    }
}
