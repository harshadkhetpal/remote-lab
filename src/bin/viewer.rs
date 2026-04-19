//! Viewer: connects to remote-host over WebSocket, shows the stream, forwards pointer + scroll (+ basic keys).

use anyhow::{anyhow, Result};
use clap::Parser;
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use remote_lab::{parse_frame_header, InputMessage, MouseButton};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

#[derive(Parser, Debug)]
#[command(name = "remote-viewer")]
struct Args {
    /// WebSocket URL of the host, e.g. ws://192.168.1.10:9753/
    #[arg(long)]
    url: String,
}

fn main() -> eframe::Result<()> {
    let args = Args::parse();

    let (frame_tx, frame_rx) = mpsc::channel::<Vec<u8>>();
    let (input_tx, input_rx) = mpsc::channel::<InputMessage>();
    let url = args.url.clone();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("tokio runtime: {e}");
                return;
            }
        };
        if let Err(e) = rt.block_on(run_ws_client(url, frame_tx, input_rx)) {
            eprintln!("websocket client: {e:#}");
        }
    });

    let app = RemoteViewer::new(args.url, frame_rx, input_tx);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 540.0])
            .with_title("remote-lab viewer"),
        ..Default::default()
    };

    eframe::run_native(
        "remote-lab viewer",
        options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
}

