//! Coin612/Mars thermal camera live viewer.
//!
//! Low-latency Rust port of coin612_viewer.py: frame-boundary-aligned USB bulk
//! reads, a latest-wins triple buffer, an event-driven (non-polling) SDL loop,
//! and a single composed LUT pass per frame.
//!
//! Controls: Q/ESC quit, C cycle palette, P palette on/off, 1-9 palette,
//! H histogram EQ, T Y16/UYVY source, F fullscreen, +/- contrast,
//! S screenshot, SPACE quick NUC, N extended NUC, G low-noise gain, V vsync.

mod events;
mod font;
mod frame;
mod osd;
mod palette;
mod palette_tables;
mod recorder;
mod render;
mod screenshot;
mod sync;
mod usb;

use anyhow::{anyhow, Result};
use events::{Disconnected, NewFrame};
use frame::{FramePair, HEIGHT, WIDTH};
use osd::Overlay;
use render::{compose_lut, compute_stats, LutParams, Stats};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::BlendMode;
use sdl2::video::FullscreenType;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BAR_W: usize = 20;
const WIN_W: u32 = (WIDTH + BAR_W) as u32;
const WIN_H: u32 = HEIGHT as u32;

struct ViewerState {
    palette_idx: usize,
    use_palette: bool,
    histeq: bool,
    fullscreen: bool,
    show_y16: bool,
    contrast: f32,
    low_noise: bool,
    vsync: bool,
}

