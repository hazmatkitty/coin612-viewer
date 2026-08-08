#!/usr/bin/env python3
"""
Coin612 Live Video Viewer
=========================
Displays live infrared video from the Coin612 thermal camera over USB.

The camera streams interleaved Y16 + YUV422 data on USB bulk endpoint 0x81.
Each frame = 513 line-pairs (512 active + 1 blanking), each pair = 1 Y16 line
+ 1 UYVY line, total 1,313,280 bytes/frame at ~23 fps.

Requirements:
  pip install pyusb opencv-python numpy
  WinUSB driver installed via Zadig for VID:04B4 PID:F7F7

Controls:
  ESC / Q     - Quit
  S           - Save screenshot (PNG + raw numpy)
  C           - Cycle pseudo-color palette
  P           - Toggle color palette on/off
  F           - Toggle fullscreen
  H           - Toggle histogram equalization
  R           - Record video start/stop
  SPACE       - Quick NUC (shutter compensation)
  N           - Extended NUC (better ghost removal)
  G           - Toggle Low Noise gain mode
  +/-         - Adjust contrast
  1-9         - Select palette directly
"""

import usb.core
import usb.util
import numpy as np
import cv2
import time
import sys
import os
import subprocess
import threading
from collections import deque

# ── Camera constants ──────────────────────────────────────────────
VID = 0x04B4
PID = 0xF7F7
WIDTH = 640
HEIGHT = 512
LINE = WIDTH * 2          # 1280 bytes per line
PAIR = LINE * 2           # 2560 bytes per Y16+UYVY line pair

