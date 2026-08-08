#!/usr/bin/env python3
"""
Coin612 Live Video Viewer (v2 — pygame, no OpenCV)
====================================================
Displays live infrared video from the Coin612 thermal camera over USB.

Requirements:
  nix-shell (see shell.nix) or: pip install pyusb pygame numpy

Controls:
  ESC / Q     - Quit
  S           - Save screenshot (PNG + raw numpy)
  C           - Cycle pseudo-color palette
  P           - Toggle color palette on/off
  F           - Toggle fullscreen
  H           - Toggle histogram equalization
  R           - Record video start/stop (raw .npy sequence)
  SPACE       - Quick NUC (shutter compensation)
  N           - Extended NUC (better ghost removal)
  G           - Toggle Low Noise gain mode
  +/-         - Adjust contrast
  1-9         - Select palette directly
  T           - Toggle Y16/UYVY source
"""

import warnings
warnings.filterwarnings("ignore", message=".*avx2.*")

import usb.core
import usb.util
import numpy as np
import pygame
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
LINE = WIDTH * 2
PAIR = LINE * 2

# ── Pseudo-color palettes (all produce (256, 3) uint8 LUTs) ──────

def _make_lut_ironbow():
    lut = np.zeros((256, 3), dtype=np.uint8)
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
        lut[i] = [r, g, b]
    return lut

def _make_lut_rainbow():
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        h = i * 180.0 / 255.0 / 360.0
        # HSV(h, 1, 1) -> RGB
        import colorsys
        r, g, b = colorsys.hsv_to_rgb(h, 1.0, 1.0)
        lut[i] = [int(r*255), int(g*255), int(b*255)]
    return lut

def _make_lut_hot_iron():
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = min(255, int(t*2*255))
        g = max(0, min(255, int((t-0.4)*2.5*255)))
        b = max(0, min(255, int((t-0.7)*3.3*255)))
        lut[i] = [r, g, b]
    return lut

def _make_lut_arctic():
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = max(0, min(255, int((t-0.5)*2*255)))
        g = max(0, min(255, int(t*1.5*255)))
        b = min(255, int((1-t*0.5)*255))
        lut[i] = [r, g, b]
    return lut

def _make_lut_jet():
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = min(255, max(0, int(255 * min(1, max(0, 1.5 - abs(t - 0.75) * 4)))))
        g = min(255, max(0, int(255 * min(1, max(0, 1.5 - abs(t - 0.5) * 4)))))
        b = min(255, max(0, int(255 * min(1, max(0, 1.5 - abs(t - 0.25) * 4)))))
        lut[i] = [r, g, b]
    return lut

def _make_lut_inferno():
    # Approximate matplotlib inferno
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = min(255, max(0, int(255 * (1.56*t - 0.20*t**2 - 0.01) if t > 0.1 else t*3)))
        g = min(255, max(0, int(255 * max(0, (t - 0.35) * 2.0))))
        b = min(255, max(0, int(255 * (0.5 * (1 + np.sin(np.pi * (t * 0.85 + 0.15)))
              if t < 0.65 else max(0, 1 - (t - 0.65) * 2.5)))))
        lut[i] = [r, g, b]
    return lut

def _make_lut_turbo():
    lut = np.zeros((256, 3), dtype=np.uint8)
    for i in range(256):
        t = i / 255.0
        r = min(255, max(0, int(255 * (0.13572 + t*(4.61539 + t*(-42.66032 + t*(132.13108 + t*(-152.54800 + t*59.28637))))))))
        g = min(255, max(0, int(255 * (0.09140 + t*(2.26344 + t*(-14.85307 + t*(43.42938 + t*(-48.14816 + t*17.65510))))))))
        b = min(255, max(0, int(255 * (0.10667 + t*(12.64194 + t*(-60.58204 + t*(132.00258 + t*(-135.45389 + t*51.26106))))))))
        lut[i] = [r, g, b]
    return lut

