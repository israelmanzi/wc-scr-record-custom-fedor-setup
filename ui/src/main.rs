mod caps;
mod config;

use config::Values;
use eframe::egui;
use std::{
    env, fs,
    io::Read,
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Mode {
    #[default]
    Idle,
    Webcam,
    Screen,
    Stale,
}

#[derive(Clone, Default)]
struct Status {
    mode: Mode,
    paused: bool,
    privacy: bool,
    session: String,
    elapsed: String,
    preview: String,
}

impl Status {
    #[allow(dead_code)]
    fn preview_running(&self) -> bool {
        self.preview.starts_with("running")
    }
}

#[derive(Clone)]
struct Session {
    name: String,
    duration: String,
    status: String,
    mode: String,
}

#[derive(Clone)]
struct PendingStart {
    mode: Mode,
    name: String,
    started: Instant,
    seconds: u64,
    resume: bool,
}

enum Msg {
    Status(Status),
    Sessions(Vec<Session>),
    Paths {
        output_dir: String,
        config_path: String,
    },
    Config(Values),
    ConfigError(String),
    Hardware(caps::Hardware, Result<caps::CameraCaps, String>, String),
    CameraCaps(String, Result<caps::CameraCaps, String>),
    Done,
    Error(String),
}

#[derive(Clone)]
enum Action {
    StartWebcam,
    StartScreen,
    Stop,
    Pause,
    Resume,
    PrivacyOff,
    RestartPreview,
    OpenFolder,
    OpenMenu,
    EditConfig,
    OpenSession(String),
    Finalize(String),
    SaveConfig,
    ReloadConfig,
    RefreshHardware,
    Quit,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuTab {
    History,
    Settings,
}

#[derive(Default)]
struct PreviewShared {
    current: Mutex<Option<Vec<u8>>>,
    spare: Mutex<Option<Vec<u8>>>,
}

impl PreviewShared {
    fn take_spare(&self, len: usize) -> Vec<u8> {
        self.spare
            .lock()
            .ok()
            .and_then(|mut spare| spare.take())
            .filter(|frame| frame.len() == len)
            .unwrap_or_else(|| vec![0u8; len])
    }

    fn publish(&self, frame: Vec<u8>, len: usize) -> Vec<u8> {
        let old = {
            let mut current = self
                .current
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            current.replace(frame)
        };
        old.unwrap_or_else(|| self.take_spare(len))
    }

    fn latest(&self) -> Option<Vec<u8>> {
        self.current
            .lock()
            .ok()
            .and_then(|mut current| current.take())
    }

    fn recycle(&self, frame: Vec<u8>) {
        if let Ok(mut spare) = self.spare.lock() {
            if spare.is_none() {
                *spare = Some(frame);
            }
        }
    }

    fn clear(&self) {
        if let Ok(mut current) = self.current.lock() {
            *current = None;
        }
    }
}

struct App {
    rec: Option<PathBuf>,
    output_dir: String,
    config_path: String,
    status: Status,
    sessions: Vec<Session>,
    busy: bool,
    error: Option<(String, Instant)>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
    menu_open: bool,
    menu_tab: MenuTab,
    session_name: String,
    timer_seconds: u64,
    pending_start: Option<PendingStart>,
    starting_recording: bool,
    name_used: bool,
    resume_guard_until: Option<Instant>,
    hardware: caps::Hardware,
    camera_caps: Option<caps::CameraCaps>,
    camera_caps_device: String,
    hardware_loading: bool,
    camera_loading: bool,
    hardware_error: Option<String>,
    hardware_note: Option<String>,
    settings: Values,
    saved_settings: Values,
    config_loaded: bool,
    config_loading: bool,
    settings_section: usize,
    preview: Arc<PreviewShared>,
    preview_texture: Option<egui::TextureHandle>,
    preview_enabled: bool,
    preview_w: usize,
    preview_h: usize,
    preview_ok: bool,
    fatal: Option<String>,
    screen_preview: Option<Child>,
    screen_was_active: bool,
    ui_pid_file: PathBuf,
    ui_size_file: PathBuf,
    window_active: Option<bool>,
    window_sized: bool,
    menu_initialized: bool,
    window_icon: Option<Arc<egui::IconData>>,
    icon_sent: bool,
    quit_requested: bool,
}

const GREEN: egui::Color32 = egui::Color32::from_rgb(90, 200, 130);
const BLUE: egui::Color32 = egui::Color32::from_rgb(100, 170, 240);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(235, 170, 80);
const RED: egui::Color32 = egui::Color32::from_rgb(235, 100, 100);
const GREY: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);
const WINDOW_W: f32 = 354.0;
const WINDOW_H: f32 = 320.0;

fn find_rec() -> Option<PathBuf> {
    if let Some(value) = env::var_os("REC_BIN") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(paths) = env::var_os("PATH") {
        for dir in env::split_paths(&paths) {
            let candidate = dir.join("rec");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for relative in ["bin/rec", "../bin/rec"] {
        let candidate = PathBuf::from(relative);
        if candidate.is_file() {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }

    None
}

fn run_rec(rec: &Path, args: &[String]) -> Result<String, String> {
    let output = Command::new(rec)
        .args(args)
        .output()
        .map_err(|error| error.to_string())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("rec exited with {}", output.status)
        } else {
            stderr
        })
    }
}

fn start_poll(rec: PathBuf, tx: Sender<Msg>) {
    thread::spawn(move || {
        let mut output_dir = run_rec(&rec, &["path".to_string()]).unwrap_or_default();
        let config_path = run_rec(&rec, &["config".to_string()]).unwrap_or_default();
        let _ = tx.send(Msg::Paths {
            output_dir: output_dir.clone(),
            config_path: config_path.clone(),
        });

        let mut tick: u64 = 0;
        loop {
            if let Ok(raw) = run_rec(&rec, &["status".to_string()]) {
                let _ = tx.send(Msg::Status(parse_status(&raw)));
            }

            if tick % 3 == 0 {
                if let Ok(path) = run_rec(&rec, &["path".to_string()]) {
                    output_dir = path.trim().to_string();
                    let _ = tx.send(Msg::Paths {
                        output_dir: output_dir.clone(),
                        config_path: config_path.clone(),
                    });
                }
                if let Ok(raw) = run_rec(&rec, &["list".to_string(), "--tsv".to_string()]) {
                    let mut sessions = parse_sessions(&raw);
                    fill_session_modes(&mut sessions, output_dir.trim());
                    let _ = tx.send(Msg::Sessions(sessions));
                }
            }

            tick = tick.wrapping_add(1);
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn spawn_load_hardware(device: String, tx: Sender<Msg>) {
    thread::spawn(move || {
        let (hardware, camera) = caps::load_hardware(&device);
        let _ = tx.send(Msg::Hardware(hardware, camera, device));
    });
}

fn spawn_load_camera(device: String, tx: Sender<Msg>) {
    thread::spawn(move || {
        let result = caps::load_camera_caps(&device);
        let _ = tx.send(Msg::CameraCaps(device, result));
    });
}

fn spawn_load_config(rec: PathBuf, tx: Sender<Msg>) {
    thread::spawn(move || match run_rec(&rec, &["values".to_string()]) {
        Ok(raw) => {
            let _ = tx.send(Msg::Config(config::parse_values(&raw)));
        }
        Err(error) => {
            let _ = tx.send(Msg::ConfigError(error));
        }
    });
}

fn parse_status(raw: &str) -> Status {
    let mut status = Status::default();

    let first = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let first = first.trim();

    status.paused = first.starts_with("PAUSED");
    status.privacy = first.starts_with("PRIVACY");
    status.mode = if first.starts_with("PRIVACY")
        || first.starts_with("PAUSED")
        || first.starts_with("RECORDING")
    {
        if first.contains("(screen)") {
            Mode::Screen
        } else {
            Mode::Webcam
        }
    } else if first.starts_with("STALE") {
        Mode::Stale
    } else {
        Mode::Idle
    };

    for line in raw.lines() {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "session" => status.session = value.trim().to_string(),
                "elapsed" => status.elapsed = value.trim().to_string(),
                "preview" => status.preview = value.trim().to_string(),
                _ => {}
            }
        }
    }

    status
}

fn parse_sessions(raw: &str) -> Vec<Session> {
    let mut sessions = Vec::new();

    for line in raw.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 4 {
            continue;
        }

        sessions.push(Session {
            name: fields[0].to_string(),
            duration: fields[1].to_string(),
            status: fields[3].to_string(),
            mode: session_mode(fields[0]).to_string(),
        });
    }

    sessions
}

fn fill_session_modes(sessions: &mut [Session], output_dir: &str) {
    for session in sessions {
        let mut recorded_mode = None;
        let info = Path::new(output_dir)
            .join(&session.name)
            .join("session-info.txt");
        if let Ok(text) = fs::read_to_string(info) {
            for line in text.lines() {
                if let Some(value) = line.strip_prefix("mode:") {
                    let value = value.trim();
                    if !value.is_empty() {
                        recorded_mode = Some(value.to_string());
                        break;
                    }
                }
            }
        }
        if let Some(mode) = recorded_mode {
            session.mode = mode;
        } else if session.mode.is_empty() {
            session.mode = "recording".to_string();
        }
    }
}

fn is_digits(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
}

fn auto_name_date(value: &str) -> Option<(&str, &str)> {
    let mut parts = value.split('-');
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    if parts.next().is_some()
        || year.len() != 4
        || month.len() != 2
        || day.len() != 2
        || !is_digits(year)
        || !is_digits(month)
        || !is_digits(day)
    {
        return None;
    }
    Some((month, day))
}

fn auto_name_time(value: &str) -> Option<(&str, &str)> {
    let mut parts = value.split('-');
    let hours = parts.next()?;
    let minutes = parts.next()?;
    let seconds = parts.next()?;
    if parts.next().is_some()
        || hours.len() != 2
        || minutes.len() != 2
        || seconds.len() != 2
        || !is_digits(hours)
        || !is_digits(minutes)
        || !is_digits(seconds)
    {
        return None;
    }
    Some((hours, minutes))
}

fn session_label(name: &str) -> String {
    let base = name
        .strip_prefix("webcam-")
        .or_else(|| name.strip_prefix("screen-"))
        .or_else(|| name.strip_prefix("session-"))
        .unwrap_or(name);

    if let Some((date, time)) = base.split_once('_') {
        if let (Some((month, day)), Some((hours, minutes))) =
            (auto_name_date(date), auto_name_time(time))
        {
            return format!("{month}-{day} {hours}:{minutes}");
        }
    }

    name.to_string()
}

fn session_mode(name: &str) -> &'static str {
    if name.starts_with("webcam-") {
        "webcam"
    } else if name.starts_with("screen-") {
        "screen"
    } else {
        ""
    }
}

fn is_recoverable(status: &str) -> bool {
    status.contains("chunks")
}

fn runtime_dir() -> PathBuf {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("webcam-rec")
}

fn ensure_preview_fifo(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let is_fifo = fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_fifo())
        .unwrap_or(false);
    if is_fifo {
        return Ok(());
    }
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    let output = Command::new("mkfifo")
        .args(["-m", "600"])
        .arg(path)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn claim_ui_pid(path: &Path) -> Result<(), String> {
    if let Ok(text) = fs::read_to_string(path) {
        if let Ok(pid) = text.trim().parse::<u32>() {
            if pid != std::process::id() && Path::new("/proc").join(pid.to_string()).exists() {
                return Err("another recorder UI is already running".to_string());
            }
        }
    }
    fs::write(path, format!("{}\n", std::process::id())).map_err(|error| error.to_string())
}

fn start_preview_reader(
    fifo: PathBuf,
    width: usize,
    height: usize,
    shared: Arc<PreviewShared>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        let len = width * height * 3;
        let mut frame = shared.take_spare(len);
        let mut filled = 0;
        let mut file: Option<fs::File> = None;
        let mut inode = 0;
        let mut last_check = Instant::now();

        loop {
            if file.is_none() {
                match fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&fifo)
                {
                    Ok(opened) => {
                        inode = opened.metadata().map(|meta| meta.ino()).unwrap_or(0);
                        filled = 0;
                        file = Some(opened);
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                }
            }

            let reader = file.as_mut().expect("preview file");
            match reader.read(&mut frame[filled..]) {
                Ok(0) => {
                    filled = 0;
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(count) => {
                    filled += count;
                    if filled == len {
                        frame = shared.publish(frame, len);
                        ctx.request_repaint();
                        filled = 0;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => {
                    file = None;
                    filled = 0;
                    thread::sleep(Duration::from_millis(100));
                }
            }

            if last_check.elapsed() >= Duration::from_millis(500) {
                last_check = Instant::now();
                let path_inode = fs::symlink_metadata(&fifo).map(|meta| meta.ino()).ok();
                if path_inode != Some(inode) {
                    file = None;
                    filled = 0;
                }
            }
        }
    });
}

fn spawn_rec(tx: Sender<Msg>, rec: PathBuf, args: Vec<String>) {
    thread::spawn(move || match Command::new(rec).args(args).output() {
        Ok(output) if output.status.success() => {
            let _ = tx.send(Msg::Done);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let message = if stderr.is_empty() {
                format!("rec exited with {}", output.status)
            } else {
                stderr
            };
            let _ = tx.send(Msg::Error(message));
        }
        Err(error) => {
            let _ = tx.send(Msg::Error(error.to_string()));
        }
    });
}

fn spawn_open(tx: Sender<Msg>, path: PathBuf) {
    thread::spawn(move || match Command::new("xdg-open").arg(path).spawn() {
        Ok(_) => {
            let _ = tx.send(Msg::Done);
        }
        Err(error) => {
            let _ = tx.send(Msg::Error(error.to_string()));
        }
    });
}

fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, color);
}

