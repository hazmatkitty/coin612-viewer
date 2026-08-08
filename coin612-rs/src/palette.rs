//! Pseudo-color palettes as 256-entry RGB LUTs, same order and formulas as the
//! Python viewer's PALETTES dict (keys 1-9 map identically).

use crate::palette_tables;

pub type Lut = [[u8; 3]; 256];

pub struct Palette {
    pub name: &'static str,
    pub lut: Lut,
}

fn gray() -> Lut {
    std::array::from_fn(|i| [i as u8; 3])
}

fn gray_inverted() -> Lut {
    std::array::from_fn(|i| [255 - i as u8; 3])
}

fn ironbow() -> Lut {
    std::array::from_fn(|i| {
        let t = i as f64 / 255.0;
        let (r, g, b) = if t < 0.25 {
            (t / 0.25 * 128.0, 0.0, t / 0.25 * 80.0)
        } else if t < 0.5 {
            let u = (t - 0.25) / 0.25;
            (128.0 + u * 127.0, u * 128.0, 80.0 - u * 80.0)
        } else if t < 0.75 {
            (255.0, 128.0 + (t - 0.5) / 0.25 * 127.0, 0.0)
        } else {
            (255.0, 255.0, (t - 0.75) / 0.25 * 255.0)
        };
        [r as u8, g as u8, b as u8]
    })
}

fn hot_iron() -> Lut {
    std::array::from_fn(|i| {
        let t = i as f64 / 255.0;
        let r = (t * 2.0 * 255.0).min(255.0);
        let g = ((t - 0.4) * 2.5 * 255.0).clamp(0.0, 255.0);
        let b = ((t - 0.7) * 3.3 * 255.0).clamp(0.0, 255.0);
        [r as u8, g as u8, b as u8]
    })
}

fn arctic() -> Lut {
    std::array::from_fn(|i| {
        let t = i as f64 / 255.0;
        let r = ((t - 0.5) * 2.0 * 255.0).clamp(0.0, 255.0);
        let g = (t * 1.5 * 255.0).clamp(0.0, 255.0);
        let b = ((1.0 - t * 0.5) * 255.0).min(255.0);
        [r as u8, g as u8, b as u8]
    })
}

/// OpenCV-style rainbow: hue channel i*180/255 (i.e. i*360/255 degrees), full S/V.
fn rainbow() -> Lut {
    std::array::from_fn(|i| {
        let h_deg = (i * 180 / 255) as f64 * 2.0; // integer truncation matches cv2 u8 math
        hsv_to_rgb(h_deg, 1.0, 1.0)
    })
}

fn hsv_to_rgb(h_deg: f64, s: f64, v: f64) -> [u8; 3] {
    let h = (h_deg % 360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    ]
}

pub fn all() -> Vec<Palette> {
    vec![
        Palette { name: "White Hot", lut: gray() },
        Palette { name: "Black Hot", lut: gray_inverted() },
        Palette { name: "Iron Bow", lut: ironbow() },
        Palette { name: "Hot Iron", lut: hot_iron() },
        Palette { name: "Rainbow", lut: rainbow() },
        Palette { name: "Arctic", lut: arctic() },
        Palette { name: "Jet", lut: palette_tables::JET },
        Palette { name: "Inferno", lut: palette_tables::INFERNO },
        Palette { name: "Turbo", lut: palette_tables::TURBO },
    ]
}

/// The palette used when the palette toggle (P) is off — plain grayscale,
/// matching the Python apply_palette fallback.
pub fn grayscale() -> Lut {
    gray()
}
