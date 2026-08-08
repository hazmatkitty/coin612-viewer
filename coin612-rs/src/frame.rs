pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 512;
pub const LINE: usize = WIDTH * 2; // 1280 bytes per line
pub const PAIR: usize = LINE * 2; // 2560 bytes per Y16+UYVY line pair
pub const PIXELS: usize = WIDTH * HEIGHT;

/// One de-interleaved frame: the Y16 plane (even lines, even bytes) and the
/// camera-processed UYVY Y channel (odd lines, odd bytes).
#[derive(Clone)]
pub struct FramePair {
    pub y16: Vec<u8>,
    pub uyvy: Vec<u8>,
    pub seq: u64,
    pub t_read_done: Option<std::time::Instant>,
}

impl Default for FramePair {
    fn default() -> Self {
        Self {
            y16: vec![0; PIXELS],
            uyvy: vec![0; PIXELS],
            seq: 0,
            t_read_done: None,
        }
    }
}

/// Split the first HEIGHT line-pairs of a raw frame into the two planes.
/// Port of the numpy slicing: y16 = active[0::2, 0::2], uyvy = active[1::2, 1::2].
pub fn deinterleave(raw: &[u8], out: &mut FramePair) {
    debug_assert!(raw.len() >= HEIGHT * PAIR);
    for row in 0..HEIGHT {
        let y16_src = &raw[row * PAIR..row * PAIR + LINE];
        let uyvy_src = &raw[row * PAIR + LINE..row * PAIR + 2 * LINE];
        let y16_dst = &mut out.y16[row * WIDTH..(row + 1) * WIDTH];
        let uyvy_dst = &mut out.uyvy[row * WIDTH..(row + 1) * WIDTH];
        for x in 0..WIDTH {
            y16_dst[x] = y16_src[2 * x];
            uyvy_dst[x] = uyvy_src[2 * x + 1];
        }
    }
}
