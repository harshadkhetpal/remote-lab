//! Host: captures the primary (or selected) monitor and streams JPEG frames over WebSocket.
//! Receives input messages and injects them with Enigo (requires accessibility permissions on macOS).

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use futures_util::{SinkExt, StreamExt};
use image::codecs::jpeg::JpegEncoder;
use image::ExtendedColorType;
use remote_lab::{InputMessage, MouseButton, FRAME_MAGIC};
use std::io::Cursor;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

const VIEWER_HTML: &str = include_str!("../../web/viewer.html");

static INPUT_TX: OnceLock<std::sync::mpsc::Sender<InputMessage>> = OnceLock::new();

#[derive(Parser, Debug)]
#[command(name = "remote-host")]
struct Args {
    /// Address to listen on, e.g. 0.0.0.0:9753
    #[arg(long, default_value = "0.0.0.0:9753")]
    bind: String,

    /// Monitor index (from `remote-host --list-monitors` if we add it; 0 = first)
    #[arg(long, default_value_t = 0)]
    monitor: usize,

    /// Target FPS (approximate)
    #[arg(long, default_value_t = 12)]
    fps: u32,

    /// JPEG quality 1–100
    #[arg(long, default_value_t = 60)]
    jpeg_quality: u8,

    /// Max output width in pixels (frames are downscaled to fit). 0 = no limit.
    #[arg(long, default_value_t = 1280)]
    max_width: u32,

    /// Print detected monitors and exit
    #[arg(long)]
    list_monitors: bool,
}

static CAP_W: AtomicU32 = AtomicU32::new(1);
static CAP_H: AtomicU32 = AtomicU32::new(1);

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.list_monitors {
        let monitors = xcap::Monitor::all().map_err(|e| anyhow!("monitors: {e}"))?;
        for (i, m) in monitors.iter().enumerate() {
            match (m.width(), m.height()) {
                (Ok(w), Ok(h)) => println!("{i}: {w} x {h}"),
                _ => println!("{i}: (could not read dimensions)"),
            }
        }
        return Ok(());
    }

    let (tx, rx) = std::sync::mpsc::channel::<InputMessage>();
    INPUT_TX
        .set(tx)
        .map_err(|_| anyhow!("input channel already initialized"))?;
    std::thread::Builder::new()
        .name("input-injector".into())
        .spawn(move || input_thread(rx))?;

    let listener = TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("bind {}", args.bind))?;
    println!("remote-host listening on http://{}  (open this URL on your phone)", args.bind);
    println!("Native viewer: cargo run --bin remote-viewer -- --url ws://<host-ip>:PORT/");
    println!("macOS: grant Accessibility + Screen Recording to Terminal/IDE running this binary.");

    while let Ok((stream, addr)) = listener.accept().await {
        println!("client connecting: {addr}");
        let args = Args {
            bind: args.bind.clone(),
            monitor: args.monitor,
            fps: args.fps,
            jpeg_quality: args.jpeg_quality,
            max_width: args.max_width,
            list_monitors: false,
        };
        tokio::spawn(async move {
            if let Err(e) = dispatch(stream, args).await {
                eprintln!("session ended ({addr}): {e:#}");
            }
        });
    }
    Ok(())
}

async fn dispatch(stream: TcpStream, args: Args) -> Result<()> {
    let mut peek = [0u8; 1024];
    let n = stream.peek(&mut peek).await.unwrap_or(0);
    let head = std::str::from_utf8(&peek[..n]).unwrap_or("");

    let is_ws = head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("upgrade:") && l.to_ascii_lowercase().contains("websocket"));

    if is_ws {
        handle_ws_client(stream, args).await
    } else {
        serve_http(stream).await
    }
}

