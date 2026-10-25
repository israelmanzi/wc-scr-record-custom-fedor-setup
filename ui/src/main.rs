use eframe::egui;
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AudioMode {
    System,
    Mic,
    Both,
    None,
}

impl AudioMode {
    fn flag(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Mic => "mic",
            Self::Both => "both",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Default)]
struct Status {
    mode: Mode,
    session: String,
    elapsed: String,
    preview: String,
}

impl Status {
    fn preview_running(&self) -> bool {
        self.preview.starts_with("running")
    }
}

#[derive(Clone)]
struct Session {
    name: String,
    duration: String,
    status: String,
}

enum Msg {
    Status(Status),
    Sessions(Vec<Session>),
    Paths {
        output_dir: String,
        config_path: String,
    },
    Done,
    Error(String),
}

#[derive(Clone)]
enum Action {
    StartWebcam,
    StartScreen,
    Stop,
    RestartPreview,
    OpenFolder,
    EditConfig,
    OpenSession(String),
    Finalize(String),
}

struct App {
    rec: Option<PathBuf>,
    output_dir: String,
    config_path: String,
    status: Status,
    sessions: Vec<Session>,
    audio: AudioMode,
    show_settings: bool,
    busy: bool,
    error: Option<(String, Instant)>,
    rx: Receiver<Msg>,
    tx: Sender<Msg>,
}

const GREEN: egui::Color32 = egui::Color32::from_rgb(90, 200, 130);
const BLUE: egui::Color32 = egui::Color32::from_rgb(100, 170, 240);
const ORANGE: egui::Color32 = egui::Color32::from_rgb(235, 170, 80);
const RED: egui::Color32 = egui::Color32::from_rgb(235, 100, 100);
const GREY: egui::Color32 = egui::Color32::from_rgb(150, 150, 150);

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
        let output_dir = run_rec(&rec, &["path".to_string()]).unwrap_or_default();
        let config_path = run_rec(&rec, &["config".to_string()]).unwrap_or_default();
        let _ = tx.send(Msg::Paths {
            output_dir,
            config_path,
        });

        let mut tick: u64 = 0;
        loop {
            if let Ok(raw) = run_rec(&rec, &["status".to_string()]) {
                let _ = tx.send(Msg::Status(parse_status(&raw)));
            }

            if tick % 3 == 0 {
                if let Ok(raw) = run_rec(&rec, &["list".to_string()]) {
                    let _ = tx.send(Msg::Sessions(parse_sessions(&raw)));
                }
            }

            tick = tick.wrapping_add(1);
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn parse_status(raw: &str) -> Status {
    let mut status = Status::default();

    let first = raw.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    let first = first.trim();

    status.mode = if first.starts_with("RECORDING") {
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
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }

        sessions.push(Session {
            name: fields[0].to_string(),
            duration: fields[1].to_string(),
            status: fields[3..].join(" "),
        });
    }

    sessions
}

