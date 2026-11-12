//! A driver for the real window, for testing a build by hand from another computer: each line
//! appended to the file named by SSHREMOTE_DRIVE is turned into the window's own input (pointer,
//! keys, text) or a screenshot of it, so the app can be clicked through and looked at without the
//! system's mouse and keyboard. Results go to SSHREMOTE_DRIVE_OUT (default: beside the file):
//! `log.txt` gets `done N` after the Nth line, and `shot NAME` writes NAME.png.
//!
//! Lines: `wait MS`, `move X Y`, `click X Y [right|middle]`, `dbl X Y`, `drag X1 Y1 X2 Y2`,
//! `scroll DX DY`, `key NAME [ctrl+alt+shift+cmd]`, `text WORDS`, `shot NAME`, `info`,
//! `fullscreen on|off`, `exit`. Coordinates are in points; `#` starts a comment.

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Vec2};

struct Driver {
    file: PathBuf,
    out: PathBuf,
    /// Lines of the file already taken.
    taken: usize,
    /// Lines waiting their turn, with their number.
    queue: VecDeque<(usize, String)>,
    /// Events for the coming frames, one batch per frame.
    frames: VecDeque<Vec<Event>>,
    /// Nothing more starts before this.
    until: Instant,
    /// The line whose events are still going out.
    current: Option<usize>,
    last_read: Instant,
    /// A screenshot to ask for during the frame (a request made before it starts is dropped).
    shot: Option<String>,
}

static DRIVER: Mutex<Option<Driver>> = Mutex::new(None);

/// Starts the driver when SSHREMOTE_DRIVE names a file.
pub fn start() {
    let Some(file) = std::env::var_os("SSHREMOTE_DRIVE").map(PathBuf::from) else { return };
    let out = std::env::var_os("SSHREMOTE_DRIVE_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| file.parent().map(PathBuf::from).unwrap_or_default());
    let _ = std::fs::create_dir_all(&out);
    let now = Instant::now();
    *DRIVER.lock().unwrap_or_else(|e| e.into_inner()) = Some(Driver {
        file,
        out,
        taken: 0,
        queue: VecDeque::new(),
        frames: VecDeque::new(),
        until: now,
        current: None,
        last_read: now - Duration::from_secs(1),
        shot: None,
    });
}

/// Called with each frame's input, before the frame runs.
pub fn hook(ctx: &egui::Context, raw: &mut RawInput) {
    let mut guard = DRIVER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(d) = guard.as_mut() else { return };
    raw.focused = true;
    if let Some(batch) = d.frames.pop_front() {
        raw.events.extend(batch);
        return;
    }
    if let Some(n) = d.current.take() {
        d.log(&format!("done {n}"));
    }
    if Instant::now() < d.until {
        return;
    }
    if d.queue.is_empty() && d.last_read.elapsed() > Duration::from_millis(150) {
        d.read();
    }
    let Some((n, line)) = d.queue.pop_front() else { return };
    match d.run(ctx, &line) {
        Ok(()) => d.current = Some(n),
        Err(e) => d.log(&format!("error {n} {e}: {line}")),
    }
    if let Some(batch) = d.frames.pop_front() {
        raw.events.extend(batch);
    }
}

/// Called at the end of each frame: keeps frames coming (requests made before a frame starts are
/// dropped) and asks for the screenshot a `shot` line wanted.
pub fn end_of_frame(ctx: &egui::Context) {
    let mut guard = DRIVER.lock().unwrap_or_else(|e| e.into_inner());
    let Some(d) = guard.as_mut() else { return };
    ctx.request_repaint_after(Duration::from_millis(30));
    let Some(name) = d.shot.take() else { return };
    let path = d.out.join(format!("{name}.png"));
    let log = d.out.join("log.txt");
    ctx.request_screenshot(move |img| {
        let note = match save_png(&path, &img) {
            Ok(()) => format!("shot {name} {}x{}", img.size[0], img.size[1]),
            Err(e) => format!("shot {name} failed: {e}"),
        };
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
            let _ = writeln!(f, "{note}");
        }
    });
}

