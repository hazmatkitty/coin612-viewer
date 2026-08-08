//! Screenshot writing off the render thread: PNG of the displayed ARGB frame
//! plus the raw Y16 plane as plain bytes.
//! (Python load: np.fromfile(f, np.uint8).reshape(512, 640))

use std::io::BufWriter;
use std::path::PathBuf;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

const TS_FORMAT: &[FormatItem<'_>] =
    format_description!("[year][month][day]_[hour][minute][second]");

pub fn screenshots_dir() -> PathBuf {
    // Shared with the Python viewers: <repo>/screenshots next to the crate.
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../screenshots"))
}

pub fn timestamp(local_offset: UtcOffset) -> String {
    OffsetDateTime::now_utc()
        .to_offset(local_offset)
        .format(TS_FORMAT)
        .unwrap_or_else(|_| "unknown".into())
}

/// Fire-and-forget save. `argb` is the full displayed frame (video + overlays),
/// tightly packed (pitch == w*4).
pub fn save(local_offset: UtcOffset, argb: Vec<u8>, w: usize, h: usize, y16: Vec<u8>) {
    std::thread::spawn(move || {
        let dir = screenshots_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("Screenshot dir failed: {e}");
            return;
        }
        let ts = timestamp(local_offset);

        let png_path = dir.join(format!("coin612_{ts}.png"));
        let raw_path = dir.join(format!("coin612_{ts}_y16.raw"));

        let mut rgb = Vec::with_capacity(w * h * 3);
        for chunk in argb.chunks_exact(4) {
            // ARGB8888 little-endian in memory: B, G, R, A
            rgb.extend_from_slice(&[chunk[2], chunk[1], chunk[0]]);
        }

        let write_png = || -> anyhow::Result<()> {
            let file = std::fs::File::create(&png_path)?;
            let mut enc = png::Encoder::new(BufWriter::new(file), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&rgb)?;
            Ok(())
        };
        match write_png() {
            Ok(()) => println!("Saved {}", png_path.display()),
            Err(e) => eprintln!("Screenshot PNG failed: {e}"),
        }
        match std::fs::write(&raw_path, &y16) {
            Ok(()) => println!("Saved {}", raw_path.display()),
            Err(e) => eprintln!("Y16 raw dump failed: {e}"),
        }
    });
}
