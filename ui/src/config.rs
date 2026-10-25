use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub type Values = BTreeMap<String, String>;

#[derive(Clone, Copy)]
pub enum Kind {
    Text,
    Bool,
    Number { min: i64, max: i64 },
    Choice(&'static [(&'static str, &'static str)]),
}

pub struct Field {
    pub section: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub default: &'static str,
    pub restart: bool,
}

const WEB_CODECS: &[(&str, &str)] = &[
    ("hevc_nvenc", "HEVC (NVENC)"),
    ("h264_nvenc", "H.264 (NVENC)"),
    ("libsvtav1", "AV1 (SVT-AV1, CPU)"),
    ("libx264", "H.264 (x264, CPU)"),
];
const SCREEN_CODECS: &[(&str, &str)] = &[
    ("hevc_nvenc", "HEVC (NVENC)"),
    ("h264_nvenc", "H.264 (NVENC)"),
    ("libx264", "H.264 (x264, CPU)"),
];
const NVENC_PRESETS: &[(&str, &str)] = &[
    ("p1", "p1 (fastest)"),
    ("p2", "p2"),
    ("p3", "p3"),
    ("p4", "p4"),
    ("p5", "p5 (balanced)"),
    ("p6", "p6"),
    ("p7", "p7 (slowest)"),
];
const AUDIO_BITRATES: &[(&str, &str)] = &[
    ("96k", "96 kb/s"),
    ("128k", "128 kb/s"),
    ("160k", "160 kb/s"),
    ("192k", "192 kb/s"),
    ("256k", "256 kb/s"),
];
const SCREEN_AUDIO_BITRATES: &[(&str, &str)] = &[
    ("128k", "128 kb/s"),
    ("160k", "160 kb/s"),
    ("192k", "192 kb/s"),
    ("256k", "256 kb/s"),
    ("320k", "320 kb/s"),
];
const SCREEN_AUDIO_MODES: &[(&str, &str)] = &[
    ("system", "System audio"),
    ("mic", "Microphone"),
    ("both", "System + microphone"),
    ("none", "No audio"),
];
const PRED_STRUCTS: &[(&str, &str)] = &[("1", "Low delay"), ("2", "Random access")];

pub const FIELDS: &[Field] = &[
    Field {
        section: "Capture",
        key: "DEVICE",
        label: "Camera device",
        help: "V4L2 capture device, usually /dev/video0.",
        kind: Kind::Text,
        default: "/dev/video0",
        restart: false,
    },
    Field {
        section: "Capture",
        key: "INPUT_FORMAT",
        label: "Input format",
        help: "Camera pixel format, for example mjpeg or yuyv422.",
        kind: Kind::Text,
        default: "mjpeg",
        restart: false,
    },
    Field {
        section: "Capture",
        key: "VIDEO_SIZE",
        label: "Capture size",
        help: "Camera resolution, for example 1280x720.",
        kind: Kind::Text,
        default: "1280x720",
        restart: false,
    },
    Field {
        section: "Capture",
        key: "FRAMERATE",
        label: "Frame rate",
        help: "Camera frame rate. It must be supported by the selected size and format.",
        kind: Kind::Number { min: 5, max: 60 },
        default: "30",
        restart: false,
    },
    Field {
        section: "Audio",
        key: "AUDIO_ENABLED",
        label: "Record microphone",
        help: "Enable microphone audio in webcam mode.",
        kind: Kind::Bool,
        default: "1",
        restart: false,
    },
    Field {
        section: "Audio",
        key: "AUDIO_SOURCE",
        label: "Microphone source",
        help: "PulseAudio/PipeWire source name; default follows the system input.",
        kind: Kind::Text,
        default: "default",
        restart: false,
    },
    Field {
        section: "Audio",
        key: "AUDIO_BITRATE",
        label: "Microphone bitrate",
        help: "AAC bitrate for webcam audio.",
        kind: Kind::Choice(AUDIO_BITRATES),
        default: "128k",
        restart: false,
    },
    Field {
        section: "Audio",
        key: "MIC_GAIN",
        label: "Enforce microphone gain",
        help: "Apply the ALSA values below before every recording.",
        kind: Kind::Bool,
        default: "0",
        restart: false,
    },
    Field {
        section: "Audio",
        key: "MIC_BOOST",
        label: "Microphone boost",
        help: "ALSA Mic Boost value. Leave at 0 to avoid noise.",
        kind: Kind::Number { min: 0, max: 3 },
        default: "0",
        restart: false,
    },
    Field {
        section: "Audio",
        key: "MIC_CAPTURE",
        label: "Microphone capture",
        help: "Raw ALSA capture value. The useful range is hardware-specific.",
        kind: Kind::Number { min: 0, max: 127 },
        default: "112",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "VIDEO_CODEC",
        label: "Encoder",
        help: "HEVC/H.264 NVENC needs an NVIDIA GPU; AV1 and x264 use the CPU.",
        kind: Kind::Choice(WEB_CODECS),
        default: "hevc_nvenc",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "GPU_DECODE",
        label: "GPU decode",
        help: "Decode MJPG on NVDEC for NVENC encoders. Applies to the next recording.",
        kind: Kind::Bool,
        default: "1",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "NVENC_CQ",
        label: "NVENC quality (CQ)",
        help: "Lower is better quality and a larger file.",
        kind: Kind::Number { min: 0, max: 51 },
        default: "30",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "NVENC_PRESET",
        label: "NVENC preset",
        help: "Encoder effort. p5 is a good balance.",
        kind: Kind::Choice(NVENC_PRESETS),
        default: "p5",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "CRF",
        label: "CPU quality (CRF)",
        help: "Quality for SVT-AV1 and x264. Lower is better.",
        kind: Kind::Number { min: 0, max: 63 },
        default: "31",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "PRESET",
        label: "CPU preset",
        help: "SVT-AV1 preset number (0-13) or x264 preset name.",
        kind: Kind::Text,
        default: "8",
        restart: false,
    },
    Field {
        section: "Webcam codec",
        key: "SVT_PRED_STRUCT",
        label: "AV1 prediction",
        help: "Low delay keeps the audio tail aligned when stopping.",
        kind: Kind::Choice(PRED_STRUCTS),
        default: "1",
        restart: false,
    },
    Field {
        section: "Recording",
        key: "SEGMENT_SECONDS",
        label: "Chunk length",
        help: "Length of each crash-safe MKV chunk, in seconds.",
        kind: Kind::Number { min: 30, max: 3600 },
        default: "600",
        restart: false,
    },
    Field {
        section: "Recording",
        key: "OUTPUT_DIR",
        label: "Recordings folder",
        help: "Where session folders are written. Applies to the next recording.",
        kind: Kind::Text,
        default: "$HOME/Videos/webcam-recorder",
        restart: false,
    },
    Field {
        section: "Recording",
        key: "MIN_FREE_GB",
        label: "Free-space reserve",
        help: "Stop recording when less than this much space remains.",
        kind: Kind::Number { min: 1, max: 1000 },
        default: "20",
        restart: false,
    },
    Field {
        section: "Recording",
        key: "INHIBIT",
        label: "Prevent suspend",
        help: "Hold a systemd idle/sleep inhibitor while recording.",
        kind: Kind::Bool,
        default: "1",
        restart: false,
    },
    Field {
        section: "Recording",
        key: "KEEP_PARTS",
        label: "Keep MKV chunks",
        help: "Keep the chunks after the MP4 has been verified.",
        kind: Kind::Bool,
        default: "0",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_CODEC",
        label: "Encoder",
        help: "Screen capture is usually fastest with NVENC.",
        kind: Kind::Choice(SCREEN_CODECS),
        default: "hevc_nvenc",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_CQ",
        label: "Quality (CQ/CRF)",
        help: "Lower is better quality and a larger file.",
        kind: Kind::Number { min: 0, max: 63 },
        default: "28",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_PRESET",
        label: "NVENC preset",
        help: "Encoder effort for NVENC screen recordings.",
        kind: Kind::Choice(NVENC_PRESETS),
        default: "p5",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_FRAMERATE",
        label: "Frame rate",
        help: "Screen capture frame rate.",
        kind: Kind::Number { min: 5, max: 60 },
        default: "30",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_SIZE",
        label: "Capture size",
        help: "auto follows the current display, or set a size like 1920x1080.",
        kind: Kind::Text,
        default: "auto",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_AUDIO",
        label: "Audio",
        help: "Audio captured with the screen.",
        kind: Kind::Choice(SCREEN_AUDIO_MODES),
        default: "system",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "SCREEN_AUDIO_BITRATE",
        label: "Audio bitrate",
        help: "AAC bitrate for screen audio.",
        kind: Kind::Choice(SCREEN_AUDIO_BITRATES),
        default: "192k",
        restart: false,
    },
    Field {
        section: "Screen recording",
        key: "AUDIO_MONITOR",
        label: "System-audio source",
        help: "auto uses the default sink's monitor. Set a source explicitly if needed.",
        kind: Kind::Text,
        default: "auto",
        restart: false,
    },
    Field {
        section: "Preview",
        key: "PREVIEW_WIDTH",
        label: "Preview width",
        help: "Camera preview stream width. Restart the app to apply.",
        kind: Kind::Number {
            min: 160,
            max: 1920,
        },
        default: "320",
        restart: true,
    },
    Field {
        section: "Preview",
        key: "PREVIEW_HEIGHT",
        label: "Preview height",
        help: "Camera preview stream height. Restart the app to apply.",
        kind: Kind::Number { min: 90, max: 1080 },
        default: "180",
        restart: true,
    },
    Field {
        section: "Preview",
        key: "PREVIEW_MARGIN",
        label: "Screen margin",
        help: "Distance from the screen edge when the window opens.",
        kind: Kind::Number { min: 0, max: 500 },
        default: "20",
        restart: false,
    },
    Field {
        section: "Preview",
        key: "PREVIEW_TITLE",
        label: "CLI preview title",
        help: "Window title used by the command-line ffplay preview.",
        kind: Kind::Text,
        default: "Webcam Preview",
        restart: false,
    },
];

pub fn defaults() -> Values {
    FIELDS
        .iter()
        .map(|field| (field.key.to_string(), field.default.to_string()))
        .collect()
}

pub fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|field| field.key == key)
}