fn session_label(name: &str) -> String {
    let base = name
        .strip_prefix("webcam-")
        .or_else(|| name.strip_prefix("screen-"))
        .unwrap_or(name);

    if let Some((date, time)) = base.split_once('_') {
        let date = date.get(5..).unwrap_or(date);
        let hours = time.get(0..2).unwrap_or("");
        let minutes = time.get(3..5).unwrap_or("");
        if hours.len() == 2 && minutes.len() == 2 {
            return format!("{date} {hours}:{minutes}");
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
        "session"
    }
}

fn is_recoverable(status: &str) -> bool {
    status.contains("chunks")
}

fn spawn_rec(tx: Sender<Msg>, rec: PathBuf, args: Vec<String>) {
    thread::spawn(move || {
        match Command::new(rec).args(args).output() {
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
        }
    });
}

fn spawn_open(tx: Sender<Msg>, path: PathBuf) {
    thread::spawn(move || {
        match Command::new("xdg-open").arg(path).spawn() {
            Ok(_) => {
                let _ = tx.send(Msg::Done);
            }
            Err(error) => {
                let _ = tx.send(Msg::Error(error.to_string()));
            }
        }
    });
}

fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let (tx, rx) = mpsc::channel();
        let rec = find_rec();
        if let Some(path) = &rec {
            start_poll(path.clone(), tx.clone());
        }

        Self {
            rec,
            output_dir: String::new(),
            config_path: String::new(),
            status: Status::default(),
            sessions: Vec::new(),
            audio: AudioMode::System,
            show_settings: false,
            busy: false,
            error: None,
            rx,
            tx,
        }
    }

    fn set_error(&mut self, message: impl Into<String>) {
        self.error = Some((message.into(), Instant::now()));
    }

    fn execute(&mut self, action: Action) {
        self.error = None;

        let rec = match &self.rec {
            Some(rec) => rec.clone(),
            None => {
                self.set_error("recorder command not found; set REC_BIN or add rec to PATH");
                return;
            }
        };

        match action {
            Action::StartWebcam => {
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["start".into()]);
            }
            Action::StartScreen => {
                self.busy = true;
                spawn_rec(
                    self.tx.clone(),
                    rec,
                    vec![
                        "screen".into(),
                        "start".into(),
                        format!("--audio={}", self.audio.flag()),
                    ],
                );
            }
            Action::Stop => {
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["stop".into()]);
            }
            Action::RestartPreview => {
                self.busy = true;
                spawn_rec(
                    self.tx.clone(),
                    rec,
                    vec!["preview".into(), "start".into()],
                );
            }
            Action::OpenFolder => {
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
                    spawn_open(
                        self.tx.clone(),
                        Path::new(&self.output_dir).join(name),
                    );
                }
            }
            Action::Finalize(name) => {
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
        }
    }

    fn drain_messages(&mut self) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Msg::Status(status) => self.status = status,
                Msg::Sessions(sessions) => self.sessions = sessions,
                Msg::Paths {
                    output_dir,
                    config_path,
                } => {
                    self.output_dir = output_dir.trim().to_string();
                    self.config_path = config_path.trim().to_string();
                }
                Msg::Done => self.busy = false,
                Msg::Error(text) => {
                    self.busy = false;
                    self.set_error(text);
                }
            }
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root_ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_messages();
        root_ui
            .ctx()
            .request_repaint_after(Duration::from_millis(500));

        let status = self.status.clone();
        let sessions = self.sessions.clone();
        let output_dir = self.output_dir.clone();
        let busy = self.busy;
        let mut audio = self.audio;
        let mut show_settings = self.show_settings;
        let mut action: Option<Action> = None;

        egui::CentralPanel::default().show(root_ui, |ui| {
            if self.rec.is_none() {
                ui.colored_label(
                    RED,
                    "recorder command not found.\nSet REC_BIN or add rec to PATH.",
                );
                return;
            }

            let (mode_label, mode_color) = match status.mode {
                Mode::Idle => ("Idle", GREY),
                Mode::Webcam => ("Recording webcam", GREEN),
                Mode::Screen => ("Recording screen", BLUE),
                Mode::Stale => ("Interrupted", ORANGE),
            };

            ui.horizontal(|ui| {
                status_dot(ui, mode_color);
                ui.label(
                    egui::RichText::new(mode_label)
                        .size(16.0)
                        .strong()
                        .color(mode_color),
                );

                if matches!(status.mode, Mode::Webcam | Mode::Screen)
                    && !status.elapsed.is_empty()
                {
                    ui.label(egui::RichText::new(&status.elapsed).monospace().weak());
                }

                if busy {
                    ui.spinner();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .selectable_label(show_settings, "Settings")
                        .clicked()
                    {
                        show_settings = !show_settings;
                    }
                    if ui.button("Folder").clicked() {
                        action = Some(Action::OpenFolder);
                    }
                });
            });

            if show_settings {
                ui.add_space(10.0);
                ui.group(|ui| {
                    ui.label(egui::RichText::new("Screen audio").small().weak());
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut audio, AudioMode::System, "System");
                        ui.radio_value(&mut audio, AudioMode::Mic, "Mic");
                        ui.radio_value(&mut audio, AudioMode::Both, "Both");
                        ui.radio_value(&mut audio, AudioMode::None, "None");
                    });
                    ui.separator();
                    if ui.button("Edit configuration").clicked() {
                        action = Some(Action::EditConfig);
                    }
                    if matches!(status.mode, Mode::Screen) && !status.preview_running() {
                        if ui.button("Restart preview").clicked() {
                            action = Some(Action::RestartPreview);
                        }
                    }
                    ui.separator();
                    ui.label(
                        egui::RichText::new("Super+F9 webcam  ·  Super+F10 screen")
                            .small()
                            .weak(),
                    );
                });
            }

            ui.add_space(14.0);

            match status.mode {
                Mode::Idle | Mode::Stale => {
                    if status.mode == Mode::Stale {
                        ui.add_enabled_ui(!busy, |ui| {
                            let width = ui.available_width();
                            if ui
                                .add_sized(
                                    [width, 46.0],
                                    egui::Button::new(
                                        egui::RichText::new("Recover").size(16.0),
                                    ),
                                )
                                .clicked()
                            {
                                action = Some(Action::Stop);
                            }
                        });
                    } else {
                        ui.add_enabled_ui(!busy, |ui| {
                            let width = ui.available_width();
                            if ui
                                .add_sized(
                                    [width, 48.0],
                                    egui::Button::new(
                                        egui::RichText::new("Record webcam").size(16.0),
                                    ),
                                )
                                .clicked()
                            {
                                action = Some(Action::StartWebcam);
                            }
                            ui.add_space(8.0);
                            if ui
                                .add_sized(
                                    [width, 48.0],
                                    egui::Button::new(
                                        egui::RichText::new("Record screen").size(16.0),
                                    ),
                                )
                                .clicked()
                            {
                                action = Some(Action::StartScreen);
                            }
                        });
                    }
                }
                Mode::Webcam | Mode::Screen => {
                    ui.add_enabled_ui(!busy, |ui| {
                        let width = ui.available_width();
                        if ui
                            .add_sized(
                                [width, 48.0],
                                egui::Button::new(
                                    egui::RichText::new("Stop").size(18.0).color(RED),
                                ),
                            )
                            .clicked()
                        {
                            action = Some(Action::Stop);
                        }
                    });
                }
            }

            if let Some((text, when)) = &self.error {
                if when.elapsed() < Duration::from_secs(8) {
                    ui.add_space(8.0);
                    ui.colored_label(RED, text);
                }
            }

            ui.add_space(14.0);
            ui.label(egui::RichText::new("Recent").strong());
            ui.add_space(4.0);

            let list_height = ui.available_height().max(80.0);
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .show(ui, |ui| {
                    if sessions.is_empty() {
                        ui.label(egui::RichText::new("No recordings").weak());
                    }

                    for session in &sessions {
                        if !status.session.is_empty()
                            && status.session.ends_with(&session.name)
                        {
                            continue;
                        }

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(session_label(&session.name))
                                    .monospace()
                                    .strong(),
                            )
                            .on_hover_text(&session.name);

                            ui.label(
                                egui::RichText::new(session_mode(&session.name))
                                    .small()
                                    .weak(),
                            );

                            let duration = if session.duration == "-" {
                                "—"
                            } else {
                                session.duration.as_str()
                            };
                            ui.label(egui::RichText::new(duration).small().weak());

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if is_recoverable(&session.status) {
                                        if ui.small_button("Recover").clicked() {
                                            action =
                                                Some(Action::Finalize(session.name.clone()));
                                        }
                                    } else if !output_dir.is_empty()
                                        && ui.small_button("Open").clicked()
                                    {
                                        action =
                                            Some(Action::OpenSession(session.name.clone()));
                                    }
                                },
                            );
                        });
                        ui.add_space(2.0);
                    }
                });
        });

        self.audio = audio;
        self.show_settings = show_settings;
        if let Some(action) = action {
            self.execute(action);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 520.0])
            .with_min_inner_size([340.0, 420.0])
            .with_title("Recorder"),
        ..Default::default()
    };

    eframe::run_native(
        "Recorder",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parses_idle_status() {
        let status = parse_status("idle\n");
        assert_eq!(status.mode, Mode::Idle);
        assert!(status.elapsed.is_empty());
    }

    #[test]
    fn parses_sessions() {
        let raw = "SESSION                        DURATION     SIZE  STATUS\nwebcam-2026-01-01_10-00-00     00:00:12     2.9M  finalized\nscreen-2026-01-01_10-05-00     00:01:00     8.0M  chunks only\n";
        let sessions = parse_sessions(raw);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "webcam-2026-01-01_10-00-00");
        assert_eq!(sessions[0].duration, "00:00:12");
        assert_eq!(sessions[0].status, "finalized");
        assert_eq!(sessions[1].status, "chunks only");
    }

    #[test]
    fn formats_session_labels() {
        assert_eq!(
            session_label("webcam-2026-10-25_18-53-18"),
            "10-25 18:53"
        );
        assert_eq!(
            session_label("screen-2026-10-25_17-27-28"),
            "10-25 17:27"
        );
        assert_eq!(session_label("2026-10-25_17-21-51"), "10-25 17:21");
        assert_eq!(session_mode("webcam-2026-10-25_18-53-18"), "webcam");
        assert_eq!(session_mode("screen-2026-10-25_17-27-28"), "screen");
        assert_eq!(session_mode("2026-10-25_17-21-51"), "session");
    }
}