fn ensure_current_option(
    mut options: Vec<(String, String)>,
    current: &str,
) -> Vec<(String, String)> {
    if !options.iter().any(|(value, _)| value == current) {
        options.push((current.to_string(), format!("{current} (unavailable)")));
    }
    options
}

fn choice_widget(
    ui: &mut egui::Ui,
    id: &str,
    current: &str,
    options: &[(String, String)],
    width: f32,
) -> Option<String> {
    let selected = options
        .iter()
        .find(|(value, _)| value == current)
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| current.to_string());
    let mut value = current.to_string();
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .width(width)
        .show_ui(ui, |ui| {
            for (option, label) in options {
                ui.selectable_value(&mut value, option.clone(), label.clone());
            }
        });
    (value != current).then_some(value)
}

fn timer_combo(ui: &mut egui::Ui, timer: &mut u64, width: f32) {
    let combo = egui::ComboBox::from_id_salt("record-timer")
        .selected_text(egui::RichText::new(format!("{}s", *timer)).size(15.0))
        .width(width)
        .show_ui(ui, |ui| {
            for seconds in [0u64, 3, 5, 10] {
                ui.selectable_value(
                    timer,
                    seconds,
                    egui::RichText::new(format!("{seconds}s")).size(15.0),
                );
            }
        });
    combo.response.on_hover_text("Start and resume timer");
}

fn section_short(section: &str) -> &'static str {
    match section {
        "Capture" => "Capture",
        "Audio" => "Audio",
        "Webcam codec" => "Codec",
        "Recording" => "Output",
        "Screen recording" => "Screen",
        "Preview" => "Preview",
        _ => "Settings",
    }
}

fn configure_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (egui::TextStyle::Small, egui::FontId::proportional(14.0)),
            (egui::TextStyle::Body, egui::FontId::proportional(18.0)),
            (egui::TextStyle::Button, egui::FontId::proportional(18.0)),
            (egui::TextStyle::Heading, egui::FontId::proportional(23.0)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(16.0)),
        ]
        .into_iter()
        .collect();
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 9.0);
    });
}

fn clean_session_name(input: &str) -> String {
    let mut cleaned = String::new();
    for character in input.chars() {
        if character.is_control() {
            continue;
        }
        cleaned.push(match character {
            '/' | '\\' | '\'' => '-',
            _ => character,
        });
    }
    let trimmed = cleaned
        .trim_matches(|character: char| character.is_whitespace() || character == '.')
        .to_string();
    trimmed.chars().take(120).collect()
}

fn auto_session_name() -> String {
    let stamp = Command::new("date")
        .arg("+%Y-%m-%d_%H-%M-%S")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|stamp| !stamp.is_empty())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs().to_string())
                .unwrap_or_else(|_| "0".to_string())
        });
    format!("session-{stamp}")
}