PALETTES = {
    "White Hot": None,
    "Black Hot": "invert",
    "Iron Bow": _make_lut_ironbow(),
    "Hot Iron": _make_lut_hot_iron(),
    "Rainbow": _make_lut_rainbow(),
    "Arctic": _make_lut_arctic(),
    "Jet": _make_lut_jet(),
    "Inferno": _make_lut_inferno(),
    "Turbo": _make_lut_turbo(),
}
PALETTE_NAMES = list(PALETTES.keys())


# ── Histogram equalization (pure numpy) ──────────────────────────

def equalize_hist(img):
    hist, _ = np.histogram(img.flatten(), 256, [0, 256])
    cdf = hist.cumsum()
    cdf_min = cdf[cdf > 0].min()
    total = img.size
    lut = ((cdf - cdf_min) * 255 / (total - cdf_min)).clip(0, 255).astype(np.uint8)
    return lut[img]


# ── Apply palette ────────────────────────────────────────────────

def apply_palette(gray, idx, use_pal=True):
    """gray: (H,W) uint8 -> (H,W,3) uint8 RGB"""
    pname = PALETTE_NAMES[idx]
    pval = PALETTES[pname]
    if not use_pal or pval is None:
        return np.stack([gray, gray, gray], axis=-1)
    elif isinstance(pval, str):  # "invert"
        inv = 255 - gray
        return np.stack([inv, inv, inv], axis=-1)
    else:
        # pval is (256, 3) LUT
        return pval[gray]


# ── Frame alignment ──────────────────────────────────────────────