fn main() -> Result<()> {
    let mut capture_raw: Option<PathBuf> = None;
    let mut latency_debug = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--capture-raw" => {
                capture_raw = Some(PathBuf::from(
                    args.next().ok_or_else(|| anyhow!("--capture-raw needs a path"))?,
                ))
            }
            "--latency-debug" => latency_debug = true,
            other => return Err(anyhow!("Unknown argument: {other}")),
        }
    }

    // Must run before any thread exists (time crate soundness rule).
    let local_offset =
        time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);

    println!("Connecting to Coin612...");
    let handle = usb::open()?;
    println!("Connected: VID:{:04X} PID:{:04X}", usb::VID, usb::PID);
    let cmd = usb::CmdSender { handle: handle.clone() };

    let sdl = sdl2::init().map_err(|e| anyhow!(e))?;
    let video = sdl.video().map_err(|e| anyhow!(e))?;
    let ev = sdl.event().map_err(|e| anyhow!(e))?;
    ev.register_custom_event::<NewFrame>().map_err(|e| anyhow!(e))?;
    ev.register_custom_event::<Disconnected>().map_err(|e| anyhow!(e))?;

    let window = video
        .window("Coin612 Thermal Viewer (rs)", WIN_W, WIN_H)
        .position_centered()
        .resizable()
        .build()?;
    // Accelerated, vsync OFF: present() returns immediately.
    let mut canvas = window.into_canvas().accelerated().build()?;
    canvas.set_logical_size(WIN_W, WIN_H)?;
    canvas.set_blend_mode(BlendMode::None);
    let texture_creator = canvas.texture_creator();
    let mut tex = texture_creator.create_texture_streaming(
        PixelFormatEnum::ARGB8888,
        WIDTH as u32,
        HEIGHT as u32,
    )?;
    let mut bar_tex = texture_creator.create_texture_streaming(
        PixelFormatEnum::ARGB8888,
        BAR_W as u32,
        HEIGHT as u32,
    )?;

    let video_rect = Rect::new(0, 0, WIDTH as u32, HEIGHT as u32);
    let bar_rect = Rect::new(WIDTH as i32, 0, BAR_W as u32, HEIGHT as u32);

    // Placeholder while the reader syncs.
    tex.with_lock(None, |px, pitch| {
        px.fill(0);
        let mut ov = Overlay { px, pitch, w: WIDTH, h: HEIGHT };
        ov.text_scaled(250, 250, "Syncing...", osd::GREEN, 2);
    })
    .map_err(|e| anyhow!(e))?;
    canvas.clear();
    canvas.copy(&tex, None, Some(video_rect)).map_err(|e| anyhow!(e))?;
    canvas.present();

    let stop = Arc::new(AtomicBool::new(false));
    let (input, mut output) = triple_buffer::TripleBuffer::new(&FramePair::default()).split();
    let reader = usb::spawn_reader(
        handle.clone(),
        input,
        ev.event_sender(),
        stop.clone(),
        usb::ReaderConfig { capture_raw },
    );

    let palettes = palette::all();
    let grayscale = palette::grayscale();
    let mut state = ViewerState {
        palette_idx: 0,
        use_palette: true,
        histeq: false,
        fullscreen: false,
        show_y16: false,
        contrast: 1.0,
        low_noise: false,
        vsync: false,
    };

    println!("Controls: Q=Quit C=Palette P=Raw H=HEQ F=Full S=Screenshot R=Record");
    println!("NUC: SPACE=Quick N=Extended G=LowNoise  T=Y16/UYVY  +/-=Contrast V=VSync");

    let mut event_pump = sdl.event_pump().map_err(|e| anyhow!(e))?;
    let mut last_seq = 0u64;
    let mut bar_dirty = true;
    let mut fps_times: VecDeque<Instant> = VecDeque::with_capacity(30);
    let mut latencies: Vec<Duration> = Vec::new();
    let mut last_lat_print = Instant::now();
    let mut disconnected: Option<String> = None;
    // Persists until the next rendered frame so a keypress between frames isn't lost.
    let mut want_screenshot = false;
    let mut recorder: Option<recorder::Recorder> = None;

    'running: loop {
        let mut new_frame = false;
        let mut pending: Vec<Event> = Vec::new();
        if let Some(e) = event_pump.wait_event_timeout(100) {
            pending.push(e);
        }
        pending.extend(event_pump.poll_iter());

        for e in pending {
            if e.as_user_event_type::<NewFrame>().is_some() {
                new_frame = true;
                continue;
            }
            if let Some(d) = e.as_user_event_type::<Disconnected>() {
                disconnected = Some(d.msg);
                continue;
            }
            match e {
                Event::Quit { .. } => break 'running,
                Event::KeyDown { keycode: Some(k), .. } => match k {
                    Keycode::Escape | Keycode::Q => break 'running,
                    Keycode::C => {
                        state.palette_idx = (state.palette_idx + 1) % palettes.len();
                        bar_dirty = true;
                    }
                    Keycode::P => {
                        state.use_palette = !state.use_palette;
                        bar_dirty = true;
                    }
                    Keycode::H => state.histeq = !state.histeq,
                    Keycode::T => state.show_y16 = !state.show_y16,
                    Keycode::F => {
                        state.fullscreen = !state.fullscreen;
                        let mode = if state.fullscreen {
                            FullscreenType::Desktop
                        } else {
                            FullscreenType::Off
                        };
                        if let Err(e) = canvas.window_mut().set_fullscreen(mode) {
                            eprintln!("Fullscreen failed: {e}");
                        }
                    }
                    Keycode::V => {
                        state.vsync = !state.vsync;
                        // SDL_RenderSetVSync (SDL >= 2.0.18); not wrapped by rust-sdl2.
                        let rc = unsafe {
                            sdl2::sys::SDL_RenderSetVSync(canvas.raw(), state.vsync as i32)
                        };
                        if rc != 0 {
                            eprintln!("VSync toggle unsupported by renderer");
                            state.vsync = !state.vsync;
                        } else {
                            println!("VSync: {}", if state.vsync { "on" } else { "off" });
                        }
                    }
                    Keycode::S => want_screenshot = true,
                    Keycode::R => match recorder.take() {
                        Some(r) => r.stop(),
                        None => {
                            let fps_est = if fps_times.len() > 1 {
                                let d = (*fps_times.back().unwrap()
                                    - *fps_times.front().unwrap())
                                .as_secs_f64();
                                if d > 0.0 {
                                    ((fps_times.len() - 1) as f64 / d).round() as u32
                                } else {
                                    30
                                }
                            } else {
                                30
                            }
                            .clamp(1, 120);
                            let dir = screenshot::screenshots_dir();
                            if let Err(e) = std::fs::create_dir_all(&dir) {
                                eprintln!("Cannot create {}: {e}", dir.display());
                            } else {
                                let path = dir.join(format!(
                                    "coin612_{}.mp4",
                                    screenshot::timestamp(local_offset)
                                ));
                                match recorder::Recorder::start(
                                    path,
                                    WIDTH + BAR_W,
                                    HEIGHT,
                                    fps_est,
                                ) {
                                    Ok(r) => recorder = Some(r),
                                    Err(e) => eprintln!("Recording failed: {e:#}"),
                                }
                            }
                        }
                    },
                    Keycode::Space => cmd.quick_nuc(),
                    Keycode::N => {
                        let h = handle.clone();
                        std::thread::spawn(move || {
                            let cmd = usb::CmdSender { handle: h };
                            cmd.shutter(false);
                            std::thread::sleep(Duration::from_millis(500));
                            cmd.quick_nuc();
                            std::thread::sleep(Duration::from_millis(150));
                            cmd.shutter(true);
                        });
                    }
                    Keycode::G => {
                        state.low_noise = !state.low_noise;
                        cmd.low_noise(state.low_noise);
                    }
                    Keycode::Plus | Keycode::Equals | Keycode::KpPlus => {
                        state.contrast = (state.contrast + 0.1).min(5.0);
                    }
                    Keycode::Minus | Keycode::Underscore | Keycode::KpMinus => {
                        state.contrast = (state.contrast - 0.1).max(0.1);
                    }
                    _ => {
                        let num = k.into_i32() - Keycode::Num1.into_i32();
                        if (0..palettes.len() as i32).contains(&num) {
                            state.palette_idx = num as usize;
                            bar_dirty = true;
                        }
                    }
                },
                _ => {}
            }
        }

        if let Some(msg) = disconnected.take() {
            eprintln!("{msg}");
            tex.with_lock(None, |px, pitch| {
                px.fill(0);
                let mut ov = Overlay { px, pitch, w: WIDTH, h: HEIGHT };
                ov.text_scaled(160, 250, "Camera disconnected", osd::RED, 2);
            })
            .map_err(|e| anyhow!(e))?;
            canvas.clear();
            canvas.copy(&tex, None, Some(video_rect)).map_err(|e| anyhow!(e))?;
            canvas.present();
            std::thread::sleep(Duration::from_secs(2));
            break 'running;
        }

        if !new_frame {
            continue;
        }
        let fp = output.read();
        if fp.seq == last_seq {
            continue;
        }
        last_seq = fp.seq;

        let now = Instant::now();
        if fps_times.len() == 30 {
            fps_times.pop_front();
        }
        fps_times.push_back(now);
        let fps = if fps_times.len() > 1 {
            (fps_times.len() - 1) as f64
                / (now - *fps_times.front().unwrap()).as_secs_f64()
        } else {
            0.0
        };

        let plane = if state.show_y16 { &fp.y16 } else { &fp.uyvy };
        let src_name = if state.show_y16 { "Y16" } else { "UYVY" };
        let stats = compute_stats(plane, WIDTH);
        let params = LutParams {
            autoscale: state.show_y16,
            contrast: state.contrast,
            histeq: state.histeq,
        };
        let pal = if state.use_palette {
            &palettes[state.palette_idx].lut
        } else {
            &grayscale
        };
        let lut = compose_lut(&stats, &params, pal);

        let recording = recorder.is_some();
        tex.with_lock(None, |px, pitch| {
            render::blit(plane, &lut, px, pitch, WIDTH);
            let mut ov = Overlay { px, pitch, w: WIDTH, h: HEIGHT };
            draw_overlays(&mut ov, &stats, &state, &palettes, fps, src_name, recording);
        })
        .map_err(|e| anyhow!(e))?;

        if bar_dirty {
            bar_tex
                .with_lock(None, |px, pitch| {
                    render::fill_color_bar(pal, px, pitch, BAR_W, HEIGHT);
                })
                .map_err(|e| anyhow!(e))?;
            bar_dirty = false;
        }

        canvas.clear();
        canvas.copy(&tex, None, Some(video_rect)).map_err(|e| anyhow!(e))?;
        canvas.copy(&bar_tex, None, Some(bar_rect)).map_err(|e| anyhow!(e))?;
        canvas.present();

        if latency_debug {
            if let Some(t0) = fp.t_read_done {
                latencies.push(t0.elapsed());
            }
            if last_lat_print.elapsed() > Duration::from_secs(1) && !latencies.is_empty() {
                let n = latencies.len() as f64;
                let avg = latencies.iter().sum::<Duration>().as_secs_f64() * 1000.0 / n;
                let max = latencies.iter().max().unwrap().as_secs_f64() * 1000.0;
                println!("latency read->present: avg {avg:.2} ms, max {max:.2} ms, n={n}");
                latencies.clear();
                last_lat_print = Instant::now();
            }
        }

        if let Some(rec) = recorder.as_mut() {
            let frame =
                compose_display_frame(plane, &lut, &stats, &state, &palettes, pal, fps, src_name, true);
            rec.push(frame);
        }

        if want_screenshot {
            let frame =
                compose_display_frame(plane, &lut, &stats, &state, &palettes, pal, fps, src_name, recording);
            screenshot::save(local_offset, frame, WIDTH + BAR_W, HEIGHT, fp.y16.clone());
            want_screenshot = false;
        }
    }

    if let Some(r) = recorder.take() {
        r.stop();
    }
    stop.store(true, Ordering::Relaxed);
    // Reader notices the stop flag within one read timeout.
    let _ = reader.join();
    Ok(())
}

