# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

Live video viewers for Coin612-family USB thermal cameras (VID `04B4`, PID `F7F7`, 640×512 sensor; the original Coin612 and the "Mars" module both use this ID):

- `coin612-rs/` — **primary viewer**, low-latency Rust/SDL2 port (see its README for design and controls).
- `coin612_viewer.py` — original OpenCV viewer. More recently developed than v2 despite the naming.
- `coin612_viewer_v2.py` — pygame-based variant (no OpenCV dependency).

## Running

```bash
# Rust viewer (primary)
nix-shell --run 'cd coin612-rs && cargo build --release'
nix-shell --run 'cd coin612-rs && cargo run --release'          # flags: --latency-debug, --capture-raw <file>
nix-shell --run 'cd coin612-rs && cargo test --release'         # incl. find_sync fixture test

# Faster iteration on the Rust code (skips the python/opencv part of the shell):
nix-shell -p rustc cargo pkg-config SDL2 libusb1 --run 'cargo build --release'

# Python viewers
nix-shell --run 'python3 coin612_viewer.py'
nix-shell --run 'python3 coin612_viewer_v2.py'
```

Note: system `rustup` has a broken `rustc` proxy on this machine — always build inside nix-shell.

USB access on Linux requires the udev rule in `99-coin612.rules` (install to `/etc/udev/rules.d/`); on Windows, a WinUSB driver via Zadig.

The viewer writes its PID to `.coin612_viewer.pid` and kills any previous instance on startup, so it's safe to just relaunch. On camera disconnect or quit it hard-exits via `os._exit()` (the USB reader thread can otherwise hang the process).

## Architecture

Both viewers share the same pipeline; only the display layer differs:

1. **USB stream** — the camera streams interleaved Y16 + UYVY data on bulk endpoint 0x81. Each frame is 512 active line-pairs + module-dependent blanking (Mars: 515 pairs total, ~30 fps); each pair is one Y16 line (1280 bytes) followed by one UYVY line (1280 bytes). One blanking pair per frame is a telemetry line whose chroma bytes are NOT 0x80.
2. **Sync** (`find_sync`) — the stream has no frame markers. H-sync is found by locating UYVY lines (even bytes ≈ 0x80); frame height is detected by trying 508–520 line-pairs and picking the strongest inter-line-diff boundary peak. Frame height is *detected*, not assumed — don't hardcode it. Beware: the boundary heuristic can place the frame start ON the telemetry pair (it does on the Mars). The Rust viewer corrects this (`sync::refine_start`); the Python viewers display the telemetry line as image row 0.
3. **FrameReader thread** — accumulates bulk reads into a buffer, drops stale frames if more than 2 are queued, and de-interleaves each frame into two images: `latest_y16` (raw thermal, high byte only) and `latest_frame` (camera-processed 8-bit UYVY Y channel). Display loop polls via `get_frame()` under a lock.
4. **Display loop** — palette LUTs, histogram EQ, contrast, min/max markers, OSD, screenshot (`screenshots/`, PNG + raw `.npy` of the Y16 frame) and recording.

**Camera control protocol** — commands go out the OUT endpoint as 12-byte packets: `55 AA 07 <func> <page> <opt> <val:be32> <xor of bytes 2..9> F0` (see `FrameReader.send_command`). Known commands: quick NUC `(0x02, 0x01, 0x08, 1)`, shutter enable/disable `(0xA0, 0x02, 0x08, val)`, low-noise gain `(0x01, 0x00, 0x09, val)`.

When changing frame parsing or protocol behavior, keep the two viewer scripts consistent unless intentionally diverging.
