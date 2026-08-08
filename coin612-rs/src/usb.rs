//! Device access: open/claim, command sender, and the frame reader thread.

use crate::events::{Disconnected, NewFrame};
use crate::frame::{deinterleave, FramePair, HEIGHT, PAIR};
use crate::sync::{find_sync, refine_start, uyvy_signature_ok};
use anyhow::{anyhow, Context as _, Result};
use rusb::{Context, DeviceHandle, UsbContext};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const VID: u16 = 0x04B4;
pub const PID: u16 = 0xF7F7;
pub const EP_IN: u8 = 0x81;
pub const EP_OUT: u8 = 0x02;

const READ_TIMEOUT: Duration = Duration::from_millis(1000);
// Same accumulation threshold as the Python viewer: ~4 frames of slack for find_sync.
const SYNC_BYTES: usize = PAIR * 521 * 4;

pub fn open() -> Result<Arc<DeviceHandle<Context>>> {
    let ctx = Context::new().context("libusb init")?;
    let handle = ctx
        .open_device_with_vid_pid(VID, PID)
        .ok_or_else(|| anyhow!("Camera not found (04b4:f7f7)"))?;
    let _ = handle.set_auto_detach_kernel_driver(true);
    handle
        .set_active_configuration(1)
        .context("set_configuration")?;
    handle.claim_interface(0).context("claim_interface")?;
    Ok(Arc::new(handle))
}

/// Camera control packets on the bulk OUT endpoint:
/// {0x55, 0xAA, 0x07, func, page, opt, val_be32, xor(bytes[2..=9]), 0xF0}
pub struct CmdSender {
    pub handle: Arc<DeviceHandle<Context>>,
}

impl CmdSender {
    pub fn send(&self, func: u8, page: u8, opt: u8, val: u32) {
        let mut cmd = [0u8; 12];
        cmd[0] = 0x55;
        cmd[1] = 0xAA;
        cmd[2] = 0x07;
        cmd[3] = func;
        cmd[4] = page;
        cmd[5] = opt;
        cmd[6..10].copy_from_slice(&val.to_be_bytes());
        cmd[10] = cmd[2..10].iter().fold(0, |a, &b| a ^ b);
        cmd[11] = 0xF0;
        if let Err(e) = self.handle.write_bulk(EP_OUT, &cmd, Duration::from_secs(1)) {
            eprintln!("Command write failed: {e}");
        }
    }

    pub fn quick_nuc(&self) {
        self.send(0x02, 0x01, 0x08, 1);
    }

    pub fn shutter(&self, open: bool) {
        self.send(0xA0, 0x02, 0x08, open as u32);
    }

    pub fn low_noise(&self, on: bool) {
        self.send(0x01, 0x00, 0x09, on as u32);
    }
}

pub struct ReaderConfig {
    pub capture_raw: Option<PathBuf>,
}

/// Reader thread: Syncing -> Locked state machine.
///
/// Locked issues one frame-sized bulk read per frame, rounded up to the
/// 512-byte USB packet size, so the read returns on the very packet carrying
/// the frame's last byte. The 0..511 tail bytes belonging to the next frame
/// are carried over in front of the buffer.
pub fn spawn_reader(
    handle: Arc<DeviceHandle<Context>>,
    mut input: triple_buffer::Input<FramePair>,
    sender: sdl2::event::EventSender,
    stop: Arc<AtomicBool>,
    cfg: ReaderConfig,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("usb-reader".into())
        .spawn(move || {
            let mut chunk = vec![0u8; 256 * 1024];
            let mut seq = 0u64;
            let mut capture_raw = cfg.capture_raw;

            'resync: while !stop.load(Ordering::Relaxed) {
                // --- Syncing: accumulate enough stream, then align ---
                let mut buf: Vec<u8> = Vec::with_capacity(SYNC_BYTES + chunk.len());
                while buf.len() < SYNC_BYTES {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    match handle.read_bulk(EP_IN, &mut chunk, READ_TIMEOUT) {
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(rusb::Error::Timeout) => continue,
                        Err(e) => {
                            let _ = sender.push_custom_event(Disconnected {
                                msg: format!("USB read failed: {e}"),
                            });
                            return;
                        }
                    }
                }

                if let Some(path) = capture_raw.take() {
                    match std::fs::write(&path, &buf) {
                        Ok(()) => eprintln!("Captured {} raw bytes to {}", buf.len(), path.display()),
                        Err(e) => eprintln!("Raw capture failed: {e}"),
                    }
                }

                let (coarse, frame_h) = find_sync(&buf);
                let frame_bytes = PAIR * frame_h;
                let offset = refine_start(&buf, coarse, frame_h);
                println!(
                    "Sync: {frame_h} line-pairs, frame={frame_bytes} bytes (origin +{} pairs)",
                    (offset - coarse) / PAIR
                );

                // Drop whole stale frames; keep the partial current frame.
                let leftover = &buf[offset..];
                let partial = leftover.len() % frame_bytes;
                let mut raw_buf = vec![0u8; frame_bytes + 512];
                raw_buf[..partial].copy_from_slice(&leftover[leftover.len() - partial..]);
                let mut filled = partial;

                // --- Locked: frame-boundary-aligned reads ---
                let mut bad_sig = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    while filled < frame_bytes {
                        let read_len = (frame_bytes - filled).div_ceil(512) * 512;
                        match handle.read_bulk(
                            EP_IN,
                            &mut raw_buf[filled..filled + read_len],
                            READ_TIMEOUT,
                        ) {
                            Ok(n) => filled += n,
                            Err(rusb::Error::Timeout) => {
                                eprintln!("Stream stalled, resyncing...");
                                continue 'resync;
                            }
                            Err(e) => {
                                let _ = sender.push_custom_event(Disconnected {
                                    msg: format!("USB read failed: {e}"),
                                });
                                return;
                            }
                        }
                    }
                    let t_read_done = Instant::now();

                    if uyvy_signature_ok(&raw_buf[..frame_bytes]) {
                        bad_sig = 0;
                    } else {
                        bad_sig += 1;
                        if bad_sig >= 3 {
                            eprintln!("Lost stream alignment, resyncing...");
                            continue 'resync;
                        }
                    }

                    let slot = input.input_buffer_mut();
                    deinterleave(&raw_buf[..HEIGHT * PAIR], slot);
                    seq += 1;
                    slot.seq = seq;
                    slot.t_read_done = Some(t_read_done);
                    input.publish();
                    let _ = sender.push_custom_event(NewFrame);

                    // Tail bytes past the boundary open the next frame.
                    raw_buf.copy_within(frame_bytes..filled, 0);
                    filled -= frame_bytes;
                }
            }
        })
        .expect("spawn usb-reader")
}
