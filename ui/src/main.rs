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
    chunks: String,
    free: String,
    audio: String,
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
    size: String,
    status: String,
}

enum Msg {
    Status(Status),
    Sessions(Vec<Session>),
    Paths { output_dir: String, config_path: String },
    Done(String),
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
    busy: bool,
    message: Option<(String, bool, Instant)>,
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
            let value = value.trim().to_string();
            match key.trim() {
                "session" => status.session = value,
                "elapsed" => status.elapsed = value,
                "chunks" => status.chunks = value,
                "free" => status.free = value,
                "audio" => status.audio = value,
                "preview" => status.preview = value,
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
            size: fields[2].to_string(),
            status: fields[3..].join(" "),
        });
    }

    sessions
}

fn spawn_rec(tx: Sender<Msg>, rec: PathBuf, args: Vec<String>, success: &'static str) {
    thread::spawn(move || {
        match Command::new(rec).args(args).output() {
            Ok(output) if output.status.success() => {
                let _ = tx.send(Msg::Done(success.to_string()));
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

fn spawn_open(tx: Sender<Msg>, path: PathBuf, success: &'static str) {
    thread::spawn(move || {
        match Command::new("xdg-open").arg(path).spawn() {
            Ok(_) => {
                let _ = tx.send(Msg::Done(success.to_string()));
            }
            Err(error) => {
                let _ = tx.send(Msg::Error(error.to_string()));
            }
        }
    });
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
            busy: false,
            message: None,
            rx,
            tx,
        }
    }

    fn execute(&mut self, action: Action) {
        let rec = match &self.rec {
            Some(rec) => rec.clone(),
            None => {
                self.message = Some((
                    "recorder command not found; set REC_BIN or add rec to PATH".into(),
                    true,
                    Instant::now(),
                ));
                return;
            }
        };

        match action {
            Action::StartWebcam => {
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["start".into()], "Webcam started");
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
                    "Screen recording started",
                );
            }
            Action::Stop => {
                self.busy = true;
                spawn_rec(self.tx.clone(), rec, vec!["stop".into()], "Recording stopped");
            }
            Action::RestartPreview => {
                self.busy = true;
                spawn_rec(
                    self.tx.clone(),
                    rec,
                    vec!["preview".into(), "start".into()],
                    "Preview restarted",
                );
            }
            Action::OpenFolder => {
                spawn_rec(
                    self.tx.clone(),
                    rec,
                    vec!["open".into()],
                    "Opened recordings folder",
                );
            }
            Action::EditConfig => {
                if self.config_path.is_empty() {
                    self.message = Some(("Configuration path unavailable".into(), true, Instant::now()));
                } else {
                    spawn_open(
                        self.tx.clone(),
                        PathBuf::from(&self.config_path),
                        "Opened configuration",
                    );
                }
            }
            Action::OpenSession(name) => {
                if self.output_dir.is_empty() {
                    self.message = Some(("Recordings directory unavailable".into(), true, Instant::now()));
                } else {
                    spawn_open(
                        self.tx.clone(),
                        Path::new(&self.output_dir).join(name),
                        "Opened session",
                    );
                }
            }
            Action::Finalize(name) => {
                if self.output_dir.is_empty() {
                    self.message = Some(("Recordings directory unavailable".into(), true, Instant::now()));
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
                        "Session finalized",
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
                Msg::Done(text) => {
                    self.busy = false;
                    self.message = Some((text, false, Instant::now()));
                }
                Msg::Error(text) => {
                    self.busy = false;
                    self.message = Some((text, true, Instant::now()));
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
        let mut action: Option<Action> = None;

        egui::CentralPanel::default().show(root_ui, |ui| {
            ui.heading("Recorder");
            ui.label(
                egui::RichText::new("Super+F9 webcam  ·  Super+F10 screen")
                    .small()
                    .weak(),
            );
            ui.add_space(10.0);

            if self.rec.is_none() {
                ui.label(
                    egui::RichText::new(
                        "recorder command not found.\nSet REC_BIN or add rec to PATH.",
                    )
                    .color(RED),
                );
                return;
            }

            let (mode_label, mode_color) = match status.mode {
                Mode::Idle => ("Idle", GREY),
                Mode::Webcam => ("Recording webcam", GREEN),
                Mode::Screen => ("Recording screen", BLUE),
                Mode::Stale => ("Stale session", ORANGE),
            };

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(mode_label)
                        .size(20.0)
                        .strong()
                        .color(mode_color),
                );
                if busy {
                    ui.spinner();
                }
            });
            ui.add_space(8.0);

            match status.mode {
                Mode::Idle | Mode::Stale => {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(
                                    egui::RichText::new("Start webcam").size(16.0),
                                )
                                .min_size(egui::vec2(185.0, 44.0)),
                            )
                            .clicked()
                        {
                            action = Some(Action::StartWebcam);
                        }

                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(
                                    egui::RichText::new("Start screen").size(16.0),
                                )
                                .min_size(egui::vec2(185.0, 44.0)),
                            )
                            .clicked()
                        {
                            action = Some(Action::StartScreen);
                        }
                    });

                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label("Screen audio:");
                        ui.radio_value(&mut audio, AudioMode::System, "System");
                        ui.radio_value(&mut audio, AudioMode::Mic, "Mic");
                        ui.radio_value(&mut audio, AudioMode::Both, "Both");
                        ui.radio_value(&mut audio, AudioMode::None, "None");
                    });

                    if status.mode == Mode::Stale {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("The recorder state is stale; stop to finalize.")
                                .color(ORANGE),
                        );
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new("Stop and finalize")
                                    .min_size(egui::vec2(185.0, 36.0)),
                            )
                            .clicked()
                        {
                            action = Some(Action::Stop);
                        }
                    }
                }
                Mode::Webcam | Mode::Screen => {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(
                                    egui::RichText::new("Stop").size(18.0).color(RED),
                                )
                                .min_size(egui::vec2(185.0, 44.0)),
                            )
                            .clicked()
                        {
                            action = Some(Action::Stop);
                        }

                        if status.mode == Mode::Screen && !status.preview_running() {
                            if ui
                                .add_enabled(
                                    !busy,
                                    egui::Button::new("Restart preview")
                                        .min_size(egui::vec2(160.0, 44.0)),
                                )
                                .clicked()
                            {
                                action = Some(Action::RestartPreview);
                            }
                        }
                    });
                }
            }

            if status.mode != Mode::Idle {
                ui.add_space(10.0);
                egui::Grid::new("status_grid")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        if !status.session.is_empty() {
                            ui.label("Session");
                            ui.label(egui::RichText::new(&status.session).monospace().small());
                            ui.end_row();
                        }
                        if !status.elapsed.is_empty() {
                            ui.label("Elapsed");
                            ui.label(&status.elapsed);
                            ui.end_row();
                        }
                        if !status.chunks.is_empty() {
                            ui.label("Chunks");
                            ui.label(&status.chunks);
                            ui.end_row();
                        }
                        if !status.free.is_empty() {
                            ui.label("Free space");
                            ui.label(&status.free);
                            ui.end_row();
                        }
                        if !status.audio.is_empty() {
                            ui.label("Audio");
                            ui.label(&status.audio);
                            ui.end_row();
                        }
                        if !status.preview.is_empty() {
                            ui.label("Preview");
                            ui.label(&status.preview);
                            ui.end_row();
                        }
                    });
            }

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Open recordings").clicked() {
                    action = Some(Action::OpenFolder);
                }
                if ui.button("Edit configuration").clicked() {
                    action = Some(Action::EditConfig);
                }
            });

            ui.add_space(8.0);
            ui.heading("Recent recordings");
            egui::ScrollArea::vertical()
                .max_height(190.0)
                .show(ui, |ui| {
                    if sessions.is_empty() {
                        ui.label(egui::RichText::new("No recordings yet.").weak());
                    }

                    for session in &sessions {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(&session.name).monospace().size(13.0),
                            );
                            let color = if session.status == "finalized" {
                                GREEN
                            } else {
                                ORANGE
                            };
                            ui.label(egui::RichText::new(&session.status).color(color).small());
                        });
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&session.duration).small().weak());
                            ui.label(egui::RichText::new(&session.size).small().weak());

                            if !output_dir.is_empty() && ui.small_button("Open").clicked() {
                                action = Some(Action::OpenSession(session.name.clone()));
                            }
                            if session.status.contains("chunks")
                                && ui.small_button("Finalize").clicked()
                            {
                                action = Some(Action::Finalize(session.name.clone()));
                            }
                        });
                        ui.separator();
                    }
                });

            if let Some((text, is_error, when)) = &self.message {
                if when.elapsed() < Duration::from_secs(10) {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(text)
                            .color(if *is_error { RED } else { GREEN })
                            .small(),
                    );
                }
            }

            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("The preview is a separate window controlled by the recorder.")
                    .small()
                    .weak(),
            );
        });

        self.audio = audio;
        if let Some(action) = action {
            self.message = None;
            self.execute(action);
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 650.0])
            .with_min_inner_size([380.0, 500.0]),
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
        assert_eq!(status.chunks, "3 (4.5M)");
        assert_eq!(status.audio, "both");
        assert!(status.preview_running());
    }

    #[test]
    fn parses_idle_status() {
        let status = parse_status("idle\n");
        assert_eq!(status.mode, Mode::Idle);
        assert!(status.session.is_empty());
    }

    #[test]
    fn parses_sessions() {
        let raw = "SESSION                        DURATION     SIZE  STATUS\nwebcam-2026-01-01_10-00-00     00:00:12     2.9M  finalized\nscreen-2026-01-01_10-05-00     00:01:00     8.0M  chunks only\n";
        let sessions = parse_sessions(raw);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "webcam-2026-01-01_10-00-00");
        assert_eq!(sessions[0].status, "finalized");
        assert_eq!(sessions[1].status, "chunks only");
    }
}
