# Coin612 thermal camera viewers

Live video viewers for Coin612-family USB thermal cameras — the original
Coin612 and the "Mars" module, both enumerating as USB `04B4:F7F7` with a
640×512 sensor (~23–30 fps depending on module).

These cameras stream raw interleaved Y16 + UYVY video over USB bulk transfer
with no frame markers and no standard driver. The viewers here do the sync,
de-interleaving, and display themselves, plus camera control (NUC/shutter
calibration, gain modes) over the vendor command protocol.

## What's in here

| Path | What it is |
|---|---|
| `coin612-rs/` | **Primary viewer** — low-latency Rust/SDL2 port (~4 ms read→present). See [its README](coin612-rs/README.md) for design notes, controls, and Windows builds. |
| `coin612_viewer.py` | Original OpenCV viewer, most feature-complete of the Python two. |
| `coin612_viewer_v2.py` | pygame variant (no OpenCV dependency). |
| `99-coin612.rules` | udev rule for USB access on Linux. |
| `shell.nix` | Dev environment (Rust, SDL2, libusb, Python/OpenCV, ffmpeg). |
| `screenshots/` | Where all viewers drop screenshots (PNG + raw Y16 dumps) and recordings. |

## Quick start (Linux)

One-time USB permission setup:

```bash
sudo cp 99-coin612.rules /etc/udev/rules.d/
sudo udevadm control --reload && sudo udevadm trigger
```

Then plug in the camera and run the Rust viewer:

```bash
nix-shell --run 'cd coin612-rs && cargo run --release'
```

Or one of the Python viewers:

```bash
nix-shell --run 'python3 coin612_viewer.py'
```

Key controls (full list in [coin612-rs/README.md](coin612-rs/README.md)):
palettes (`C`/`1`–`9`), histogram EQ (`H`), raw-vs-processed source (`T`),
screenshot (`S`), record MP4 (`R`), quick NUC (`SPACE`), quit (`Q`).

## Windows

The Rust viewer runs on Windows as a single self-contained `coin612-rs.exe`
(SDL2 and libusb statically linked). Either build it natively with the MSVC
toolchain or cross-compile it from Linux — both recipes are in
[coin612-rs/README.md](coin612-rs/README.md#windows).

On the Windows machine itself you only need:

1. **Zadig** (once): bind the WinUSB driver to the device `04B4:F7F7`.
2. **ffmpeg** on `PATH` — optional, only for MP4 recording.

Screenshots and recordings land in a `screenshots/` folder next to the exe.

## How it works (short version)

The camera streams line pairs on bulk endpoint 0x81: one Y16 line (raw
16-bit thermal) followed by one UYVY line (camera-processed 8-bit video),
1280 bytes each. A frame is 512 active pairs plus a few module-dependent
blanking pairs, one of which carries telemetry.

Since the stream has no frame markers, the viewers find alignment
statistically: UYVY lines are recognizable by their chroma bytes (≈ `0x80`),
and the frame boundary is picked by testing candidate heights (508–520 pairs)
for the strongest inter-line discontinuity. Frame height is *detected*, not
assumed. A per-frame signature watchdog re-syncs automatically if alignment
is lost.

Camera commands (NUC, shutter enable, gain mode) are 12-byte packets on the
OUT endpoint: `55 AA 07 <func> <page> <opt> <val:be32> <xor> F0`.

Deeper details: architecture notes in [CLAUDE.md](CLAUDE.md), Rust design
notes in [coin612-rs/README.md](coin612-rs/README.md).