async fn serve_http(mut stream: TcpStream) -> Result<()> {
    let body = VIEWER_HTML.as_bytes();
    let response = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_ws_client(stream: TcpStream, args: Args) -> Result<()> {
    let ws = accept_async(stream).await?;
    let (mut write, mut read) = ws.split();

    {
        let monitors = xcap::Monitor::all().map_err(|e| anyhow!("monitors: {e}"))?;
        if monitors.get(args.monitor).is_none() {
            return Err(anyhow!("monitor index {} not found", args.monitor));
        }
    }
    let monitor_index = args.monitor;

    // Pump incoming input messages on a separate task so the frame loop
    // never blocks waiting for them.
    let input_task = tokio::spawn(async move {
        while let Some(msg) = read.next().await {
            let Ok(msg) = msg else { break };
            match msg {
                Message::Text(t) => {
                    if let Ok(input) = serde_json::from_str::<InputMessage>(&t) {
                        if let Some(tx) = INPUT_TX.get() {
                            let _ = tx.send(input);
                        }
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    let frame_interval = tokio::time::Duration::from_millis((1000 / args.fps.max(1)) as u64);
    let max_w = args.max_width;
    let quality = args.jpeg_quality;

    let result: Result<()> = async {
        loop {
            let frame_started = tokio::time::Instant::now();

            // Capture + encode on a blocking thread so the async runtime stays free.
            let payload = tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>> {
                let monitors = xcap::Monitor::all().map_err(|e| anyhow!("monitors: {e}"))?;
                let Some(mon) = monitors.get(monitor_index) else {
                    return Ok(None);
                };
                let rgba = match mon.capture_image() {
                    Ok(img) => img,
                    Err(e) => {
                        eprintln!("capture: {e}");
                        return Ok(None);
                    }
                };
                let (cap_w, cap_h) = (rgba.width(), rgba.height());
                CAP_W.store(cap_w, Ordering::Relaxed);
                CAP_H.store(cap_h, Ordering::Relaxed);

                let raw = rgba.as_raw();
                let mut rgb = Vec::with_capacity((cap_w as usize) * (cap_h as usize) * 3);
                for px in raw.chunks_exact(4) {
                    rgb.push(px[0]);
                    rgb.push(px[1]);
                    rgb.push(px[2]);
                }
                let (w, h, rgb) = if max_w > 0 && cap_w > max_w {
                    let new_w = max_w;
                    let new_h = ((cap_h as u64) * (new_w as u64) / (cap_w as u64)) as u32;
                    let scaled = nearest_resize_rgb(&rgb, cap_w, cap_h, new_w, new_h);
                    (new_w, new_h, scaled)
                } else {
                    (cap_w, cap_h, rgb)
                };

                let mut jpeg = Vec::with_capacity(64 * 1024);
                {
                    let mut cursor = Cursor::new(&mut jpeg);
                    let mut enc = JpegEncoder::new_with_quality(&mut cursor, quality);
                    enc.encode(&rgb, w, h, ExtendedColorType::Rgb8)
                        .map_err(|e| anyhow!("jpeg encode: {e}"))?;
                }

                let mut payload = Vec::with_capacity(9 + jpeg.len());
                payload.push(FRAME_MAGIC);
                payload.extend_from_slice(&w.to_le_bytes());
                payload.extend_from_slice(&h.to_le_bytes());
                payload.extend_from_slice(&jpeg);
                Ok(Some(payload))
            })
            .await
            .map_err(|e| anyhow!("blocking task: {e}"))??;

            if let Some(payload) = payload {
                write.send(Message::Binary(payload)).await?;
            }

            // Pace ourselves: target frame interval AFTER send completes,
            // so a slow client/network slows capture too instead of queuing.
            let elapsed = frame_started.elapsed();
            if elapsed < frame_interval {
                tokio::time::sleep(frame_interval - elapsed).await;
            }
        }
    }
    .await;

    input_task.abort();
    result
}

fn nearest_resize_rgb(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dw as usize) * (dh as usize) * 3);
    let sw_us = sw as usize;
    for y in 0..dh {
        let sy = ((y as u64) * (sh as u64) / (dh as u64)) as usize;
        let row = sy * sw_us * 3;
        for x in 0..dw {
            let sx = ((x as u64) * (sw as u64) / (dw as u64)) as usize;
            let i = row + sx * 3;
            out.push(src[i]);
            out.push(src[i + 1]);
            out.push(src[i + 2]);
        }
    }
    out
}

fn input_thread(rx: std::sync::mpsc::Receiver<InputMessage>) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("enigo init failed: {e}. Input injection disabled.");
            return;
        }
    };
    while let Ok(input) = rx.recv() {
        if let Err(e) = apply_input(&mut enigo, input) {
            eprintln!("input error: {e}");
        }
    }
}

fn apply_input(g: &mut Enigo, input: InputMessage) -> Result<()> {
    let w = CAP_W.load(Ordering::Relaxed).max(1);
    let h = CAP_H.load(Ordering::Relaxed).max(1);

    match input {
        InputMessage::MouseMove { x, y } => {
            let px = (x.clamp(0.0, 1.0) * w as f32).round() as i32;
            let py = (y.clamp(0.0, 1.0) * h as f32).round() as i32;
            g.move_mouse(px, py, Coordinate::Abs)
                .map_err(|e| anyhow!("mouse move: {e}"))?;
        }
        InputMessage::MouseDown { button } => {
            let b = match button {
                MouseButton::Left => Button::Left,
                MouseButton::Right => Button::Right,
                MouseButton::Middle => Button::Middle,
            };
            g.button(b, Direction::Press)
                .map_err(|e| anyhow!("mouse down: {e}"))?;
        }
        InputMessage::MouseUp { button } => {
            let b = match button {
                MouseButton::Left => Button::Left,
                MouseButton::Right => Button::Right,
                MouseButton::Middle => Button::Middle,
            };
            g.button(b, Direction::Release)
                .map_err(|e| anyhow!("mouse up: {e}"))?;
        }
        InputMessage::Scroll { dx, dy } => {
            if dy != 0.0 {
                g.scroll(dy.signum() as i32 * 2, Axis::Vertical)
                    .map_err(|e| anyhow!("scroll: {e}"))?;
            }
            if dx != 0.0 {
                g.scroll(dx.signum() as i32 * 2, Axis::Horizontal)
                    .map_err(|e| anyhow!("scroll: {e}"))?;
            }
        }
        InputMessage::KeyDown { key } => {
            if let Some(k) = map_key(&key) {
                g.key(k, Direction::Press)
                    .map_err(|e| anyhow!("key: {e}"))?;
            }
        }
        InputMessage::KeyUp { key } => {
            if let Some(k) = map_key(&key) {
                g.key(k, Direction::Release)
                    .map_err(|e| anyhow!("key: {e}"))?;
            }
        }
    }
    Ok(())
}

fn map_key(s: &str) -> Option<Key> {
    if s.len() == 1 {
        let c = s.chars().next()?;
        return Some(Key::Unicode(c));
    }
    match s {
        "return" | "enter" => Some(Key::Return),
        "tab" => Some(Key::Tab),
        "space" => Some(Key::Space),
        "backspace" => Some(Key::Backspace),
        "escape" | "esc" => Some(Key::Escape),
        "delete" => Some(Key::Delete),
        "up" => Some(Key::UpArrow),
        "down" => Some(Key::DownArrow),
        "left" => Some(Key::LeftArrow),
        "right" => Some(Key::RightArrow),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" => Some(Key::PageUp),
        "pagedown" => Some(Key::PageDown),
        "f1" => Some(Key::F1),
        "f2" => Some(Key::F2),
        "f3" => Some(Key::F3),
        "f4" => Some(Key::F4),
        "f5" => Some(Key::F5),
        "f6" => Some(Key::F6),
        "f7" => Some(Key::F7),
        "f8" => Some(Key::F8),
        "f9" => Some(Key::F9),
        "f10" => Some(Key::F10),
        "f11" => Some(Key::F11),
        "f12" => Some(Key::F12),
        "meta" | "command" | "super" => Some(Key::Meta),
        "shift" => Some(Key::Shift),
        "control" | "ctrl" => Some(Key::Control),
        "alt" | "option" => Some(Key::Alt),
        _ => None,
    }
}
