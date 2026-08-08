//! MP4 recording via an ffmpeg subprocess fed raw BGRA frames on stdin.
//! A bounded channel + writer thread keeps the render loop from ever blocking
//! on the encoder; frames are dropped (and counted) if it falls behind.

use anyhow::{Context as _, Result};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::time::Instant;

pub struct Recorder {
    tx: Option<SyncSender<Vec<u8>>>,
    writer: Option<std::thread::JoinHandle<()>>,
    child: Child,
    path: PathBuf,
    dropped: u64,
    last_warn: Instant,
}

impl Recorder {
    pub fn start(path: PathBuf, w: usize, h: usize, fps: u32) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-f", "rawvideo", "-pixel_format", "bgra"])
            .arg("-video_size")
            .arg(format!("{w}x{h}"))
            .arg("-framerate")
            .arg(fps.to_string())
            .args(["-i", "-"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23"])
            .args(["-pix_fmt", "yuv420p"])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn()
            .context("ffmpeg not found -- run inside nix-shell")?;
        let mut stdin = child.stdin.take().expect("piped stdin");

        // Sized to absorb ffmpeg's startup delay (~1s of frames at 30fps);
        // steady-state encoding is much faster than the camera.
        let (tx, rx) = sync_channel::<Vec<u8>>(32);
        let writer = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || {
                for frame in rx {
                    if stdin.write_all(&frame).is_err() {
                        break; // ffmpeg died; sender side will notice on stop
                    }
                }
                // stdin drops here -> ffmpeg sees EOF and finalizes the file
            })
            .expect("spawn recorder thread");

        println!("Recording to {}", path.display());
        Ok(Self {
            tx: Some(tx),
            writer: Some(writer),
            child,
            path,
            dropped: 0,
            last_warn: Instant::now(),
        })
    }

    pub fn push(&mut self, frame: Vec<u8>) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                self.dropped += 1;
                if self.last_warn.elapsed().as_secs() >= 1 {
                    eprintln!(
                        "Recorder: encoder falling behind, {} frame(s) dropped",
                        self.dropped
                    );
                    self.last_warn = Instant::now();
                }
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub fn stop(mut self) {
        drop(self.tx.take());
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        match self.child.wait() {
            Ok(status) if status.success() => {
                if self.dropped > 0 {
                    println!(
                        "Saved {} ({} frame(s) dropped)",
                        self.path.display(),
                        self.dropped
                    );
                } else {
                    println!("Saved {}", self.path.display());
                }
            }
            Ok(status) => eprintln!("ffmpeg exited with {status}"),
            Err(e) => eprintln!("ffmpeg wait failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end pipe test: encode 30 synthetic frames, expect a valid
    /// finalized MP4. Skipped when ffmpeg is not installed.
    #[test]
    fn encodes_synthetic_frames() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("ffmpeg missing, skipping");
            return;
        }
        let dir = std::env::temp_dir().join("coin612-rs-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recorder_test.mp4");
        let _ = std::fs::remove_file(&path);

        let (w, h) = (660usize, 512usize);
        let mut rec = Recorder::start(path.clone(), w, h, 30).unwrap();
        for i in 0..30u32 {
            // moving gradient so the encoder gets non-constant input
            let mut frame = vec![0u8; w * h * 4];
            for (p, px) in frame.chunks_exact_mut(4).enumerate() {
                let v = ((p as u32 + i * 640) % 256) as u8;
                px.copy_from_slice(&[v, v, v, 0xFF]);
            }
            rec.push(frame);
        }
        assert_eq!(rec.dropped, 0, "frames dropped during encoder startup");
        rec.stop();

        let meta = std::fs::metadata(&path).expect("mp4 not written");
        assert!(meta.len() > 1000, "mp4 suspiciously small: {} bytes", meta.len());
    }
}