# ── Pseudo-color palettes ────────────────────────────────────────
def make_lut_ironbow():
    lut = np.zeros((256, 1, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        if t < 0.25:
            r, g, b = int(t/0.25*128), 0, int(t/0.25*80)
        elif t < 0.5:
            r = 128 + int((t-0.25)/0.25*127)
            g, b = int((t-0.25)/0.25*128), 80-int((t-0.25)/0.25*80)
        elif t < 0.75:
            r, g, b = 255, 128+int((t-0.5)/0.25*127), 0
        else:
            r, g, b = 255, 255, int((t-0.75)/0.25*255)
        lut[i, 0] = [b, g, r]
    return lut

def make_lut_rainbow():
    lut = np.zeros((256, 1, 3), dtype=np.uint8)
    for i in range(256):
        lut[i, 0] = [int(i*180/255), 255, 255]
    return cv2.cvtColor(lut, cv2.COLOR_HSV2BGR)

def make_lut_hot_iron():
    lut = np.zeros((256, 1, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = min(255, int(t*2*255))
        g = max(0, min(255, int((t-0.4)*2.5*255)))
        b = max(0, min(255, int((t-0.7)*3.3*255)))
        lut[i, 0] = [b, g, r]
    return lut

def make_lut_arctic():
    lut = np.zeros((256, 1, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = max(0, min(255, int((t-0.5)*2*255)))
        g = max(0, min(255, int(t*1.5*255)))
        b = min(255, int((1-t*0.5)*255))
        lut[i, 0] = [b, g, r]
    return lut

PALETTES = {
    "White Hot": None,
    "Black Hot": "invert",
    "Iron Bow": make_lut_ironbow(),
    "Hot Iron": make_lut_hot_iron(),
    "Rainbow": make_lut_rainbow(),
    "Arctic": make_lut_arctic(),
    "Jet": cv2.COLORMAP_JET,
    "Inferno": cv2.COLORMAP_INFERNO,
    "Turbo": cv2.COLORMAP_TURBO,
}
PALETTE_NAMES = list(PALETTES.keys())


# ── Frame alignment ──────────────────────────────────────────────

def find_sync(data):
    """
    Find byte offset and frame height for Y16+UYVY interleaved stream.

    1. H-sync: find where UYVY lines start (even bytes = 0x80).
    2. Frame height: try 508-520 line-pairs, pick strongest boundary peak.
    3. V-sync: frame boundary from averaged inter-line diffs.

    Returns (byte_offset, frame_h).
    """
    arr = np.frombuffer(data, dtype=np.uint8)

    # --- H-sync: find line-pair alignment ---
    # Use a rough frame estimate for sampling across frames
    rough_frame = PAIR * 512
    best_score = 0
    best_offset = 0
    for off in range(0, PAIR, 2):
        score = 0
        for fn in range(4):
            start = off + LINE + fn * rough_frame
            if start + LINE > len(arr):
                break
            score += np.sum(arr[start:start + LINE:2] == 0x80)
        if score > best_score:
            best_score = score
            best_offset = off

    # --- Extract UYVY Y channel from all line pairs ---
    aligned = arr[best_offset:]
    n_pairs = len(aligned) // PAIR
    if n_pairs < 530:
        return best_offset, 512

    uyvy_y = np.zeros((n_pairs, WIDTH), dtype=np.uint8)
    for i in range(n_pairs):
        s = i * PAIR + LINE
        if s + LINE > len(aligned):
            break
        uyvy_y[i] = aligned[s + 1:s + LINE:2]

    line_diffs = np.mean(np.abs(
        uyvy_y[1:].astype(float) - uyvy_y[:-1].astype(float)), axis=1)

    # --- Find frame height + V-sync ---
    best_ratio = 0.0
    best_h = 512
    best_v_off = 0

    for candidate_h in range(508, 521):
        avg = np.zeros(candidate_h, dtype=np.float64)
        cnt = np.zeros(candidate_h, dtype=np.float64)
        pos = np.arange(len(line_diffs)) % candidate_h
        np.add.at(avg, pos, line_diffs)
        np.add.at(cnt, pos, 1.0)
        cnt[cnt == 0] = 1
        avg /= cnt

        peak = float(np.max(avg))
        mean = float(np.mean(avg))
        ratio = peak / mean if mean > 0 else 0

        if ratio > best_ratio:
            best_ratio = ratio
            best_h = candidate_h
            boundary = int(np.argmax(avg))
            best_v_off = (boundary + 1) % candidate_h

    return best_offset + best_v_off * PAIR, best_h


# ── Frame reader thread ──────────────────────────────────────────

class FrameReader(threading.Thread):
    """Reads USB data, syncs to interleaved Y16+UYVY frames."""

    def __init__(self, dev):
        super().__init__(daemon=True)
        self.dev = dev
        cfg = dev.get_active_configuration()
        intf = cfg[(0, 0)]
        self.ep_in = [e for e in intf if e.bEndpointAddress == 0x81][0]
        self.ep_out = usb.util.find_descriptor(intf,
            custom_match=lambda e: usb.util.endpoint_direction(e.bEndpointAddress) == usb.util.ENDPOINT_OUT)

        self.running = True
        self.frame_lock = threading.Lock()
        self.latest_frame = None  # UYVY Y channel (processed 8-bit)
        self.latest_y16 = None    # Y16 high byte (raw thermal)
        self.frame_count = 0
        self.fps = 0.0
        self._fps_times = deque(maxlen=30)
        self.synced = False
        self.disconnected = False
        self.frame_h = 512  # detected during sync

    def run(self):
        buf = bytearray()

        while self.running:
            try:
                buf.extend(self.ep_in.read(65536, timeout=500))

                # Phase 1: collect data for sync
                if not self.synced:
                    if len(buf) < PAIR * 521 * 4:
                        continue
                    offset, self.frame_h = find_sync(bytes(buf))
                    self._frame_bytes = PAIR * self.frame_h
                    del buf[:offset]
                    print(f"Sync: {self.frame_h} line-pairs, "
                          f"frame={self._frame_bytes} bytes")
                    self.synced = True
                    continue

                # Phase 2: extract frames
                fb = self._frame_bytes
                while len(buf) >= fb:
                    if len(buf) >= fb * 2:
                        del buf[:fb]
                        continue

                    raw = np.frombuffer(bytes(buf[:fb]), dtype=np.uint8)
                    del buf[:fb]

                    # First 512 active line pairs (skip blanking)
                    active = raw[:HEIGHT * PAIR].reshape(HEIGHT * 2, LINE)
                    y16_img = active[0::2, 0::2]   # even lines, even bytes
                    uyvy_img = active[1::2, 1::2]  # odd lines, odd bytes

                    with self.frame_lock:
                        self.latest_frame = uyvy_img.copy()
                        self.latest_y16 = y16_img.copy()
                        self.frame_count += 1

                    now = time.time()
                    self._fps_times.append(now)
                    if len(self._fps_times) > 1:
                        dt = self._fps_times[-1] - self._fps_times[0]
                        if dt > 0:
                            self.fps = (len(self._fps_times) - 1) / dt

            except usb.core.USBTimeoutError:
                continue
            except (usb.core.USBError, Exception):
                self.running = False
                self.disconnected = True
                def _force_exit():
                    time.sleep(3)
                    os._exit(1)
                threading.Thread(target=_force_exit, daemon=True).start()
                return

    def get_frame(self):
        with self.frame_lock:
            return self.latest_frame, self.latest_y16, self.frame_count, self.fps

    def send_command(self, func, page, opt, val):
        if self.ep_out is None:
            return
        cmd = bytearray([0x55, 0xAA, 0x07, func, page, opt,
                         (val >> 24) & 0xFF, (val >> 16) & 0xFF,
                         (val >> 8) & 0xFF, val & 0xFF, 0, 0xF0])
        xor = 0
        for b in cmd[2:10]:
            xor ^= b
        cmd[10] = xor
        try:
            self.ep_out.write(bytes(cmd), timeout=1000)
        except Exception:
            pass

    def stop(self):
        self.running = False


# ── Zombie cleanup ───────────────────────────────────────────────
LOCK_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                         ".coin612_viewer.pid")

def _kill_previous():
    """Kill any previous viewer instance via PID file."""
    my_pid = os.getpid()
    try:
        with open(LOCK_FILE, "r") as f:
            old_pid = int(f.read().strip())
        if old_pid != my_pid:
            os.kill(old_pid, 9)
            time.sleep(0.5)
    except Exception:
        pass


# ── Main viewer ──────────────────────────────────────────────────

def main():
    _kill_previous()

    try:
        with open(LOCK_FILE, "w") as f:
            f.write(str(os.getpid()))
    except Exception:
        pass

    # Connect
    print("Connecting to Coin612...")
    dev = usb.core.find(idVendor=VID, idProduct=PID)
    if dev is None:
        print("Camera not found.")
        sys.exit(1)
    try:
        dev.set_configuration()
    except usb.core.USBError as e:
        print(f"Cannot open device: {e}")
        sys.exit(1)

    print(f"Connected: VID:{VID:04X} PID:{PID:04X}")

    reader = FrameReader(dev)
    reader.start()

    # Viewer state
    palette_idx = 0
    use_palette = True
    histogram_eq = False
    fullscreen = False
    recording = False
    video_writer = None
    contrast_gain = 1.0
    show_y16 = False  # toggle between UYVY (processed) and Y16 (raw)
    screenshot_dir = os.path.join(os.path.dirname(__file__), "screenshots")

    window_name = "Coin612 Thermal Viewer"
    cv2.namedWindow(window_name, cv2.WINDOW_NORMAL | cv2.WINDOW_GUI_NORMAL)
    cv2.resizeWindow(window_name, WIDTH, HEIGHT)

    print("Controls: Q=Quit C=Palette P=Raw H=HEQ F=Full S=Screenshot R=Record")
    print("NUC: SPACE=Quick N=Extended G=LowNoise  T=Toggle Y16/UYVY  +/-=Contrast")
    print(">>> Click the VIDEO WINDOW for keyboard controls <<<")

    last_count = 0
    display_color = None

    def apply_palette(gray, idx):
        pname = PALETTE_NAMES[idx]
        pval = PALETTES[pname]
        if not use_palette or pval is None:
            return cv2.cvtColor(gray, cv2.COLOR_GRAY2BGR)
        elif isinstance(pval, str):
            return cv2.cvtColor(255 - gray, cv2.COLOR_GRAY2BGR)
        elif isinstance(pval, int):
            return cv2.applyColorMap(gray, pval)
        else:
            b = cv2.LUT(gray, pval[:, 0, 0])
            g = cv2.LUT(gray, pval[:, 0, 1])
            r = cv2.LUT(gray, pval[:, 0, 2])
            return cv2.merge([b, g, r])

    bar_w = 20
    bar_gray = np.flipud(np.linspace(0, 255, HEIGHT, dtype=np.uint8).reshape(-1, 1))
    bar_gray = np.tile(bar_gray, (1, bar_w))
    cached_bar_idx = -1
    cached_bar_color = None

    # Placeholder
    ph = np.zeros((HEIGHT, WIDTH + bar_w, 3), dtype=np.uint8)
    cv2.putText(ph, "Syncing...", (250, 260),
                cv2.FONT_HERSHEY_SIMPLEX, 0.8, (0, 255, 0), 2)
    cv2.imshow(window_name, ph)

    try:
        while True:
            try:
                if cv2.getWindowProperty(window_name, cv2.WND_PROP_VISIBLE) < 1:
                    break
            except cv2.error:
                break

            if reader.disconnected:
                msg = np.zeros((HEIGHT, WIDTH + bar_w, 3), dtype=np.uint8)
                cv2.putText(msg, "Camera disconnected", (160, 256),
                            cv2.FONT_HERSHEY_SIMPLEX, 0.9, (0, 0, 255), 2)
                cv2.imshow(window_name, msg)
                cv2.waitKey(2000)
                break

            frame_uyvy, frame_y16, count, fps = reader.get_frame()

            if frame_uyvy is not None and count != last_count:
                last_count = count

                # Choose source: processed UYVY or raw Y16
                if show_y16 and frame_y16 is not None:
                    raw = frame_y16
                    # Auto-scale Y16 high bytes to 0-255
                    mn, mx = int(raw.min()), int(raw.max())
                    if mx > mn:
                        display = ((raw.astype(float) - mn) * 255 / (mx - mn)).astype(np.uint8)
                    else:
                        display = raw
                    src_name = "Y16"
                else:
                    display = frame_uyvy
                    raw = frame_uyvy
                    src_name = "UYVY"

                if contrast_gain != 1.0:
                    display = np.clip(display.astype(np.float32) * contrast_gain,
                                      0, 255).astype(np.uint8)
                if histogram_eq:
                    display = cv2.equalizeHist(display)

                display_color = apply_palette(display, palette_idx)

                # Min/max markers
                min_val, max_val, min_loc, max_loc = cv2.minMaxLoc(raw)
                cv2.drawMarker(display_color, max_loc, (0, 0, 255),
                               cv2.MARKER_CROSS, 15, 1)
                cv2.drawMarker(display_color, min_loc, (255, 0, 0),
                               cv2.MARKER_CROSS, 15, 1)

                # OSD
                pname = PALETTE_NAMES[palette_idx]
                info = f"{fps:.0f}fps {src_name} {pname}"
                if histogram_eq: info += " HEQ"
                info2 = (f"H:{max_val:.0f}({max_loc[0]},{max_loc[1]}) "
                         f"C:{min_val:.0f}({min_loc[0]},{min_loc[1]})")
                for i, t in enumerate([info, info2]):
                    y = 18 + i * 18
                    cv2.putText(display_color, t, (4, y),
                                cv2.FONT_HERSHEY_SIMPLEX, 0.4, (0,0,0), 2)
                    cv2.putText(display_color, t, (4, y),
                                cv2.FONT_HERSHEY_SIMPLEX, 0.4, (0,255,0), 1)

                # Red REC indicator
                if recording:
                    cv2.putText(display_color, "REC", (WIDTH - 50, 18),
                                cv2.FONT_HERSHEY_SIMPLEX, 0.5, (0,0,0), 3)
                    cv2.putText(display_color, "REC", (WIDTH - 50, 18),
                                cv2.FONT_HERSHEY_SIMPLEX, 0.5, (0,0,255), 2)

                # Controls legend at bottom
                legend = [
                    "Q:Quit  C:Palette  P:Raw  H:HEQ  F:Full  T:Y16/UYVY  +/-:Contrast",
                    "S:Screenshot  R:Record  SPACE:NUC  N:ExtNUC  G:LowNoise  1-9:Palette",
                ]
                for i, t in enumerate(legend):
                    y = HEIGHT - 22 + i * 14
                    cv2.putText(display_color, t, (4, y),
                                cv2.FONT_HERSHEY_SIMPLEX, 0.32, (0,0,0), 2)
                    cv2.putText(display_color, t, (4, y),
                                cv2.FONT_HERSHEY_SIMPLEX, 0.32, (200,200,200), 1)

                # Color bar
                if cached_bar_idx != palette_idx or cached_bar_color is None:
                    cached_bar_color = apply_palette(bar_gray, palette_idx)
                    cached_bar_idx = palette_idx
                display_color = np.hstack([display_color, cached_bar_color])

                if recording and video_writer is not None:
                    video_writer.write(display_color)

                cv2.imshow(window_name, display_color)

            # Key handling
            key = cv2.waitKey(1) & 0xFF
            if key == 255:
                continue
            elif key in (27, ord('q'), ord('Q')):
                break
            elif key in (ord('c'), ord('C')):
                palette_idx = (palette_idx + 1) % len(PALETTE_NAMES)
            elif key in (ord('p'), ord('P')):
                use_palette = not use_palette
            elif key in (ord('h'), ord('H')):
                histogram_eq = not histogram_eq
            elif key in (ord('t'), ord('T')):
                show_y16 = not show_y16
            elif key in (ord('f'), ord('F')):
                fullscreen = not fullscreen
                cv2.setWindowProperty(window_name, cv2.WND_PROP_FULLSCREEN,
                                      cv2.WINDOW_FULLSCREEN if fullscreen else cv2.WINDOW_NORMAL)
            elif key in (ord('s'), ord('S')):
                if display_color is not None:
                    os.makedirs(screenshot_dir, exist_ok=True)
                    ts = time.strftime("%Y%m%d_%H%M%S")
                    cv2.imwrite(os.path.join(screenshot_dir, f"coin612_{ts}.png"),
                                display_color)
                    if frame_y16 is not None:
                        np.save(os.path.join(screenshot_dir, f"coin612_{ts}_y16.npy"),
                                frame_y16)
            elif key in (ord('r'), ord('R')):
                if not recording:
                    os.makedirs(screenshot_dir, exist_ok=True)
                    ts = time.strftime("%Y%m%d_%H%M%S")
                    fourcc = cv2.VideoWriter_fourcc(*'mp4v')
                    video_writer = cv2.VideoWriter(
                        os.path.join(screenshot_dir, f"coin612_{ts}.mp4"),
                        fourcc, 23.0, (WIDTH + bar_w, HEIGHT))
                    recording = True
                else:
                    recording = False
                    if video_writer:
                        video_writer.release()
                        video_writer = None
            elif key == ord(' '):
                reader.send_command(0x02, 0x01, 0x08, 1)
            elif key in (ord('n'), ord('N')):
                def _extended_nuc():
                    reader.send_command(0xA0, 0x02, 0x08, 0)
                    time.sleep(0.5)
                    reader.send_command(0x02, 0x01, 0x08, 1)
                    time.sleep(0.15)
                    reader.send_command(0xA0, 0x02, 0x08, 1)
                threading.Thread(target=_extended_nuc, daemon=True).start()
            elif key in (ord('g'), ord('G')):
                if not hasattr(main, '_low_noise'):
                    main._low_noise = False
                main._low_noise = not main._low_noise
                reader.send_command(0x01, 0x00, 0x09, 1 if main._low_noise else 0)
            elif key in (ord('+'), ord('=')):
                contrast_gain = min(5.0, contrast_gain + 0.1)
            elif key in (ord('-'), ord('_')):
                contrast_gain = max(0.1, contrast_gain - 0.1)
            elif ord('1') <= key < ord('1') + len(PALETTE_NAMES):
                palette_idx = key - ord('1')

    except KeyboardInterrupt:
        pass
    finally:
        reader.stop()
        reader.join(timeout=1)
        if video_writer:
            video_writer.release()
        cv2.destroyAllWindows()
        try:
            usb.util.dispose_resources(dev)
        except Exception:
            pass
        os._exit(0)


if __name__ == "__main__":
    main()