impl Driver {
    fn read(&mut self) {
        self.last_read = Instant::now();
        let Ok(text) = std::fs::read_to_string(&self.file) else { return };
        // Only whole lines: one still being written waits for its newline.
        let whole = text.rfind('\n').map_or("", |i| &text[..i]);
        for (i, line) in whole.lines().enumerate().skip(self.taken) {
            self.taken = i + 1;
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                self.log(&format!("done {}", i + 1));
            } else {
                self.queue.push_back((i + 1, line.to_string()));
            }
        }
    }

    fn log(&self, text: &str) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.out.join("log.txt")) {
            let _ = writeln!(f, "{text}");
        }
    }

    fn run(&mut self, ctx: &egui::Context, line: &str) -> Result<(), String> {
        let mut words = line.split_whitespace();
        let cmd = words.next().unwrap_or("");
        let rest: Vec<&str> = words.collect();
        let num = |i: usize| -> Result<f32, String> {
            rest.get(i).ok_or("missing number")?.parse::<f32>().map_err(|_| "not a number".to_string())
        };
        let at = |i: usize| -> Result<Pos2, String> { Ok(Pos2::new(num(i)?, num(i + 1)?)) };
        let mods = Modifiers::NONE;
        match cmd {
            "wait" => self.until = Instant::now() + Duration::from_millis(num(0)? as u64),
            "move" => self.frames.push_back(vec![Event::PointerMoved(at(0)?)]),
            "click" | "dbl" => {
                let pos = at(0)?;
                let button = match rest.get(2).copied() {
                    Some("right") => PointerButton::Secondary,
                    Some("middle") => PointerButton::Middle,
                    _ => PointerButton::Primary,
                };
                let b = |pressed| Event::PointerButton { pos, button, pressed, modifiers: mods };
                self.frames.push_back(vec![Event::PointerMoved(pos)]);
                for _ in 0..if cmd == "dbl" { 2 } else { 1 } {
                    self.frames.push_back(vec![b(true)]);
                    self.frames.push_back(vec![b(false)]);
                }
            }
            "drag" => {
                let (a, b) = (at(0)?, at(2)?);
                let press = |pos, pressed| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: mods };
                self.frames.push_back(vec![Event::PointerMoved(a)]);
                self.frames.push_back(vec![press(a, true)]);
                for k in 1..=10 {
                    self.frames.push_back(vec![Event::PointerMoved(a + (b - a) * (k as f32 / 10.0))]);
                }
                self.frames.push_back(vec![press(b, false)]);
            }
            "scroll" => self.frames.push_back(vec![Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(num(0)?, num(1)?),
                phase: egui::TouchPhase::Move,
                modifiers: mods,
                source: egui::MouseWheelSource::Wheel,
            }]),
            "key" => {
                let key = Key::from_name(rest.first().ok_or("missing key")?).ok_or("unknown key")?;
                let mut m = Modifiers::NONE;
                for part in rest.get(1).copied().unwrap_or("").split('+') {
                    match part {
                        "ctrl" => m.ctrl = true,
                        "alt" => m.alt = true,
                        "shift" => m.shift = true,
                        "cmd" => m.command = true,
                        "" => {}
                        _ => return Err(format!("unknown modifier {part}")),
                    }
                }
                if m.ctrl {
                    m.command = !cfg!(target_os = "macos") || m.command;
                }
                let k = |pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: m };
                self.frames.push_back(vec![k(true)]);
                self.frames.push_back(vec![k(false)]);
            }
            "text" => self.frames.push_back(vec![Event::Text(rest.join(" "))]),
            "shot" => {
                self.shot = Some(rest.first().ok_or("missing name")?.to_string());
                self.until = Instant::now() + Duration::from_millis(300);
            }
            "info" => {
                let v = ctx.input(|i| i.viewport().clone());
                self.log(&format!(
                    "info inner={:?} outer={:?} monitor={:?} fullscreen={:?} maximized={:?} ppp={} content={:?}",
                    v.inner_rect,
                    v.outer_rect,
                    v.monitor_size,
                    v.fullscreen,
                    v.maximized,
                    ctx.pixels_per_point(),
                    ctx.content_rect()
                ));
            }
            "fullscreen" => ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(rest.first() == Some(&"on"))),
            "exit" => {
                self.log("exit");
                std::process::exit(0);
            }
            _ => return Err("unknown command".into()),
        }
        Ok(())
    }
}

fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let mut rgba = Vec::with_capacity(img.pixels.len() * 4);
    for c in &img.pixels {
        rgba.extend_from_slice(&c.to_srgba_unmultiplied());
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.size[0] as u32, img.size[1] as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&rgba).map_err(|e| e.to_string())
}
