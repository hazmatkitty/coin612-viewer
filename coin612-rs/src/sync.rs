//! Stream alignment for the marker-less Y16+UYVY stream.
//! Faithful port of `find_sync` in coin612_viewer.py.

use crate::frame::{HEIGHT, LINE, PAIR, WIDTH};

/// Find byte offset and frame height (in line-pairs) for the interleaved stream.
///
/// 1. H-sync: find where UYVY lines start (even bytes == 0x80), sampling 4 frames.
/// 2. Frame height: try 508..=520 line-pairs, pick strongest boundary peak.
/// 3. V-sync: frame boundary from averaged inter-line diffs.
pub fn find_sync(data: &[u8]) -> (usize, usize) {
    // --- H-sync: find line-pair alignment (rough 512-pair frame for sampling) ---
    let rough_frame = PAIR * 512;
    let mut best_score = 0usize;
    let mut best_offset = 0usize;
    for off in (0..PAIR).step_by(2) {
        let mut score = 0usize;
        for frame_no in 0..4 {
            let start = off + LINE + frame_no * rough_frame;
            if start + LINE > data.len() {
                break;
            }
            score += data[start..start + LINE]
                .iter()
                .step_by(2)
                .filter(|&&b| b == 0x80)
                .count();
        }
        if score > best_score {
            best_score = score;
            best_offset = off;
        }
    }

    let aligned = &data[best_offset..];
    let n_pairs = aligned.len() / PAIR;
    if n_pairs < 530 {
        return (best_offset, 512);
    }

    // --- Mean abs diff between consecutive UYVY Y rows ---
    let mut line_diffs = Vec::with_capacity(n_pairs - 1);
    let mut prev = [0u8; WIDTH];
    for i in 0..n_pairs {
        let s = i * PAIR + LINE;
        let mut row = [0u8; WIDTH];
        for (x, dst) in row.iter_mut().enumerate() {
            *dst = aligned[s + 1 + 2 * x];
        }
        if i > 0 {
            let sum: u32 = row
                .iter()
                .zip(prev.iter())
                .map(|(&a, &b)| a.abs_diff(b) as u32)
                .sum();
            line_diffs.push(sum as f64 / WIDTH as f64);
        }
        prev = row;
    }

    // --- Find frame height + V-sync ---
    let mut best_ratio = 0.0f64;
    let mut best_h = 512usize;
    let mut best_v_off = 0usize;

    for candidate_h in 508..=520usize {
        let mut avg = vec![0.0f64; candidate_h];
        let mut cnt = vec![0.0f64; candidate_h];
        for (i, &d) in line_diffs.iter().enumerate() {
            avg[i % candidate_h] += d;
            cnt[i % candidate_h] += 1.0;
        }
        for (a, c) in avg.iter_mut().zip(cnt.iter()) {
            *a /= if *c == 0.0 { 1.0 } else { *c };
        }

        let peak = avg.iter().cloned().fold(f64::MIN, f64::max);
        let mean = avg.iter().sum::<f64>() / candidate_h as f64;
        let ratio = if mean > 0.0 { peak / mean } else { 0.0 };

        if ratio > best_ratio {
            best_ratio = ratio;
            best_h = candidate_h;
            let boundary = avg
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(i, _)| i)
                .unwrap_or(0);
            best_v_off = (boundary + 1) % candidate_h;
        }
    }

    (best_offset + best_v_off * PAIR, best_h)
}

/// Refine the frame origin returned by `find_sync` so that the HEIGHT active
/// rows are all clean UYVY.
///
/// The stream carries one telemetry/blanking line-pair per frame whose chroma
/// bytes are not 0x80, and find_sync's boundary heuristic can land the frame
/// start exactly on it (it does on the Mars: the strongest inter-line diff is
/// the video->telemetry edge). Left there, the telemetry line would be
/// displayed as image row 0, pollute min/max stats, and permanently trip the
/// per-frame signature watchdog. Scan each row's signature across the frames
/// available in the sync buffer and shift the origin to the first window of
/// HEIGHT consecutive clean rows.
pub fn refine_start(data: &[u8], offset: usize, h: usize) -> usize {
    let fb = PAIR * h;
    let frames_avail = (data.len().saturating_sub(offset)) / fb;
    if frames_avail < 2 {
        return offset;
    }
    let n_check = frames_avail.min(3);

    let mut clean = vec![true; h];
    for (r, c) in clean.iter_mut().enumerate() {
        for f in 0..n_check {
            let s = offset + f * fb + r * PAIR;
            if !uyvy_signature_ok(&data[s..s + PAIR]) {
                *c = false;
                break;
            }
        }
    }

    // Rows are periodic with the frame, so the active window may wrap.
    for shift in 0..h {
        if (0..HEIGHT).all(|k| clean[(shift + k) % h]) {
            return offset + shift * PAIR;
        }
    }
    offset // no clean window found; keep the unrefined origin
}

/// Cheap per-frame lock check: the first UYVY line of a well-aligned frame has
/// chroma bytes ~0x80 at even positions. Checks the first 128 chroma samples.
pub fn uyvy_signature_ok(frame: &[u8]) -> bool {
    if frame.len() < LINE + 256 {
        return false;
    }
    let hits = frame[LINE..LINE + 256]
        .iter()
        .step_by(2)
        .filter(|&&b| b == 0x80)
        .count();
    hits * 100 >= 128 * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture test against a real captured stream (produced with --capture-raw).
    /// Skipped when the fixture is absent.
    #[test]
    fn find_sync_on_captured_stream() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stream.raw");
        let Ok(data) = std::fs::read(path) else {
            eprintln!("fixture {path} missing, skipping");
            return;
        };
        let (coarse, h) = find_sync(&data);
        // The Mars streams 515 line-pairs per frame (512 active + 3 blanking,
        // one of which is a telemetry line).
        assert_eq!(h, 515, "unexpected frame height");
        assert!(coarse < data.len());

        let offset = refine_start(&data, coarse, h);
        // Every active row of the first two frames must be clean UYVY --
        // in particular row 0, which the runtime watchdog checks per frame.
        for f in 0..2 {
            for r in 0..HEIGHT {
                let s = offset + f * PAIR * h + r * PAIR;
                assert!(
                    uyvy_signature_ok(&data[s..s + PAIR]),
                    "frame {f} row {r} not clean UYVY"
                );
            }
        }
        // The telemetry pair must exist somewhere in the blanking tail.
        let telemetry = (HEIGHT..h).any(|r| {
            let s = offset + r * PAIR;
            s + PAIR <= data.len() && !uyvy_signature_ok(&data[s..s + PAIR])
        });
        assert!(telemetry, "expected a telemetry pair in the blanking region");
    }

    #[test]
    fn signature_rejects_noise() {
        let noise: Vec<u8> = (0..PAIR * 2).map(|i| (i * 7 + 13) as u8).collect();
        assert!(!uyvy_signature_ok(&noise));
    }

    #[test]
    fn signature_accepts_synthetic_uyvy() {
        let mut frame = vec![0u8; PAIR * 2];
        for (i, b) in frame[LINE..2 * LINE].iter_mut().enumerate() {
            *b = if i % 2 == 0 { 0x80 } else { 0x30 };
        }
        assert!(uyvy_signature_ok(&frame));
    }
}
