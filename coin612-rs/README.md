# coin612-rs

Low-latency Rust viewer for the Coin612/"Mars" thermal camera (USB `04b4:f7f7`,
640x512, ~23-30 fps depending on module — the Mars delivers 30).
Port of `../coin612_viewer.py` with a minimal-copy data path:

- one frame-sized, 512-byte-aligned bulk read per frame — the read returns on
  the USB packet carrying the frame's last byte (vs 64 KB chunking in Python)
- latest-wins triple buffer between the USB reader thread and the render loop
- event-driven SDL loop (no `waitKey(1)`-style polling)
- all display transforms (Y16 autoscale, contrast, histogram EQ, palette)
  composed into a single 256-entry LUT, applied in one pass per frame
- vsync off by default so `present()` doesn't wait for scanout

Frame height is auto-detected (the Mars streams 515 line-pairs: 512 active +
3 blanking). One blanking pair per frame is a telemetry line whose chroma
bytes are not the usual 0x80; after coarse sync the frame origin is refined so
the 512 active rows are clean video and the telemetry pair stays in the tail
(`sync::refine_start`). A per-frame signature watchdog re-syncs automatically
if alignment is ever lost. Measured `--latency-debug` on the Mars: ~4 ms
average read→present at 30 fps.

## Build & run

```bash
cd /home/vlad/thermal
nix-shell --run 'cd coin612-rs && cargo build --release'
nix-shell --run 'cd coin612-rs && cargo run --release'
```

Flags:

- `--latency-debug` — print read→present latency stats once per second
- `--capture-raw <file>` — dump the first ~5 MB of raw stream (for the
  `find_sync` fixture test: copy to `tests/fixtures/stream.raw`)

## Controls

| Key | Action |
|---|---|
| Q / ESC | Quit |
| C / 1-9 | Cycle / select palette |
| P | Palette on/off (raw grayscale) |
| H | Histogram equalization |
| T | Toggle Y16 (raw) / UYVY (processed) source |
| F | Fullscreen |
| +/- | Contrast |
| S | Screenshot (PNG + raw Y16 to `../screenshots/`) |
| SPACE | Quick NUC (shutter compensation) |
| N | Extended NUC (better ghost removal) |
| G | Low-noise gain mode |
| V | VSync toggle (off by default for latency) |

The raw Y16 dump is plain bytes (no numpy header):
`np.fromfile("coin612_..._y16.raw", np.uint8).reshape(512, 640)`.