pub fn sections() -> Vec<&'static str> {
    let mut sections = Vec::new();
    for field in FIELDS {
        if !sections.contains(&field.section) {
            sections.push(field.section);
        }
    }
    sections
}

pub fn get<'a>(values: &'a Values, key: &str) -> &'a str {
    values
        .get(key)
        .map(String::as_str)
        .or_else(|| field(key).map(|field| field.default))
        .unwrap_or("")
}

pub fn int(values: &Values, key: &str) -> i64 {
    get(values, key).trim().parse().unwrap_or(0)
}

pub fn bool_value(values: &Values, key: &str) -> bool {
    matches!(
        get(values, key).trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn preview_size(values: &Values) -> (usize, usize) {
    let width = int(values, "PREVIEW_WIDTH").clamp(160, 1920) as usize;
    let height = int(values, "PREVIEW_HEIGHT").clamp(90, 1080) as usize;
    (width, height)
}

pub fn parse_values(raw: &str) -> Values {
    let mut values = Values::new();

    for line in raw.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        values.insert(key.to_string(), value.trim_end_matches('\r').to_string());
    }

    values
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn write_changes(path: &Path, current: &Values, saved: &Values) -> Result<usize, String> {
    let mut changes: Vec<(&'static str, String)> = Vec::new();
    for field in FIELDS {
        let new = get(current, field.key);
        let old = get(saved, field.key);
        if new != old {
            changes.push((field.key, new.to_string()));
        }
    }

    if changes.is_empty() {
        return Ok(0);
    }

    let original = if path.exists() {
        fs::read_to_string(path).map_err(|error| error.to_string())?
    } else {
        String::new()
    };

    let mut lines: Vec<String> = original.lines().map(str::to_string).collect();
    let mut pending: BTreeSet<&str> = changes.iter().map(|(key, _)| *key).collect();

    for line in &mut lines {
        let update: Option<(&'static str, String)> = {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                None
            } else if let Some(equal) = trimmed.find('=') {
                let key = trimmed[..equal].trim();
                changes
                    .iter()
                    .find(|(changed, _)| *changed == key)
                    .map(|(changed, value)| (*changed, value.clone()))
            } else {
                None
            }
        };
        let Some((key, value)) = update else {
            continue;
        };
        *line = format!("{key}={}", shell_quote(&value));
        pending.remove(key);
    }

    for field in FIELDS {
        if !pending.remove(field.key) {
            continue;
        }
        let value = changes
            .iter()
            .find(|(key, _)| *key == field.key)
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();
        lines.push(format!("{}={}", field.key, shell_quote(value)));
    }

    let mut text = lines.join("\n");
    text.push('\n');

    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "rec.env".to_string());
    let temporary = path.with_file_name(format!(".{file_name}.tmp.{}", std::process::id()));
    fs::write(&temporary, text).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())?;

    Ok(changes.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_values_with_equals_and_spaces() {
        let values = parse_values(
            "DEVICE=/dev/video0\nOUTPUT_DIR=/home/me/My Videos\nPREVIEW_TITLE=Webcam Preview\n",
        );
        assert_eq!(get(&values, "DEVICE"), "/dev/video0");
        assert_eq!(get(&values, "OUTPUT_DIR"), "/home/me/My Videos");
        assert_eq!(get(&values, "PREVIEW_TITLE"), "Webcam Preview");
    }

    #[test]
    fn quotes_shell_values() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn writes_only_changed_values_and_preserves_comments() {
        let dir = std::env::temp_dir().join(format!(
            "recorder-ui-config-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rec.env");
        fs::write(
            &path,
            "# keep me\nDEVICE=\"/dev/video0\"\nFRAMERATE=\"30\"   # camera\n",
        )
        .unwrap();

        let saved = defaults();
        let mut current = saved.clone();
        current.insert("FRAMERATE".into(), "24".into());
        current.insert("MIC_GAIN".into(), "1".into());

        let changed = write_changes(&path, &current, &saved).unwrap();
        assert_eq!(changed, 2);

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep me"));
        assert!(text.contains("FRAMERATE='24'"));
        assert!(text.contains("MIC_GAIN='1'"));
        assert!(text.contains("DEVICE=\"/dev/video0\""));

        fs::remove_dir_all(&dir).unwrap();
    }
}