async fn run_ws_client(
    url: String,
    frame_tx: Sender<Vec<u8>>,
    input_rx: Receiver<InputMessage>,
) -> Result<()> {
    let (ws, _) = connect_async(&url)
        .await
        .map_err(|e| anyhow!("connect {url}: {e}"))?;
    let (mut write, mut read) = ws.split();

    loop {
        tokio::select! {
            msg = read.next() => {
                let Some(msg) = msg else { break };
                let msg = msg.map_err(|e| anyhow!("ws read: {e}"))?;
                if let Message::Binary(b) = msg {
                    let _ = frame_tx.send(b);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(5)) => {
                while let Ok(ev) = input_rx.try_recv() {
                    let text = serde_json::to_string(&ev)
                        .map_err(|e| anyhow!("serialize input: {e}"))?;
                    write
                        .send(Message::Text(text.into()))
                        .await
                        .map_err(|e| anyhow!("ws send: {e}"))?;
                }
            }
        }
    }
    Ok(())
}

struct RemoteViewer {
    url: String,
    frame_rx: Receiver<Vec<u8>>,
    input_tx: Sender<InputMessage>,
    texture: Option<egui::TextureHandle>,
    last_size: egui::Vec2,
    status: String,
}

impl RemoteViewer {
    fn new(url: String, frame_rx: Receiver<Vec<u8>>, input_tx: Sender<InputMessage>) -> Self {
        Self {
            url,
            frame_rx,
            input_tx,
            texture: None,
            last_size: egui::Vec2::ZERO,
            status: "Connecting…".into(),
        }
    }

    fn push_frame(&mut self, ctx: &egui::Context, data: &[u8]) {
        let Some((_w, _h, jpeg)) = parse_frame_header(data) else {
            self.status = "Bad frame header from host".into();
            return;
        };
        let Ok(img) = image::load_from_memory(jpeg) else {
            self.status = "JPEG decode failed".into();
            return;
        };
        let rgba = img.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        let pixels = rgba.into_raw();
        let color = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
        let tex = ctx.load_texture("remote_frame", color, Default::default());
        self.last_size = tex.size_vec2();
        self.texture = Some(tex);
        self.status = "Connected".into();
    }

    fn send_input(&self, msg: InputMessage) {
        let _ = self.input_tx.send(msg);
    }
}

impl eframe::App for RemoteViewer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(buf) = self.frame_rx.try_recv() {
            self.push_frame(ctx, &buf);
        }
        if self.texture.is_some() {
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("URL: {}", self.url));
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(tex) = &self.texture else {
                ui.label("Waiting for first frame…");
                return;
            };

            let available = ui.available_size();
            let tex_sz = self.last_size;
            if tex_sz.x <= 0.0 || tex_sz.y <= 0.0 {
                ui.label("Invalid texture size");
                return;
            }

            let scale = (available.x / tex_sz.x).min(available.y / tex_sz.y);
            let display = tex_sz * scale;

            let response = ui.add_sized(
                display,
                egui::Image::new(tex).sense(egui::Sense::click_and_drag()),
            );
            let rect = response.rect;
            let hovered = response.hovered();

            if hovered {
                if let Some(pos) = ctx.pointer_interact_pos() {
                    if rect.contains(pos) {
                        let nx = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                        let ny = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                        self.send_input(InputMessage::MouseMove { x: nx, y: ny });
                    }
                }

                let scroll = ctx.input(|i| i.smooth_scroll_delta);
                if scroll != egui::Vec2::ZERO {
                    self.send_input(InputMessage::Scroll {
                        dx: scroll.x,
                        dy: scroll.y,
                    });
                }

                for ev in ctx.input(|i| i.events.clone()) {
                    match ev {
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            ..
                        } => {
                            if rect.contains(pos) {
                                let b = match button {
                                    egui::PointerButton::Primary => MouseButton::Left,
                                    egui::PointerButton::Secondary => MouseButton::Right,
                                    egui::PointerButton::Middle => MouseButton::Middle,
                                    egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => {
                                        continue;
                                    }
                                };
                                if pressed {
                                    self.send_input(InputMessage::MouseDown { button: b });
                                } else {
                                    self.send_input(InputMessage::MouseUp { button: b });
                                }
                            }
                        }
                        egui::Event::Key {
                            key,
                            pressed,
                            repeat,
                            ..
                        } => {
                            if repeat {
                                continue;
                            }
                            if let Some(name) = egui_key_to_host_name(key) {
                                if pressed {
                                    self.send_input(InputMessage::KeyDown { key: name.clone() });
                                } else {
                                    self.send_input(InputMessage::KeyUp { key: name });
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }
}

fn egui_key_to_host_name(key: egui::Key) -> Option<String> {
    match key {
        egui::Key::Enter => Some("return".into()),
        egui::Key::Tab => Some("tab".into()),
        egui::Key::Space => Some("space".into()),
        egui::Key::Backspace => Some("backspace".into()),
        egui::Key::Escape => Some("escape".into()),
        egui::Key::Delete => Some("delete".into()),
        egui::Key::ArrowUp => Some("up".into()),
        egui::Key::ArrowDown => Some("down".into()),
        egui::Key::ArrowLeft => Some("left".into()),
        egui::Key::ArrowRight => Some("right".into()),
        egui::Key::Home => Some("home".into()),
        egui::Key::End => Some("end".into()),
        egui::Key::PageUp => Some("pageup".into()),
        egui::Key::PageDown => Some("pagedown".into()),
        egui::Key::F1 => Some("f1".into()),
        egui::Key::F2 => Some("f2".into()),
        egui::Key::F3 => Some("f3".into()),
        egui::Key::F4 => Some("f4".into()),
        egui::Key::F5 => Some("f5".into()),
        egui::Key::F6 => Some("f6".into()),
        egui::Key::F7 => Some("f7".into()),
        egui::Key::F8 => Some("f8".into()),
        egui::Key::F9 => Some("f9".into()),
        egui::Key::F10 => Some("f10".into()),
        egui::Key::F11 => Some("f11".into()),
        egui::Key::F12 => Some("f12".into()),
        egui::Key::A => Some("a".into()),
        egui::Key::B => Some("b".into()),
        egui::Key::C => Some("c".into()),
        egui::Key::D => Some("d".into()),
        egui::Key::E => Some("e".into()),
        egui::Key::F => Some("f".into()),
        egui::Key::G => Some("g".into()),
        egui::Key::H => Some("h".into()),
        egui::Key::I => Some("i".into()),
        egui::Key::J => Some("j".into()),
        egui::Key::K => Some("k".into()),
        egui::Key::L => Some("l".into()),
        egui::Key::M => Some("m".into()),
        egui::Key::N => Some("n".into()),
        egui::Key::O => Some("o".into()),
        egui::Key::P => Some("p".into()),
        egui::Key::Q => Some("q".into()),
        egui::Key::R => Some("r".into()),
        egui::Key::S => Some("s".into()),
        egui::Key::T => Some("t".into()),
        egui::Key::U => Some("u".into()),
        egui::Key::V => Some("v".into()),
        egui::Key::W => Some("w".into()),
        egui::Key::X => Some("x".into()),
        egui::Key::Y => Some("y".into()),
        egui::Key::Z => Some("z".into()),
        egui::Key::Num0 => Some("0".into()),
        egui::Key::Num1 => Some("1".into()),
        egui::Key::Num2 => Some("2".into()),
        egui::Key::Num3 => Some("3".into()),
        egui::Key::Num4 => Some("4".into()),
        egui::Key::Num5 => Some("5".into()),
        egui::Key::Num6 => Some("6".into()),
        egui::Key::Num7 => Some("7".into()),
        egui::Key::Num8 => Some("8".into()),
        egui::Key::Num9 => Some("9".into()),
        _ => None,
    }
}