/// Re-render video + overlays + color bar into a tightly packed ARGB buffer
/// (used for screenshots and as the recording frame source).
#[allow(clippy::too_many_arguments)]
fn compose_display_frame(
    plane: &[u8],
    lut: &[u32; 256],
    stats: &Stats,
    state: &ViewerState,
    palettes: &[palette::Palette],
    pal: &palette::Lut,
    fps: f64,
    src_name: &str,
    recording: bool,
) -> Vec<u8> {
    let full_w = WIDTH + BAR_W;
    let mut buf = vec![0u8; full_w * HEIGHT * 4];
    render::blit(plane, lut, &mut buf, full_w * 4, WIDTH);
    let mut ov = Overlay { px: &mut buf, pitch: full_w * 4, w: WIDTH, h: HEIGHT };
    draw_overlays(&mut ov, stats, state, palettes, fps, src_name, recording);
    render::fill_color_bar_at(pal, &mut buf, full_w * 4, WIDTH, BAR_W, HEIGHT);
    buf
}

#[allow(clippy::too_many_arguments)]
fn draw_overlays(
    ov: &mut Overlay,
    stats: &Stats,
    state: &ViewerState,
    palettes: &[palette::Palette],
    fps: f64,
    src_name: &str,
    recording: bool,
) {
    ov.cross(stats.max_loc.0 as i32, stats.max_loc.1 as i32, 15, osd::RED);
    ov.cross(stats.min_loc.0 as i32, stats.min_loc.1 as i32, 15, osd::BLUE);

    let pname = palettes[state.palette_idx].name;
    let mut info = format!("{fps:.0}fps {src_name} {pname}");
    if state.histeq {
        info.push_str(" HEQ");
    }
    let info2 = format!(
        "H:{}({},{}) C:{}({},{})",
        stats.max, stats.max_loc.0, stats.max_loc.1, stats.min, stats.min_loc.0, stats.min_loc.1
    );
    ov.text(4, 6, &info, osd::GREEN);
    ov.text(4, 18, &info2, osd::GREEN);
    if recording {
        ov.text((WIDTH - 40) as i32, 6, "REC", osd::RED);
    }

    let legend = [
        "Q:Quit C:Palette P:Raw H:HEQ F:Full T:Y16/UYVY +/-:Contrast",
        "S:Screenshot R:Record SPACE:NUC N:ExtNUC G:LowNoise V:VSync 1-9:Palette",
    ];
    for (i, line) in legend.iter().enumerate() {
        ov.text(4, (HEIGHT - 22 + i * 11) as i32, line, osd::GRAY);
    }
}