fn display_geometry() -> Option<(f32, f32)> {
    let output = Command::new("xdotool")
        .args(["getdisplaygeometry"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.split_whitespace();
    let screen_w: f32 = parts.next()?.parse().ok()?;
    let screen_h: f32 = parts.next()?.parse().ok()?;
    Some((screen_w, screen_h))
}

fn initial_position(width: f32, height: f32, margin: f32) -> Option<egui::Pos2> {
    let (screen_w, screen_h) = display_geometry()?;
    Some(egui::pos2(
        (screen_w - width - margin).max(0.0),
        margin.min((screen_h - height).max(0.0)),
    ))
}

fn preferred_camera_size(sizes: &std::collections::BTreeMap<String, Vec<u32>>) -> Option<String> {
    if sizes.contains_key("1280x720") {
        return Some("1280x720".to_string());
    }
    sizes
        .keys()
        .max_by_key(|size| {
            let mut parts = size.split('x');
            let width = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let height = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            width * height
        })
        .cloned()
}

fn preferred_frame_rate(rates: &[u32], current: u32) -> u32 {
    rates
        .iter()
        .copied()
        .filter(|rate| *rate <= current)
        .max()
        .or_else(|| rates.iter().copied().max())
        .unwrap_or(current)
}

fn pin_window(active: bool) {
    let pid = std::process::id().to_string();
    thread::spawn(move || {
        for _ in 0..20 {
            let output = Command::new("xdotool")
                .args(["search", "--pid", &pid, "--name", "Webcam Recorder"])
                .output();
            let Ok(output) = output else {
                thread::sleep(Duration::from_millis(100));
                continue;
            };
            let text = String::from_utf8_lossy(&output.stdout);
            let Some(wid) = text.lines().map(str::trim).find(|line| !line.is_empty()) else {
                thread::sleep(Duration::from_millis(100));
                continue;
            };

            if active {
                let _ = Command::new("xdotool")
                    .args(["windowstate", "--add", "ABOVE", wid])
                    .status();
                let _ = Command::new("xdotool")
                    .args(["windowstate", "--add", "STICKY", wid])
                    .status();
                let _ = Command::new("xprop")
                    .args([
                        "-id",
                        wid,
                        "-f",
                        "_NET_WM_DESKTOP",
                        "32c",
                        "-set",
                        "_NET_WM_DESKTOP",
                        "4294967295",
                    ])
                    .status();
            } else {
                let _ = Command::new("xdotool")
                    .args(["windowstate", "--remove", "STICKY", wid])
                    .status();
                let desktop = Command::new("xdotool")
                    .arg("get_desktop")
                    .output()
                    .ok()
                    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "0".to_string());
                let _ = Command::new("xprop")
                    .args([
                        "-id",
                        wid,
                        "-f",
                        "_NET_WM_DESKTOP",
                        "32c",
                        "-set",
                        "_NET_WM_DESKTOP",
                        &desktop,
                    ])
                    .status();
            }
            break;
        }
    });
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        rec: Option<PathBuf>,
        settings: Values,
        config_loaded: bool,
    ) -> Self {
        configure_style(&cc.egui_ctx);

        let (preview_w, preview_h) = config::preview_size(&settings);
        let (tx, rx) = mpsc::channel();
        let window_icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
            .ok()
            .map(Arc::new);
        let preview = Arc::new(PreviewShared::default());
        let state_dir = runtime_dir();
        let fifo = state_dir.join("ui_preview.fifo");
        let ui_pid_file = state_dir.join("ui.pid");
        let ui_size_file = state_dir.join("ui.preview");

        let mut fatal = None;
        let mut preview_ok = false;
        match ensure_preview_fifo(&fifo) {
            Ok(()) => match claim_ui_pid(&ui_pid_file) {
                Ok(()) => {
                    if let Err(error) =
                        fs::write(&ui_size_file, format!("{preview_w} {preview_h}\n"))
                    {
                        fatal = Some(format!("cannot write preview size: {error}"));
                    } else {
                        start_preview_reader(
                            fifo.clone(),
                            preview_w,
                            preview_h,
                            preview.clone(),
                            cc.egui_ctx.clone(),
                        );
                        preview_ok = true;
                    }
                }
                Err(error) => fatal = Some(error),
            },
            Err(error) => fatal = Some(format!("cannot create preview pipe: {error}")),
        }

        if let Some(path) = &rec {
            start_poll(path.clone(), tx.clone());
        }
        let device = config::get(&settings, "DEVICE").to_string();
        spawn_load_hardware(device.clone(), tx.clone());

        Self {
            rec,
            output_dir: String::new(),
            config_path: String::new(),
            status: Status::default(),
            sessions: Vec::new(),
            busy: false,
            error: None,
            rx,
            tx,
            menu_open: false,
            menu_tab: MenuTab::History,
            session_name: auto_session_name(),
            timer_seconds: 0,
            pending_start: None,
            starting_recording: false,
            name_used: false,
            resume_guard_until: None,
            hardware: caps::Hardware::default(),
            camera_caps: None,
            camera_caps_device: device,
            hardware_loading: true,
            camera_loading: true,
            hardware_error: None,
            hardware_note: None,
            settings: settings.clone(),
            saved_settings: settings,
            config_loaded,
            config_loading: false,
            settings_section: 0,
            preview,
            preview_texture: None,
            preview_enabled: false,
            preview_w,
            preview_h,
            preview_ok,
            fatal,
            screen_preview: None,
            screen_was_active: false,
            ui_pid_file,
            ui_size_file,
            window_active: None,
            window_sized: false,
            menu_initialized: false,
            window_icon,
            icon_sent: false,
            quit_requested: false,
        }
    }

    fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some((message.into(), Instant::now()));
    }

    fn request_start(&mut self, mode: Mode) {
        if self.pending_start.is_some() || self.starting_recording || self.busy {
            return;
        }

        let cleaned = clean_session_name(&self.session_name);
        let name = if cleaned.is_empty() {
            auto_session_name()
        } else {
            cleaned
        };
        self.session_name = name.clone();

        if self.timer_seconds > 0 {
            self.error = None;
            self.pending_start = Some(PendingStart {
                mode,
                name,
                started: Instant::now(),
                seconds: self.timer_seconds,
                resume: false,
            });
        } else {
            self.begin_recording(mode, &name);
        }
    }

    fn request_resume(&mut self) {
        if self.pending_start.is_some() || self.starting_recording || self.busy {
            return;
        }
        if !matches!(self.status.mode, Mode::Webcam | Mode::Screen) || !self.status.paused {
            return;
        }

        if self.timer_seconds > 0 {
            self.pending_start = Some(PendingStart {
                mode: self.status.mode,
                name: String::new(),
                started: Instant::now(),
                seconds: self.timer_seconds,
                resume: true,
            });
        } else {
            self.begin_resume();
        }
    }

    fn begin_resume(&mut self) {
        let Some(rec) = self.rec.clone() else {
            self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
            return;
        };
        self.status.paused = false;
        self.resume_guard_until = Some(Instant::now() + Duration::from_secs(2));
        self.busy = true;
        spawn_rec(self.tx.clone(), rec, vec!["resume".into()]);
    }

    fn begin_recording(&mut self, mode: Mode, name: &str) {
        let Some(rec) = self.rec.clone() else {
            self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
            return;
        };

        if mode == Mode::Screen && self.preview_ok && !self.ffplay_preview() {
            if let Err(error) = self.start_screen_preview() {
                self.set_error(error);
                return;
            }
        }

        self.preview_enabled = true;
        self.clear_preview();
        self.busy = true;
        self.starting_recording = true;
        self.name_used = true;
        self.error = None;

        let args = match mode {
            Mode::Webcam => vec!["start".to_string(), "--name".to_string(), name.to_string()],
            Mode::Screen => vec![
                "screen".to_string(),
                "start".to_string(),
                "--name".to_string(),
                name.to_string(),
            ],
            _ => return,
        };
        spawn_rec(self.tx.clone(), rec, args);
    }

    fn pending_remaining(&self) -> Option<u64> {
        self.pending_start.as_ref().map(|pending| {
            let elapsed = pending.started.elapsed().as_secs_f32();
            (pending.seconds as f32 - elapsed).ceil().max(0.0) as u64
        })
    }

    fn countdown_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        let Some(pending) = &self.pending_start else {
            return;
        };
        let mode = pending.mode;
        let name = pending.name.clone();
        let resume = pending.resume;
        let remaining = self.pending_remaining().unwrap_or(0);
        let (label, color) = match mode {
            Mode::Screen => ("screen", BLUE),
            _ => ("webcam", GREEN),
        };

        if ui.ctx().input(|input| input.key_pressed(egui::Key::Escape)) {
            self.pending_start = None;
            return;
        }

        ui.horizontal(|ui| {
            status_dot(ui, color);
            ui.label(
                egui::RichText::new(format!(
                    "{} {label}",
                    if resume { "Resuming" } else { "Starting" }
                ))
                .size(16.0)
                .strong()
                .color(color),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Exit").size(15.0))
                            .min_size(egui::vec2(44.0, 26.0)),
                    )
                    .clicked()
                {
                    *action = Some(Action::Quit);
                }
            });
        });
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(egui::RichText::new(&name).monospace().size(12.0).weak()).truncate(),
        )
        .on_hover_text(&name);

        let mut cancel = false;
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new(remaining.to_string())
                    .size(72.0)
                    .strong()
                    .color(color),
            );
            ui.add_space(6.0);
            if ui
                .add_sized([120.0, 36.0], egui::Button::new("Cancel"))
                .clicked()
            {
                cancel = true;
            }
        });
        if cancel {
            self.pending_start = None;
        }
    }

    fn codec_options(&self, key: &str) -> Vec<(String, String)> {
        let Some(field) = config::field(key) else {
            return Vec::new();
        };
        let config::Kind::Choice(options) = field.kind else {
            return Vec::new();
        };
        let all: Vec<(String, String)> = options
            .iter()
            .map(|(value, label)| (value.to_string(), label.to_string()))
            .collect();
        if self.hardware.encoders.is_empty() {
            return all;
        }
        let filtered: Vec<(String, String)> = all
            .iter()
            .filter(|(value, _)| match value.as_str() {
                "hevc_nvenc" | "h264_nvenc" => {
                    self.hardware.has_nvidia && self.hardware.encoders.contains(value)
                }
                _ => self.hardware.encoders.contains(value),
            })
            .cloned()
            .collect();
        if filtered.is_empty() { all } else { filtered }
    }

    fn normalize_codec(&mut self, key: &str, preferred: &[&str]) -> bool {
        let options = self.codec_options(key);
        if options.is_empty() {
            return false;
        }
        let current = config::get(&self.settings, key).to_string();
        if options.iter().any(|(value, _)| value == &current) {
            return false;
        }
        let fallback = preferred
            .iter()
            .find(|name| options.iter().any(|(value, _)| value == *name))
            .map(|name| name.to_string())
            .or_else(|| options.first().map(|(value, _)| value.clone()));
        if let Some(value) = fallback {
            self.settings.insert(key.to_string(), value);
            true
        } else {
            false
        }
    }

    fn normalize_preset(&mut self) -> bool {
        let codec = config::get(&self.settings, "VIDEO_CODEC").to_string();
        let current = config::get(&self.settings, "PRESET").to_string();
        let fallback = match codec.as_str() {
            "libsvtav1" => {
                let valid = current
                    .parse::<u32>()
                    .map(|value| value <= 13)
                    .unwrap_or(false);
                (!valid).then_some("8")
            }
            "libx264" => {
                const X264_PRESETS: &[&str] = &[
                    "ultrafast",
                    "superfast",
                    "veryfast",
                    "faster",
                    "fast",
                    "medium",
                    "slow",
                    "slower",
                    "veryslow",
                ];
                (!X264_PRESETS.contains(&current.as_str())).then_some("veryfast")
            }
            _ => None,
        };
        if let Some(value) = fallback {
            self.settings
                .insert("PRESET".to_string(), value.to_string());
            true
        } else {
            false
        }
    }

    fn normalize_camera_selection(&mut self) -> bool {
        let current_device = config::get(&self.settings, "DEVICE").to_string();
        if self.camera_caps_device != current_device {
            return false;
        }
        let Some(caps) = self.camera_caps.clone() else {
            return false;
        };
        if caps.formats.is_empty() {
            return false;
        }

        let mut changed = false;
        let mut format = config::get(&self.settings, "INPUT_FORMAT").to_string();
        if !caps.formats.contains_key(&format) {
            format = if caps.formats.contains_key("mjpeg") {
                "mjpeg".to_string()
            } else {
                caps.formats.keys().next().cloned().unwrap_or_default()
            };
            self.settings
                .insert("INPUT_FORMAT".to_string(), format.clone());
            changed = true;
        }

        let Some(format_caps) = caps.formats.get(&format) else {
            return changed;
        };
        let mut size = config::get(&self.settings, "VIDEO_SIZE").to_string();
        if !format_caps.sizes.contains_key(&size) {
            size = preferred_camera_size(&format_caps.sizes).unwrap_or_default();
            self.settings.insert("VIDEO_SIZE".to_string(), size.clone());
            changed = true;
        }

        let Some(rates) = format_caps.sizes.get(&size) else {
            return changed;
        };
        if rates.is_empty() {
            return changed;
        }
        let current_rate = config::int(&self.settings, "FRAMERATE").max(0) as u32;
        if !rates.contains(&current_rate) {
            let rate = preferred_frame_rate(rates, current_rate);
            self.settings
                .insert("FRAMERATE".to_string(), rate.to_string());
            changed = true;
        }

        changed
    }

    fn normalize_quality_ranges(&mut self) -> bool {
        let mut changed = false;
        let codec = config::get(&self.settings, "VIDEO_CODEC");
        if codec == "libx264" {
            let current = config::int(&self.settings, "CRF");
            let clamped = current.clamp(0, 51);
            if current != clamped {
                self.settings.insert("CRF".to_string(), clamped.to_string());
                changed = true;
            }
        }
        let screen_cq = config::int(&self.settings, "SCREEN_CQ");
        let clamped = screen_cq.clamp(0, 51);
        if screen_cq != clamped {
            self.settings
                .insert("SCREEN_CQ".to_string(), clamped.to_string());
            changed = true;
        }
        changed
    }

    fn normalize_hardware_settings(&mut self) {
        if !self.menu_open {
            return;
        }
        let mut changed = false;
        changed |= self.normalize_codec(
            "VIDEO_CODEC",
            &["libsvtav1", "libx264", "hevc_nvenc", "h264_nvenc"],
        );
        changed |= self.normalize_codec("SCREEN_CODEC", &["hevc_nvenc", "h264_nvenc", "libx264"]);
        changed |= self.normalize_preset();
        changed |= self.normalize_quality_ranges();
        changed |= self.normalize_camera_selection();
        if changed {
            self.hardware_note =
                Some("Adjusted to the detected hardware. Save to apply.".to_string());
        }
    }

    fn settings_dirty(&self) -> bool {
        self.settings != self.saved_settings
    }

    fn ffplay_preview(&self) -> bool {
        env::var("PREVIEW_SINK")
            .map(|value| value == "ffplay")
            .unwrap_or(false)
    }

    fn ensure_camera_caps_for_current(&mut self) {
        let current = config::get(&self.settings, "DEVICE").to_string();
        if self.camera_caps_device == current {
            return;
        }
        self.camera_caps_device = current.clone();
        self.camera_caps = None;
        self.camera_loading = true;
        self.hardware_error = None;
        spawn_load_camera(current, self.tx.clone());
    }

    fn reload_hardware(&mut self) {
        if self.hardware_loading {
            return;
        }
        let device = config::get(&self.settings, "DEVICE").to_string();
        self.hardware_loading = true;
        self.camera_loading = true;
        self.hardware_error = None;
        self.hardware_note = None;
        spawn_load_hardware(device, self.tx.clone());
    }

    fn reload_config(&mut self) {
        if let Some(rec) = &self.rec {
            self.config_loading = true;
            spawn_load_config(rec.clone(), self.tx.clone());
        }
    }

    fn clear_preview(&mut self) {
        self.preview.clear();
        self.preview_texture = None;
    }

    fn start_screen_preview(&mut self) -> Result<(), String> {
        if !self.preview_ok {
            return Err("preview is unavailable".to_string());
        }
        if self.screen_preview.is_some() {
            return Ok(());
        }
        let fifo = self.preview_fifo_path();
        ensure_preview_fifo(&fifo)?;

        let width = self.preview_w;
        let height = self.preview_h;
        let device = config::get(&self.saved_settings, "DEVICE").to_string();
        let input_format = config::get(&self.saved_settings, "INPUT_FORMAT").to_string();
        let video_size = config::get(&self.saved_settings, "VIDEO_SIZE").to_string();
        let framerate = config::get(&self.saved_settings, "FRAMERATE").to_string();
        let filter = format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=rgb24"
        );

        let mut command = Command::new("ffmpeg");
        command
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "v4l2",
                "-input_format",
                &input_format,
                "-video_size",
                &video_size,
                "-framerate",
                &framerate,
                "-i",
                &device,
                "-vf",
                &filter,
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-y",
            ])
            .arg(&fifo)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = command.spawn().map_err(|error| error.to_string())?;
        self.screen_preview = Some(child);
        Ok(())
    }

    fn preview_fifo_path(&self) -> PathBuf {
        runtime_dir().join("ui_preview.fifo")
    }

    fn stop_screen_preview(&mut self) {
        if let Some(mut child) = self.screen_preview.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn check_screen_preview(&mut self) {
        let exited = match &mut self.screen_preview {
            Some(child) => match child.try_wait() {
                Ok(status) => status,
                Err(_) => None,
            },
            None => None,
        };
        if let Some(status) = exited {
            self.screen_preview = None;
            if !status.success() {
                self.set_error("Camera preview stopped.");
            }
        }
    }

    fn handle_transition(&mut self, old: Mode, new: Mode) {
        if old == new {
            return;
        }

        match new {
            Mode::Screen => {
                self.starting_recording = false;
                self.preview_enabled = true;
                self.screen_was_active = true;
                if self.preview_ok
                    && !self.ffplay_preview()
                    && self.screen_preview.is_none()
                    && !self.status.preview.starts_with("running")
                {
                    if let Err(error) = self.start_screen_preview() {
                        self.set_error(error);
                    }
                }
            }
            Mode::Webcam => {
                self.starting_recording = false;
                self.preview_enabled = true;
                self.stop_screen_preview();
                self.screen_was_active = false;
                if old != Mode::Idle {
                    self.clear_preview();
                }
            }
            Mode::Idle | Mode::Stale => {
                if matches!(old, Mode::Webcam | Mode::Screen) || self.screen_was_active {
                    self.preview_enabled = false;
                    self.clear_preview();
                }
                self.starting_recording = false;
                if self.name_used {
                    self.session_name = auto_session_name();
                    self.name_used = false;
                }
                self.screen_was_active = false;
                self.stop_screen_preview();
            }
        }
    }

    fn execute(&mut self, action: Action) {
        self.error = None;

        match action {
            Action::OpenMenu => {
                if !self.menu_open {
                    self.menu_open = true;
                    self.menu_tab = MenuTab::History;
                    self.menu_initialized = false;
                }
                if !self.settings_dirty() {
                    self.reload_config();
                }
                self.ensure_camera_caps_for_current();
                self.normalize_hardware_settings();
            }
            Action::ReloadConfig => {
                if !self.settings_dirty() {
                    self.reload_config();
                }
            }
            Action::RefreshHardware => self.reload_hardware(),
            Action::Quit => {
                self.quit_requested = true;
            }
            Action::StartWebcam => self.request_start(Mode::Webcam),
            Action::StartScreen => self.request_start(Mode::Screen),
            Action::Stop => {
                let Some(rec) = self.rec.clone() else {
                    self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
                    return;
                };
                self.preview_enabled = false;
                self.clear_preview();
                self.stop_screen_preview();
                self.resume_guard_until = None;
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["stop".into()]);
            }
            Action::Pause => {
                let Some(rec) = self.rec.clone() else {
                    self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
                    return;
                };
                self.resume_guard_until = None;
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["pause".into()]);
            }
            Action::Resume => self.request_resume(),
            Action::PrivacyOff => {
                let Some(rec) = self.rec.clone() else {
                    self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
                    return;
                };
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["privacy".into(), "stop".into()]);
            }
            Action::RestartPreview => {
                self.stop_screen_preview();
                if let Err(error) = self.start_screen_preview() {
                    self.set_error(error);
                }
            }
            Action::OpenFolder => {
                let Some(rec) = self.rec.clone() else {
                    self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
                    return;
                };
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["open".into()]);
            }
            Action::EditConfig => {
                if self.config_path.is_empty() {
                    self.set_error("configuration path unavailable");
                } else {
                    spawn_open(self.tx.clone(), PathBuf::from(&self.config_path));
                }
            }
            Action::OpenSession(name) => {
                if self.output_dir.is_empty() {
                    self.set_error("recordings directory unavailable");
                } else {
                    spawn_open(self.tx.clone(), Path::new(&self.output_dir).join(name));
                }
            }
            Action::Finalize(name) => {
                let Some(rec) = self.rec.clone() else {
                    self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
                    return;
                };
                if self.output_dir.is_empty() {
                    self.set_error("recordings directory unavailable");
                } else {
                    self.busy = true;
                    spawn_rec(
                        self.tx.clone(),
                        rec,
                        vec![
                            "finalize".into(),
                            Path::new(&self.output_dir)
                                .join(name)
                                .to_string_lossy()
                                .to_string(),
                        ],
                    );
                }
            }
            Action::SaveConfig => {
                if self.config_path.is_empty() {
                    self.set_error("configuration path unavailable");
                } else {
                    match config::write_changes(
                        Path::new(&self.config_path),
                        &self.settings,
                        &self.saved_settings,
                    ) {
                        Ok(_) => {
                            self.saved_settings = self.settings.clone();
                            self.hardware_note = None;
                            self.close_menu();
                        }
                        Err(error) => self.set_error(error),
                    }
                }
            }
        }
    }

    fn drain_messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Msg::Status(mut status) => {
                    if let Some(until) = self.resume_guard_until {
                        if status.paused && Instant::now() < until {
                            status.paused = false;
                        } else {
                            self.resume_guard_until = None;
                        }
                    }
                    let was_privacy = self.status.privacy;
                    let old = self.status.mode;
                    let new = status.mode;
                    self.status = status;
                    if !was_privacy && self.status.privacy && self.status.mode == Mode::Screen {
                        self.stop_screen_preview();
                    } else if was_privacy
                        && !self.status.privacy
                        && self.status.mode == Mode::Screen
                        && self.preview_ok
                        && !self.ffplay_preview()
                        && self.screen_preview.is_none()
                    {
                        if let Err(error) = self.start_screen_preview() {
                            self.set_error(error);
                        }
                    }
                    self.handle_transition(old, new);
                }
                Msg::Sessions(sessions) => self.sessions = sessions,
                Msg::Paths {
                    output_dir,
                    config_path,
                } => {
                    self.output_dir = output_dir.trim().to_string();
                    self.config_path = config_path.trim().to_string();
                }
                Msg::Config(values) => {
                    if !self.settings_dirty() {
                        self.settings = values.clone();
                        self.saved_settings = values;
                    }
                    self.config_loaded = true;
                    self.config_loading = false;
                    if self.menu_open {
                        self.ensure_camera_caps_for_current();
                        self.normalize_hardware_settings();
                    }
                }
                Msg::ConfigError(error) => {
                    self.config_loading = false;
                    self.set_error(format!("settings: {error}"));
                }
                Msg::Hardware(hardware, camera, device) => {
                    self.hardware = hardware;
                    self.hardware_loading = false;
                    let current = config::get(&self.settings, "DEVICE").to_string();
                    if device == current {
                        self.camera_loading = false;
                        self.camera_caps_device = device;
                        match camera {
                            Ok(caps) => {
                                self.camera_caps = Some(caps);
                                self.hardware_error = None;
                            }
                            Err(error) => {
                                self.camera_caps = None;
                                self.hardware_error = Some(error);
                            }
                        }
                    }
                    if self.menu_open {
                        self.normalize_hardware_settings();
                    }
                }
                Msg::CameraCaps(device, result) => {
                    let current = config::get(&self.settings, "DEVICE").to_string();
                    if device == current {
                        self.camera_loading = false;
                        self.camera_caps_device = device;
                        match result {
                            Ok(caps) => {
                                self.camera_caps = Some(caps);
                                self.hardware_error = None;
                            }
                            Err(error) => {
                                self.camera_caps = None;
                                self.hardware_error = Some(error);
                            }
                        }
                        if self.menu_open {
                            self.normalize_hardware_settings();
                        }
                    }
                }
                Msg::Done => self.busy = false,
                Msg::Error(text) => {
                    self.busy = false;
                    self.resume_guard_until = None;
                    if self.starting_recording {
                        self.name_used = false;
                    }
                    self.starting_recording = false;
                    if !matches!(self.status.mode, Mode::Webcam | Mode::Screen) {
                        self.preview_enabled = false;
                        self.clear_preview();
                        self.stop_screen_preview();
                    }
                    self.set_error(text);
                }
            }
        }
    }

    fn drain_preview(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.preview.latest() else {
            return;
        };
        if !self.preview_enabled {
            self.preview.recycle(frame);
            return;
        }
        let image = egui::ColorImage::from_rgb([self.preview_w, self.preview_h], &frame);
        if let Some(texture) = &mut self.preview_texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.preview_texture =
                Some(ctx.load_texture("camera-preview", image, egui::TextureOptions::LINEAR));
        }
        self.preview.recycle(frame);
    }

    fn sync_window_state(&mut self, ctx: &egui::Context) {
        let active = matches!(self.status.mode, Mode::Webcam | Mode::Screen)
            || self.pending_start.is_some()
            || self.starting_recording;
        if self.window_active == Some(active) {
            return;
        }
        self.window_active = Some(active);
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop,
        ));
        pin_window(active);
    }

    fn draw_preview(&self, ui: &mut egui::Ui, max_height: f32) {
        let outer_w = ui.available_width();
        let height = (outer_w * 9.0 / 16.0).min(max_height.max(40.0));
        let width = (height * 16.0 / 9.0).min(outer_w);
        let left = ((outer_w - width) / 2.0).max(0.0);

        ui.horizontal(|ui| {
            ui.add_space(left);
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click_and_drag());
            let painter = ui.painter();
            painter.rect_filled(rect, 8.0, egui::Color32::from_rgb(24, 26, 30));
            if self.status.privacy {
                painter.text(
                    rect.center() - egui::vec2(0.0, 8.0),
                    egui::Align2::CENTER_CENTER,
                    "Recording blurred",
                    egui::FontId::proportional(18.0),
                    ORANGE,
                );
                painter.text(
                    rect.center() + egui::vec2(0.0, 16.0),
                    egui::Align2::CENTER_CENTER,
                    "Screen blurred · mic muted",
                    egui::FontId::proportional(12.0),
                    GREY,
                );
            } else if let Some(texture) = &self.preview_texture {
                painter.image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else {
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Camera preview",
                    egui::FontId::proportional(15.0),
                    GREY,
                );
            }
            if self.status.paused {
                let label = match self.status.mode {
                    Mode::Screen => "Paused (screen)",
                    _ => "Paused (webcam)",
                };
                painter.rect_filled(rect, 8.0, egui::Color32::from_black_alpha(140));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    label,
                    egui::FontId::proportional(18.0),
                    ORANGE,
                );
            }
            painter.rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(55, 58, 64)),
                egui::StrokeKind::Inside,
            );
            if response.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            let _ = response.on_hover_cursor(egui::CursorIcon::Grab);
        });
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        if self.pending_start.is_some() {
            self.countdown_ui(ui, action);
            return;
        }

        if let Some(fatal) = &self.fatal {
            ui.colored_label(RED, fatal);
        }
        if self.rec.is_none() {
            ui.colored_label(
                RED,
                "recorder command not found.\nSet REC_BIN or add rec to PATH.",
            );
        }

        let status = self.status.clone();
        let (mode_label, mode_color) = if status.privacy {
            ("Privacy", ORANGE)
        } else if status.paused {
            ("Paused", ORANGE)
        } else {
            match status.mode {
                Mode::Idle => ("Idle", GREY),
                Mode::Webcam => ("Webcam", GREEN),
                Mode::Screen => ("Screen", BLUE),
                Mode::Stale => ("Interrupted", ORANGE),
            }
        };

        ui.horizontal(|ui| {
            status_dot(ui, mode_color);
            ui.label(
                egui::RichText::new(mode_label)
                    .size(16.0)
                    .strong()
                    .color(mode_color),
            );

            if matches!(status.mode, Mode::Webcam | Mode::Screen) && !status.elapsed.is_empty() {
                ui.label(
                    egui::RichText::new(&status.elapsed)
                        .monospace()
                        .size(14.0)
                        .weak(),
                );
            }

            if self.busy {
                ui.spinner();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Exit").size(15.0))
                            .min_size(egui::vec2(44.0, 26.0)),
                    )
                    .on_hover_text("Exit (stops an active recording)")
                    .clicked()
                {
                    *action = Some(Action::Quit);
                }
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new("Menu").size(15.0))
                            .min_size(egui::vec2(56.0, 26.0)),
                    )
                    .clicked()
                {
                    *action = Some(Action::OpenMenu);
                }
            });
        });

        ui.add_space(6.0);

        if status.mode == Mode::Idle {
            ui.horizontal(|ui| {
                let timer_width = 56.0;
                let name_width = (ui.available_width() - timer_width - 8.0).max(80.0);
                let response = ui.add_sized(
                    [name_width, 28.0],
                    egui::TextEdit::singleline(&mut self.session_name)
                        .char_limit(120)
                        .font(egui::FontId::proportional(15.0)),
                );
                let _ = response.on_hover_text("Session name");
                timer_combo(ui, &mut self.timer_seconds, timer_width);
            });
            ui.add_space(4.0);
        } else if status.paused {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Timer").small().weak());
                ui.add_space(4.0);
                timer_combo(ui, &mut self.timer_seconds, 56.0);
            });
            ui.add_space(4.0);
        }

        let error_text = self
            .error
            .as_ref()
            .filter(|(_, when)| when.elapsed() < Duration::from_secs(8))
            .map(|(text, _)| text.clone());
        let error_space = if error_text.is_some() { 30.0 } else { 0.0 };
        let max_preview = (ui.available_height() - 44.0 - 8.0 - error_space).max(60.0);
        self.draw_preview(ui, max_preview);
        ui.add_space(8.0);

        let enabled = !self.busy && self.rec.is_some();
        match status.mode {
            Mode::Idle => {
                ui.columns(2, |columns| {
                    let width = columns[0].available_width();
                    if columns[0]
                        .add_enabled(
                            enabled,
                            egui::Button::new(egui::RichText::new("Record webcam").size(16.0))
                                .min_size(egui::vec2(width, 44.0)),
                        )
                        .clicked()
                    {
                        *action = Some(Action::StartWebcam);
                    }
                    if columns[1]
                        .add_enabled(
                            enabled,
                            egui::Button::new(egui::RichText::new("Record screen").size(16.0))
                                .min_size(egui::vec2(width, 44.0)),
                        )
                        .clicked()
                    {
                        *action = Some(Action::StartScreen);
                    }
                });
            }
            Mode::Stale => {
                let width = ui.available_width();
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(egui::RichText::new("Recover").size(17.0))
                            .min_size(egui::vec2(width, 44.0)),
                    )
                    .clicked()
                {
                    *action = Some(Action::Stop);
                }
            }
            Mode::Webcam | Mode::Screen => {
                ui.columns(2, |columns| {
                    let width = columns[0].available_width();
                    let (primary_label, primary_action) = if status.privacy {
                        ("Reveal", Action::PrivacyOff)
                    } else if status.paused {
                        ("Resume", Action::Resume)
                    } else {
                        ("Pause", Action::Pause)
                    };
                    if columns[0]
                        .add_enabled(
                            enabled,
                            egui::Button::new(
                                egui::RichText::new(primary_label)
                                    .size(16.0)
                                    .color(mode_color),
                            )
                            .min_size(egui::vec2(width, 44.0)),
                        )
                        .clicked()
                    {
                        *action = Some(primary_action);
                    }
                    if columns[1]
                        .add_enabled(
                            enabled,
                            egui::Button::new(egui::RichText::new("Stop").size(16.0).color(RED))
                                .min_size(egui::vec2(width, 44.0)),
                        )
                        .clicked()
                    {
                        *action = Some(Action::Stop);
                    }
                });
            }
        }

        if let Some(text) = error_text {
            ui.add_space(4.0);
            ui.add(egui::Label::new(egui::RichText::new(&text).color(RED).size(14.0)).truncate())
                .on_hover_text(text);
        }
    }

    fn close_menu(&mut self) {
        self.menu_open = false;
        self.menu_initialized = false;
        self.hardware_note = None;
        if self.settings_dirty() {
            self.settings = self.saved_settings.clone();
        }
        self.ensure_camera_caps_for_current();
    }

    fn menu_window_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        if ui.ctx().input(|input| input.viewport().close_requested()) {
            self.close_menu();
            return;
        }

        let mut close = false;
        egui::Frame::central_panel(ui.style())
            .inner_margin(14.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.menu_tab, MenuTab::History, "History");
                    ui.selectable_value(&mut self.menu_tab, MenuTab::Settings, "Settings");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                });
                ui.separator();

                let max_height = (ui.available_height() - 8.0).max(120.0);
                match self.menu_tab {
                    MenuTab::History => self.history_ui(ui, action, max_height),
                    MenuTab::Settings => self.settings_ui(ui, action, max_height),
                }
            });

        if close {
            self.close_menu();
        }
    }

    fn history_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>, max_height: f32) {
        let status = self.status.clone();
        let sessions = self.sessions.clone();
        let output_dir = self.output_dir.clone();

        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} recordings", sessions.len()))
                    .small()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!output_dir.is_empty(), egui::Button::new("Open folder"))
                    .clicked()
                {
                    *action = Some(Action::OpenFolder);
                }
            });
        });
        ui.add_space(4.0);

        let height = (max_height - 120.0).max(120.0);
        egui::ScrollArea::vertical()
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut shown = 0;
                let active_name = Path::new(&status.session)
                    .file_name()
                    .and_then(|name| name.to_str());
                for session in &sessions {
                    if active_name == Some(session.name.as_str()) {
                        continue;
                    }
                    shown += 1;

                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(session_label(&session.name))
                                .monospace()
                                .strong(),
                        )
                        .wrap(),
                    )
                    .on_hover_text(&session.name);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&session.mode).small().weak());
                        let duration = if session.duration == "-" {
                            "—"
                        } else {
                            session.duration.as_str()
                        };
                        ui.label(egui::RichText::new(duration).small().weak());

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if is_recoverable(&session.status) {
                                if ui.small_button("Recover").clicked() {
                                    *action = Some(Action::Finalize(session.name.clone()));
                                }
                            } else if !output_dir.is_empty() && ui.small_button("Open").clicked() {
                                *action = Some(Action::OpenSession(session.name.clone()));
                            }
                        });
                    });
                    ui.add_space(6.0);
                }

                if shown == 0 {
                    ui.label(egui::RichText::new("No recordings yet").weak());
                }
            });
    }

    fn camera_caps_current(&self) -> Option<&caps::CameraCaps> {
        if self.camera_caps_device == config::get(&self.settings, "DEVICE") {
            self.camera_caps.as_ref()
        } else {
            None
        }
    }

    fn dynamic_options(&self, key: &str) -> Option<Vec<(String, String)>> {
        let current = config::get(&self.settings, key).to_string();
        let options = match key {
            "DEVICE" => {
                if self.hardware.devices.is_empty() {
                    return None;
                }
                self.hardware
                    .devices
                    .iter()
                    .map(|device| (device.path.clone(), device.name.clone()))
                    .collect()
            }
            "INPUT_FORMAT" => {
                let caps = self.camera_caps_current()?;
                if caps.formats.is_empty() {
                    return None;
                }
                caps.formats
                    .iter()
                    .map(|(value, format)| (value.clone(), format!("{} ({value})", format.fourcc)))
                    .collect()
            }
            "VIDEO_SIZE" => {
                let caps = self.camera_caps_current()?;
                let format = config::get(&self.settings, "INPUT_FORMAT");
                let sizes = &caps.formats.get(format)?.sizes;
                if sizes.is_empty() {
                    return None;
                }
                sizes
                    .keys()
                    .map(|size| (size.clone(), size.clone()))
                    .collect()
            }
            "FRAMERATE" => {
                let caps = self.camera_caps_current()?;
                let format = config::get(&self.settings, "INPUT_FORMAT");
                let size = config::get(&self.settings, "VIDEO_SIZE");
                let rates = caps.formats.get(format)?.sizes.get(size)?;
                if rates.is_empty() {
                    return None;
                }
                rates
                    .iter()
                    .map(|rate| (rate.to_string(), format!("{rate} fps")))
                    .collect()
            }
            "AUDIO_SOURCE" => {
                if self.hardware.audio_sources.is_empty() {
                    return None;
                }
                let mut options = vec![("default".to_string(), "System default input".to_string())];
                options.extend(
                    self.hardware
                        .audio_sources
                        .iter()
                        .filter(|source| !source.monitor)
                        .map(|source| (source.name.clone(), source.description.clone())),
                );
                options
            }
            "AUDIO_MONITOR" => {
                if self.hardware.audio_sources.is_empty() {
                    return None;
                }
                let mut options =
                    vec![("auto".to_string(), "Automatic (default sink)".to_string())];
                options.extend(
                    self.hardware
                        .audio_sources
                        .iter()
                        .filter(|source| source.monitor)
                        .map(|source| (source.name.clone(), source.description.clone())),
                );
                options
            }
            "VIDEO_CODEC" => self.codec_options("VIDEO_CODEC"),
            "SCREEN_CODEC" => self.codec_options("SCREEN_CODEC"),
            "PRESET" => {
                let codec = config::get(&self.settings, "VIDEO_CODEC");
                match codec {
                    "libsvtav1" => (0..=13)
                        .map(|value| {
                            let suffix = if value == 0 {
                                " (fastest)"
                            } else if value == 13 {
                                " (slowest)"
                            } else {
                                ""
                            };
                            (value.to_string(), format!("{value}{suffix}"))
                        })
                        .collect(),
                    "libx264" => [
                        "ultrafast",
                        "superfast",
                        "veryfast",
                        "faster",
                        "fast",
                        "medium",
                        "slow",
                        "slower",
                        "veryslow",
                        "placebo",
                    ]
                    .iter()
                    .map(|value| (value.to_string(), value.to_string()))
                    .collect(),
                    _ => return None,
                }
            }
            _ => return None,
        };
        if options.is_empty() {
            None
        } else {
            Some(ensure_current_option(options, &current))
        }
    }

    fn field_enabled(&self, key: &str) -> (bool, Option<&'static str>) {
        let audio = config::bool_value(&self.settings, "AUDIO_ENABLED");
        let mic_gain = audio && config::bool_value(&self.settings, "MIC_GAIN");
        let codec = config::get(&self.settings, "VIDEO_CODEC");
        let nvenc = matches!(codec, "hevc_nvenc" | "h264_nvenc");
        let cpu = matches!(codec, "libsvtav1" | "libx264");
        let input_format = config::get(&self.settings, "INPUT_FORMAT");
        let screen_audio = config::get(&self.settings, "SCREEN_AUDIO");
        let screen_codec = config::get(&self.settings, "SCREEN_CODEC");

        match key {
            "AUDIO_SOURCE" | "AUDIO_BITRATE" => (audio, Some("Microphone audio is disabled")),
            "MIC_GAIN" => (audio, Some("Microphone audio is disabled")),
            "MIC_BOOST" | "MIC_CAPTURE" => (mic_gain, Some("Enable microphone gain first")),
            "PRESET" => (
                cpu,
                Some(if nvenc {
                    "Not used by NVENC"
                } else {
                    "Used by CPU encoders"
                }),
            ),
            "CRF" => (
                cpu,
                Some(if nvenc {
                    "Not used by NVENC"
                } else {
                    "Used by CPU encoders"
                }),
            ),
            "NVENC_CQ" | "NVENC_PRESET" => (nvenc, Some("NVENC encoders only")),
            "GPU_DECODE" => (
                nvenc
                    && input_format == "mjpeg"
                    && (self.hardware.has_mjpeg_cuvid || self.hardware.encoders.is_empty()),
                Some("Requires NVENC, MJPEG and NVDEC"),
            ),
            "SVT_PRED_STRUCT" => (codec == "libsvtav1", Some("SVT-AV1 only")),
            "SCREEN_PRESET" => (
                screen_codec != "libx264",
                Some("NVENC screen encoders only"),
            ),
            "SCREEN_AUDIO_BITRATE" => (screen_audio != "none", Some("Screen audio is off")),
            "AUDIO_MONITOR" => (
                matches!(screen_audio, "system" | "both"),
                Some("Used for system audio"),
            ),
            _ => (true, None),
        }
    }

    fn apply_setting_change(&mut self, key: &str, old: &str, new: &str) {
        if old == new {
            return;
        }
        self.settings.insert(key.to_string(), new.to_string());
        match key {
            "DEVICE" => {
                self.camera_caps = None;
                self.camera_caps_device = new.to_string();
                self.camera_loading = true;
                self.hardware_error = None;
                self.hardware_note = None;
                spawn_load_camera(new.to_string(), self.tx.clone());
            }
            "INPUT_FORMAT" | "VIDEO_SIZE" | "FRAMERATE" => {
                if self.normalize_camera_selection() {
                    self.hardware_note = Some(
                        "Adjusted the camera mode to supported values. Save to apply.".to_string(),
                    );
                }
            }
            "VIDEO_CODEC" => {
                let mut adjusted = self.normalize_preset();
                adjusted |= self.normalize_quality_ranges();
                if adjusted {
                    self.hardware_note = Some(
                        "Adjusted encoder settings to match the codec. Save to apply.".to_string(),
                    );
                }
            }
            "SCREEN_CODEC" => {
                if self.normalize_quality_ranges() {
                    self.hardware_note =
                        Some("Adjusted screen encoder settings. Save to apply.".to_string());
                }
            }
            _ => {}
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>, max_height: f32) {
        if !self.config_loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Loading settings…");
            });
            return;
        }

        let footer_height = 200.0;
        let scroll_height = (max_height - footer_height).max(120.0);
        let sections = config::sections();
        if self.settings_section >= sections.len() {
            self.settings_section = 0;
        }
        let active_section = sections[self.settings_section];

        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.horizontal_wrapped(|ui| {
                for (index, section) in sections.iter().enumerate() {
                    ui.selectable_value(
                        &mut self.settings_section,
                        index,
                        egui::RichText::new(section_short(section)).size(15.0),
                    );
                }
            });
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt(("settings-section", active_section))
            .max_height(scroll_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Changes apply to the next recording.")
                        .small()
                        .weak(),
                );
                ui.add_space(4.0);

                if self.hardware_loading || self.camera_loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Detecting camera and audio…")
                                .small()
                                .weak(),
                        );
                    });
                } else if let Some(error) = &self.hardware_error {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("Camera unavailable: {error}"))
                                .small()
                                .color(ORANGE),
                        )
                        .truncate(),
                    )
                    .on_hover_text(error);
                }
                if let Some(note) = &self.hardware_note {
                    ui.add(
                        egui::Label::new(egui::RichText::new(note).small().color(ORANGE)).wrap(),
                    );
                }
                if active_section == "Capture" && !self.hardware_loading {
                    ui.label(
                        egui::RichText::new(format!(
                            "Detected {} camera(s), {} audio source(s)",
                            self.hardware.devices.len(),
                            self.hardware.audio_sources.len()
                        ))
                        .small()
                        .weak(),
                    );
                }

                for field in config::FIELDS
                    .iter()
                    .filter(|field| field.section == active_section)
                {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(field.label).strong())
                            .on_hover_text(field.help);
                        if field.restart {
                            ui.label(
                                egui::RichText::new("restart to apply")
                                    .small()
                                    .color(ORANGE),
                            );
                        }
                    });

                    let (enabled, hint) = self.field_enabled(field.key);
                    let current = config::get(&self.settings, field.key).to_string();
                    let mut changed = None;
                    ui.add_enabled_ui(enabled, |ui| {
                        if let Some(options) = self.dynamic_options(field.key) {
                            changed = choice_widget(
                                ui,
                                field.key,
                                &current,
                                &options,
                                ui.available_width(),
                            );
                        } else {
                            match field.kind {
                                config::Kind::Text => {
                                    let mut value = current.clone();
                                    if ui
                                        .add(
                                            egui::TextEdit::singleline(&mut value)
                                                .desired_width(ui.available_width()),
                                        )
                                        .changed()
                                    {
                                        changed = Some(value);
                                    }
                                }
                                config::Kind::Bool => {
                                    let mut value = config::bool_value(&self.settings, field.key);
                                    if ui.checkbox(&mut value, "").changed() {
                                        changed = Some(if value {
                                            "1".to_string()
                                        } else {
                                            "0".to_string()
                                        });
                                    }
                                }
                                config::Kind::Number { min, max } => {
                                    let mut value =
                                        config::int(&self.settings, field.key).clamp(min, max);
                                    if ui.add(egui::Slider::new(&mut value, min..=max)).changed() {
                                        changed = Some(value.to_string());
                                    }
                                }
                                config::Kind::Choice(options) => {
                                    let options: Vec<(String, String)> = options
                                        .iter()
                                        .map(|(value, label)| {
                                            (value.to_string(), label.to_string())
                                        })
                                        .collect();
                                    let options = ensure_current_option(options, &current);
                                    changed = choice_widget(
                                        ui,
                                        field.key,
                                        &current,
                                        &options,
                                        ui.available_width(),
                                    );
                                }
                            }
                        }
                    });
                    if let Some(value) = changed {
                        self.apply_setting_change(field.key, &current, &value);
                    }
                    if let Some(hint) = hint {
                        if !enabled {
                            ui.label(egui::RichText::new(hint).small().weak());
                        }
                    }
                    ui.add_space(4.0);
                }
            });

        ui.separator();
        let dirty = self.settings_dirty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(dirty && !self.config_loading, egui::Button::new("Save"))
                .clicked()
            {
                *action = Some(Action::SaveConfig);
            }
            if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                self.settings = self.saved_settings.clone();
                self.hardware_note = None;
                self.ensure_camera_caps_for_current();
            }
            if ui.button("Edit file").clicked() {
                *action = Some(Action::EditConfig);
            }
            if self.config_loading {
                ui.spinner();
            }
        });

        ui.horizontal(|ui| {
            if matches!(self.status.mode, Mode::Screen) && self.preview_ok {
                if ui.button("Restart preview").clicked() {
                    *action = Some(Action::RestartPreview);
                }
            }
            if ui
                .add_enabled(
                    !self.hardware_loading,
                    egui::Button::new("Refresh hardware"),
                )
                .clicked()
            {
                *action = Some(Action::RefreshHardware);
            }
            if ui.button("Reload").clicked() {
                *action = Some(Action::ReloadConfig);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Quit").clicked() {
                    *action = Some(Action::Quit);
                }
            });
        });

        if let Some((text, when)) = &self.error {
            if when.elapsed() < Duration::from_secs(8) {
                ui.add(egui::Label::new(egui::RichText::new(text).color(RED).small()).wrap());
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(pending) = &self.pending_start {
            if pending.started.elapsed() >= Duration::from_secs(pending.seconds) {
                let pending = self.pending_start.take().expect("pending start");
                if pending.resume {
                    self.begin_resume();
                } else {
                    self.begin_recording(pending.mode, &pending.name);
                }
                ctx.request_repaint();
            }
        }
    }

    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_messages();
        self.check_screen_preview();

        let ctx = root_ui.ctx().clone();
        if !self.window_sized {
            let ppp = ctx.pixels_per_point();
            if ppp > 0.0 {
                let logical_w = (WINDOW_W / ppp).max(320.0);
                let logical_h = (WINDOW_H / ppp).max(290.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                    logical_w, logical_h,
                )));
                if let Some((screen_w, _)) = display_geometry() {
                    let margin =
                        config::int(&self.saved_settings, "PREVIEW_MARGIN").clamp(0, 500) as f32;
                    let physical_w = logical_w * ppp;
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                        (screen_w - physical_w - margin).max(0.0) / ppp,
                        margin / ppp,
                    )));
                }
            }
            self.window_sized = true;
        }
        if !self.icon_sent {
            if let Some(icon) = &self.window_icon {
                ctx.send_viewport_cmd(egui::ViewportCommand::Icon(Some(icon.clone())));
            }
            self.icon_sent = true;
        }
        self.drain_preview(&ctx);
        self.sync_window_state(&ctx);

        let repaint = if self.pending_start.is_some()
            || matches!(self.status.mode, Mode::Webcam | Mode::Screen)
            || self.screen_preview.is_some()
            || self.starting_recording
        {
            Duration::from_millis(33)
        } else {
            Duration::from_millis(500)
        };
        ctx.request_repaint_after(repaint);

        if !self.menu_open {
            if self.settings_dirty() {
                self.settings = self.saved_settings.clone();
            }
            self.menu_initialized = false;
        }

        let mut action: Option<Action> = None;
        egui::CentralPanel::default().show(root_ui, |ui| {
            self.main_ui(ui, &mut action);
        });

        if self.menu_open {
            let menu_id = egui::ViewportId::from_hash_of("recorder-menu");
            let monitor = ctx
                .input(|input| input.viewport().monitor_size)
                .unwrap_or(egui::vec2(1920.0, 1080.0));
            let mut builder = egui::ViewportBuilder::default()
                .with_title("Recorder Menu")
                .with_min_inner_size([480.0, 420.0])
                .with_always_on_top();
            if !self.menu_initialized {
                let menu_w = 640.0f32.min((monitor.x - 40.0).max(480.0));
                let menu_h = 620.0f32.min((monitor.y - 60.0).max(420.0));
                builder = builder.with_inner_size([menu_w, menu_h]);
                if let Some(main) = ctx.input(|input| input.viewport().outer_rect) {
                    let left = main.min.x - menu_w - 12.0;
                    let x = if left >= 0.0 {
                        left
                    } else {
                        (main.max.x + 12.0).min(monitor.x - menu_w).max(0.0)
                    };
                    let y = main.min.y.min((monitor.y - menu_h).max(0.0)).max(0.0);
                    builder = builder.with_position(egui::pos2(x, y));
                }
            }

            let mut menu_action = None;
            ctx.show_viewport_immediate(menu_id, builder, |ui, _class| {
                self.menu_window_ui(ui, &mut menu_action);
            });
            if !self.menu_initialized {
                self.menu_initialized = true;
                ctx.send_viewport_cmd_to(menu_id, egui::ViewportCommand::Focus);
            }
            if menu_action.is_some() {
                action = menu_action;
            }
        }

        if let Some(action) = action {
            self.execute(action);
        }

        if self.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop_screen_preview();
        let _ = fs::remove_file(&self.ui_size_file);
        if let Ok(text) = fs::read_to_string(&self.ui_pid_file) {
            if text.trim() == std::process::id().to_string() {
                let _ = fs::remove_file(&self.ui_pid_file);
            }
        }

        if self.rec.is_some()
            && (matches!(self.status.mode, Mode::Webcam | Mode::Screen)
                || self.screen_was_active
                || self.busy)
        {
            if let Some(rec) = &self.rec {
                let _ = Command::new("setsid")
                    .arg(rec)
                    .arg("stop")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let rec = find_rec();
    let loaded = rec
        .as_ref()
        .and_then(|rec| run_rec(rec, &["values".to_string()]).ok());
    let config_loaded = loaded.is_some();
    let settings = loaded
        .map(|raw| config::parse_values(&raw))
        .unwrap_or_else(config::defaults);
    let margin = config::int(&settings, "PREVIEW_MARGIN").clamp(0, 500) as f32;

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Webcam Recorder")
        .with_inner_size([WINDOW_W, WINDOW_H])
        .with_decorations(false)
        .with_resizable(false)
        .with_always_on_top();

    if let Some(position) = initial_position(WINDOW_W, WINDOW_H, margin) {
        viewport = viewport.with_position(position);
    }
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        viewport = viewport.with_icon(icon);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Webcam Recorder",
        options,
        Box::new(move |cc| {
            Ok(Box::new(App::new(
                cc,
                rec.clone(),
                settings.clone(),
                config_loaded,
            )))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_window_icon() {
        assert!(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).is_ok());
    }

    #[test]
    fn parses_recording_status() {
        let raw = "RECORDING (screen)\n  session : /tmp/screen-1\n  elapsed : 00:01:02\n  chunks  : 3 (4.5M)\n  free    : 42 GB\n  audio   : both\n  preview : running (320x180 PiP at +20,+20)\n";
        let status = parse_status(raw);
        assert_eq!(status.mode, Mode::Screen);
        assert_eq!(status.session, "/tmp/screen-1");
        assert_eq!(status.elapsed, "00:01:02");
        assert!(status.preview_running());
    }

    #[test]
    fn parses_privacy_status() {
        let raw = "PRIVACY (screen)\n  session : /tmp/screen-1\n  elapsed : 00:00:20\n  state   : privacy\n";
        let status = parse_status(raw);
        assert_eq!(status.mode, Mode::Screen);
        assert!(status.privacy);
        assert_eq!(status.elapsed, "00:00:20");
    }

    #[test]
    fn parses_paused_status() {
        let raw = "PAUSED (screen)\n  session : /tmp/screen-1\n  elapsed : 00:00:20\n  state   : paused\n  preview : paused\n";
        let status = parse_status(raw);
        assert_eq!(status.mode, Mode::Screen);
        assert!(status.paused);
        assert_eq!(status.elapsed, "00:00:20");
    }

    #[test]
    fn parses_idle_status() {
        let status = parse_status("idle\n");
        assert_eq!(status.mode, Mode::Idle);
        assert!(status.elapsed.is_empty());
    }

    #[test]
    fn parses_sessions() {
        let raw = "NAME\tDURATION\tSIZE\tSTATUS\nwebcam-2026-01-01_10-00-00\t00:00:12\t2.9M\tfinalized\nTeam standup\t00:01:00\t8.0M\tchunks only\n";
        let sessions = parse_sessions(raw);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "webcam-2026-01-01_10-00-00");
        assert_eq!(sessions[0].duration, "00:00:12");
        assert_eq!(sessions[0].status, "finalized");
        assert_eq!(sessions[0].mode, "webcam");
        assert_eq!(sessions[1].name, "Team standup");
        assert_eq!(sessions[1].status, "chunks only");
        assert_eq!(sessions[1].mode, "");
    }

    #[test]
    fn formats_session_labels() {
        assert_eq!(session_label("webcam-2026-10-25_18-53-18"), "10-25 18:53");
        assert_eq!(session_label("screen-2026-10-25_17-27-28"), "10-25 17:27");
        assert_eq!(session_label("2026-10-25_17-21-51"), "10-25 17:21");
        assert_eq!(session_label("session-2026-10-25_17-21-51"), "10-25 17:21");
        assert_eq!(session_label("Team standup"), "Team standup");
        assert_eq!(
            session_label("BIT_AI_LAB-2026-10-26_20-25-03"),
            "BIT_AI_LAB-2026-10-26_20-25-03"
        );
        assert_eq!(session_mode("webcam-2026-10-25_18-53-18"), "webcam");
        assert_eq!(session_mode("screen-2026-10-25_17-27-28"), "screen");
        assert_eq!(session_mode("2026-10-25_17-21-51"), "");
    }
}
