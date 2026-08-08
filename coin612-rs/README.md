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

### Windows

Builds natively with the MSVC toolchain (`rustup default stable-msvc`).
SDL2 is compiled from source and statically linked on Windows (the `bundled` +
`static-link` cargo features), so besides Rust you need:

- **CMake** and **MSVC Build Tools** (C compiler) — for the SDL2 and libusb
  source builds
- **Zadig** — bind the WinUSB driver to the camera (`04B4:F7F7`) once;
  without it libusb cannot open the device
- **ffmpeg** on `PATH` — optional, only for the R (record) key

```powershell
cd coin612-rs
cargo run --release
```

#### Cross-compiling the .exe from Linux

Produces a self-contained `target/x86_64-pc-windows-gnu/release/coin612-rs.exe`
(SDL2 and libusb statically linked; only Windows system DLLs imported). Zadig
on the target machine is still required.

```bash
# Isolated rustup — the system rustup install is broken on this machine.
# Persistent cache location so the toolchain survives across sessions:
export RUSTUP_HOME=~/.cache/coin612-win/rustup CARGO_HOME=~/.cache/coin612-win/cargo
nix-shell -p rustup cmake nasm pkgsCross.mingwW64.stdenv.cc pkgsCross.mingwW64.windows.pthreads --run '
  rustup toolchain install stable --profile minimal
  rustup target add x86_64-pc-windows-gnu
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
  export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
  export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++
  export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
  export CMAKE_POLICY_VERSION_MINIMUM=3.5     # vendored SDL2 vs CMake 4
  export CFLAGS_x86_64_pc_windows_gnu=-std=gnu17  # vendored SDL2 vs GCC 15 C23 default
  cargo build --release --target x86_64-pc-windows-gnu
'
```

When run outside the repo (e.g. the copied .exe), screenshots and recordings
land in a `screenshots/` folder next to the executable.

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
| R | Record H.264 MP4 of the displayed view (via ffmpeg subprocess) |
| SPACE | Quick NUC (shutter compensation) |
| N | Extended NUC (better ghost removal) |
| G | Low-noise gain mode |
| V | VSync toggle (off by default for latency) |

The raw Y16 dump is plain bytes (no numpy header):
`np.fromfile("coin612_..._y16.raw", np.uint8).reshape(512, 640)`.
