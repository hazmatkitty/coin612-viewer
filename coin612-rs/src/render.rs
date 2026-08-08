//! Per-frame processing: one stats pass over the source plane, then every
//! display transform (Y16 autoscale, contrast, histogram EQ, palette) composed
//! into a single 256-entry ARGB LUT applied in one pixel pass.

use crate::frame::PIXELS;
use crate::palette::Lut;

pub struct Stats {
    pub min: u8,
    pub max: u8,
    pub min_loc: (usize, usize),
    pub max_loc: (usize, usize),
    pub hist: [u32; 256],
}

/// Single pass: histogram + min/max values and first-occurrence locations
/// (row-major scan, matching cv2.minMaxLoc).
pub fn compute_stats(plane: &[u8], width: usize) -> Stats {
    let mut hist = [0u32; 256];
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    let mut min_i = 0usize;
    let mut max_i = 0usize;
    for (i, &v) in plane.iter().enumerate() {
        hist[v as usize] += 1;
        if v < min {
            min = v;
            min_i = i;
        }
        if v > max {
            max = v;
            max_i = i;
        }
    }
    Stats {
        min,
        max,
        min_loc: (min_i % width, min_i / width),
        max_loc: (max_i % width, max_i / width),
        hist,
    }
}

pub struct LutParams {
    /// Autoscale source min/max to 0..255 (Y16 mode).
    pub autoscale: bool,
    pub contrast: f32,
    pub histeq: bool,
}

/// Compose the full display transform into one ARGB8888 LUT.
pub fn compose_lut(stats: &Stats, params: &LutParams, palette: &Lut) -> [u32; 256] {
    // Stage 1: value mapping (autoscale then contrast), u8 -> u8.
    let mut map = [0u8; 256];
    let (mn, mx) = (stats.min as f32, stats.max as f32);
    for (i, m) in map.iter_mut().enumerate() {
        let mut v = i as f32;
        if params.autoscale && mx > mn {
            v = (v - mn) * 255.0 / (mx - mn);
        }
        v *= params.contrast;
        *m = v.clamp(0.0, 255.0) as u8;
    }

    // Stage 2: histogram equalization over the mapped histogram
    // (cv2.equalizeHist formula: scale CDF excluding the first non-zero bin).
    if params.histeq {
        let mut mapped_hist = [0u64; 256];
        for (i, &c) in stats.hist.iter().enumerate() {
            mapped_hist[map[i] as usize] += c as u64;
        }
        let total: u64 = mapped_hist.iter().sum();
        let mut cdf = [0u64; 256];
        let mut acc = 0u64;
        for (i, &c) in mapped_hist.iter().enumerate() {
            acc += c;
            cdf[i] = acc;
        }
        let cdf_min = cdf.iter().copied().find(|&c| c > 0).unwrap_or(0);
        let denom = total.saturating_sub(cdf_min);
        let mut eq = [0u8; 256];
        if denom > 0 {
            for (i, e) in eq.iter_mut().enumerate() {
                *e = (((cdf[i] - cdf_min.min(cdf[i])) as f64 / denom as f64) * 255.0).round()
                    as u8;
            }
            for m in map.iter_mut() {
                *m = eq[*m as usize];
            }
        }
    }

    // Stage 3: palette to packed ARGB.
    let mut lut = [0u32; 256];
    for (i, l) in lut.iter_mut().enumerate() {
        let [r, g, b] = palette[map[i] as usize];
        *l = 0xFF00_0000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
    }
    lut
}

/// Apply the LUT over the source plane into a locked ARGB8888 texture buffer.
pub fn blit(plane: &[u8], lut: &[u32; 256], px: &mut [u8], pitch: usize, width: usize) {
    debug_assert_eq!(plane.len(), PIXELS);
    for (row_idx, row) in plane.chunks_exact(width).enumerate() {
        let dst = &mut px[row_idx * pitch..row_idx * pitch + width * 4];
        for (x, &v) in row.iter().enumerate() {
            dst[x * 4..x * 4 + 4].copy_from_slice(&lut[v as usize].to_le_bytes());
        }
    }
}

/// Fill the color-bar texture: vertical gradient, 255 at top, palette only
/// (no contrast/histeq — matches the Python viewer's cached bar).
pub fn fill_color_bar(palette: &Lut, px: &mut [u8], pitch: usize, width: usize, height: usize) {
    fill_color_bar_at(palette, px, pitch, 0, width, height);
}

/// Same, but drawn at a horizontal pixel offset inside a wider buffer
/// (used to compose the screenshot image).
pub fn fill_color_bar_at(
    palette: &Lut,
    px: &mut [u8],
    pitch: usize,
    x_off: usize,
    width: usize,
    height: usize,
) {
    for y in 0..height {
        let v = 255 - (y * 255 / (height - 1));
        let [r, g, b] = palette[v];
        let argb = 0xFF00_0000u32 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
        let dst = &mut px[y * pitch + x_off * 4..y * pitch + (x_off + width) * 4];
        for x in 0..width {
            dst[x * 4..x * 4 + 4].copy_from_slice(&argb.to_le_bytes());
        }
    }
}