def find_sync(data):
    arr = np.frombuffer(data, dtype=np.uint8)
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
        self.latest_frame = None
        self.latest_y16 = None
        self.frame_count = 0
        self.fps = 0.0
        self._fps_times = deque(maxlen=30)
        self.synced = False
        self.disconnected = False
        self.frame_h = 512

    def run(self):
        buf = bytearray()
        while self.running:
            try:
                buf.extend(self.ep_in.read(65536, timeout=500))
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

                fb = self._frame_bytes
                while len(buf) >= fb:
                    if len(buf) >= fb * 2:
                        del buf[:fb]
                        continue
                    raw = np.frombuffer(bytes(buf[:fb]), dtype=np.uint8)
                    del buf[:fb]
                    active = raw[:HEIGHT * PAIR].reshape(HEIGHT * 2, LINE)
                    y16_img = active[0::2, 0::2]
                    uyvy_img = active[1::2, 1::2]
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

    # Pygame init
    BAR_W = 20
    WIN_W = WIDTH + BAR_W
    WIN_H = HEIGHT
    pygame.init()
    screen = pygame.display.set_mode((WIN_W, WIN_H), pygame.RESIZABLE)
    pygame.display.set_caption("Coin612 Thermal Viewer")
    clock = pygame.time.Clock()
    font = pygame.font.SysFont("monospace", 14)

    # State
    palette_idx = 0
    use_palette = True
    histogram_eq = False
    fullscreen = False
    recording = False
    ffmpeg_proc = None
    contrast_gain = 1.0
    show_y16 = False
    screenshot_dir = os.path.join(os.path.dirname(__file__), "screenshots")
    low_noise = False

    # Pre-build color bar
    bar_gray = np.flipud(np.linspace(0, 255, HEIGHT, dtype=np.uint8).reshape(-1, 1))
    bar_gray = np.tile(bar_gray, (1, BAR_W))
    cached_bar_idx = -1
    cached_bar_surf = None

    last_count = 0

    def draw_text(surf, text, x, y, color=(0, 255, 0), shadow=True):
        if shadow:
            s = font.render(text, True, (0, 0, 0))
            surf.blit(s, (x+1, y+1))
        t = font.render(text, True, color)
        surf.blit(t, (x, y))

    def draw_cross(surf, pos, color, size=7):
        x, y = pos
        pygame.draw.line(surf, color, (x - size, y), (x + size, y), 1)
        pygame.draw.line(surf, color, (x, y - size), (x, y + size), 1)

    print("Controls: Q=Quit C=Palette P=Raw H=HEQ F=Full S=Screenshot R=Record")
    print("NUC: SPACE=Quick N=Extended G=LowNoise  T=Toggle Y16/UYVY  +/-=Contrast")

    running = True
    try:
        while running:
            for event in pygame.event.get():
                if event.type == pygame.QUIT:
                    running = False
                elif event.type == pygame.KEYDOWN:
                    k = event.key
                    if k in (pygame.K_ESCAPE, pygame.K_q):
                        running = False
                    elif k == pygame.K_c:
                        palette_idx = (palette_idx + 1) % len(PALETTE_NAMES)
                    elif k == pygame.K_p:
                        use_palette = not use_palette
                    elif k == pygame.K_h:
                        histogram_eq = not histogram_eq
                    elif k == pygame.K_t:
                        show_y16 = not show_y16
                    elif k == pygame.K_f:
                        fullscreen = not fullscreen
                        if fullscreen:
                            screen = pygame.display.set_mode((0, 0), pygame.FULLSCREEN)
                        else:
                            screen = pygame.display.set_mode((WIN_W, WIN_H), pygame.RESIZABLE)
                    elif k == pygame.K_s:
                        os.makedirs(screenshot_dir, exist_ok=True)
                        ts = time.strftime("%Y%m%d_%H%M%S")
                        pygame.image.save(screen, os.path.join(screenshot_dir, f"coin612_{ts}.png"))
                        frame_uyvy, frame_y16, _, _ = reader.get_frame()
                        if frame_y16 is not None:
                            np.save(os.path.join(screenshot_dir, f"coin612_{ts}_y16.npy"), frame_y16)
                        print(f"Screenshot saved: coin612_{ts}.png")
                    elif k == pygame.K_r:
                        if not recording:
                            os.makedirs(screenshot_dir, exist_ok=True)
                            ts = time.strftime("%Y%m%d_%H%M%S")
                            rec_path = os.path.join(screenshot_dir, f"coin612_{ts}.mp4")
                            ffmpeg_proc = subprocess.Popen([
                                "ffmpeg", "-y",
                                "-f", "rawvideo",
                                "-pix_fmt", "rgb24",
                                "-s", f"{WIN_W}x{WIN_H}",
                                "-r", "23",
                                "-i", "-",
                                "-c:v", "libx264",
                                "-preset", "fast",
                                "-crf", "18",
                                "-pix_fmt", "yuv420p",
                                rec_path,
                            ], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            recording = True
                            print(f"Recording to {rec_path}...")
                        else:
                            recording = False
                            if ffmpeg_proc:
                                ffmpeg_proc.stdin.close()
                                ffmpeg_proc.wait()
                                ffmpeg_proc = None
                                print("Recording saved.")
                    elif k == pygame.K_SPACE:
                        reader.send_command(0x02, 0x01, 0x08, 1)
                    elif k == pygame.K_n:
                        def _extended_nuc():
                            reader.send_command(0xA0, 0x02, 0x08, 0)
                            time.sleep(0.5)
                            reader.send_command(0x02, 0x01, 0x08, 1)
                            time.sleep(0.15)
                            reader.send_command(0xA0, 0x02, 0x08, 1)
                        threading.Thread(target=_extended_nuc, daemon=True).start()
                    elif k == pygame.K_g:
                        low_noise = not low_noise
                        reader.send_command(0x01, 0x00, 0x09, 1 if low_noise else 0)
                    elif k in (pygame.K_PLUS, pygame.K_EQUALS, pygame.K_KP_PLUS):
                        contrast_gain = min(5.0, contrast_gain + 0.1)
                    elif k in (pygame.K_MINUS, pygame.K_KP_MINUS):
                        contrast_gain = max(0.1, contrast_gain - 0.1)
                    elif pygame.K_1 <= k <= pygame.K_9:
                        idx = k - pygame.K_1
                        if idx < len(PALETTE_NAMES):
                            palette_idx = idx

            if reader.disconnected:
                screen.fill((0, 0, 0))
                draw_text(screen, "Camera disconnected", 200, 256, (255, 0, 0))
                pygame.display.flip()
                pygame.time.wait(2000)
                break

            frame_uyvy, frame_y16, count, fps = reader.get_frame()

            if frame_uyvy is not None and count != last_count:
                last_count = count

                if show_y16 and frame_y16 is not None:
                    raw = frame_y16
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
                    display = equalize_hist(display)

                # Apply palette -> (H, W, 3) RGB
                color_img = apply_palette(display, palette_idx, use_palette)

                # Min/max markers
                flat_min = int(np.argmin(raw))
                flat_max = int(np.argmax(raw))
                min_loc = (flat_min % WIDTH, flat_min // WIDTH)
                max_loc = (flat_max % WIDTH, flat_max // WIDTH)
                min_val = int(raw.flat[flat_min])
                max_val = int(raw.flat[flat_max])

                # Color bar
                if cached_bar_idx != palette_idx or cached_bar_surf is None:
                    bar_rgb = apply_palette(bar_gray, palette_idx, use_palette)
                    cached_bar_surf = pygame.surfarray.make_surface(
                        np.ascontiguousarray(bar_rgb.transpose(1, 0, 2)))
                    cached_bar_idx = palette_idx

                # Build surface
                surf = pygame.surfarray.make_surface(
                    np.ascontiguousarray(color_img.transpose(1, 0, 2)))

                # Draw markers on surface
                draw_cross(surf, max_loc, (255, 0, 0))
                draw_cross(surf, min_loc, (0, 0, 255))

                # OSD - top
                pname = PALETTE_NAMES[palette_idx]
                info = f"{fps:.0f}fps {src_name} {pname}"
                if histogram_eq: info += " HEQ"
                info2 = (f"H:{max_val}({max_loc[0]},{max_loc[1]}) "
                         f"C:{min_val}({min_loc[0]},{min_loc[1]})")
                draw_text(surf, info, 4, 4)
                draw_text(surf, info2, 4, 20)
                if recording:
                    draw_text(surf, "REC", WIDTH - 40, 4, (255, 0, 0))

                # OSD - controls legend at bottom
                legend = [
                    "Q:Quit  C:Palette  P:Raw  H:HEQ  F:Full  T:Y16/UYVY  +/-:Contrast",
                    "S:Screenshot  R:Record  SPACE:NUC  N:ExtNUC  G:LowNoise  1-9:Palette",
                ]
                for i, line in enumerate(legend):
                    draw_text(surf, line, 4, HEIGHT - 32 + i * 16, (200, 200, 200))

                # Compose final frame
                frame_surf = pygame.Surface((WIN_W, WIN_H))
                frame_surf.blit(surf, (0, 0))
                frame_surf.blit(cached_bar_surf, (WIDTH, 0))

                # Scale to window
                win_size = screen.get_size()
                if win_size != (WIN_W, WIN_H):
                    scaled = pygame.transform.scale(frame_surf, win_size)
                    screen.blit(scaled, (0, 0))
                else:
                    screen.blit(frame_surf, (0, 0))

                pygame.display.flip()

                if recording and ffmpeg_proc:
                    try:
                        # Get the composed frame as RGB bytes
                        rgb = pygame.surfarray.array3d(frame_surf)  # (W, H, 3)
                        rgb = rgb.transpose(1, 0, 2)  # (H, W, 3)
                        ffmpeg_proc.stdin.write(rgb.tobytes())
                    except BrokenPipeError:
                        recording = False
                        ffmpeg_proc = None
            else:
                # No new frame, don't burn CPU
                clock.tick(60)

    except KeyboardInterrupt:
        pass
    finally:
        reader.stop()
        reader.join(timeout=1)
        if ffmpeg_proc:
            ffmpeg_proc.stdin.close()
            ffmpeg_proc.wait()
        pygame.quit()
        try:
            usb.util.dispose_resources(dev)
        except Exception:
            pass
        os._exit(0)


if __name__ == "__main__":
    main()
