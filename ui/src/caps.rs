use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoDevice {
    pub path: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct CameraFormat {
    pub fourcc: String,
    pub sizes: BTreeMap<String, Vec<u32>>,
}

#[derive(Clone, Debug, Default)]
pub struct CameraCaps {
    pub formats: BTreeMap<String, CameraFormat>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSource {
    pub name: String,
    pub description: String,
    pub monitor: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Hardware {
    pub devices: Vec<VideoDevice>,
    pub audio_sources: Vec<AudioSource>,
    pub encoders: BTreeSet<String>,
    pub has_nvidia: bool,
    pub has_mjpeg_cuvid: bool,
}

pub fn load_devices() -> Vec<VideoDevice> {
    let mut devices = Command::new("v4l2-ctl")
        .arg("--list-devices")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_devices(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default();

    if devices.is_empty() {
        devices = scan_video_nodes();
    }
    devices
}

pub fn load_camera_caps(device: &str) -> Result<CameraCaps, String> {
    let output = Command::new("v4l2-ctl")
        .args(["-d", device, "--list-formats-ext"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if message.is_empty() {
            "cannot read camera modes".to_string()
        } else {
            message
        });
    }

    let caps = parse_camera_caps(&String::from_utf8_lossy(&output.stdout));
    if caps.formats.is_empty() {
        Err("camera reports no capture modes".to_string())
    } else {
        Ok(caps)
    }
}

pub fn load_audio_sources() -> Vec<AudioSource> {
    let mut sources = Command::new("pactl")
        .args(["list", "sources"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_audio_sources(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default();

    if sources.is_empty() {
        sources = Command::new("pactl")
            .args(["list", "short", "sources"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| parse_short_sources(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default();
    }
    sources
}

pub fn load_hardware(device: &str) -> (Hardware, Result<CameraCaps, String>) {
    let encoders = load_encoders();
    let decoders = load_decoders();
    let hardware = Hardware {
        devices: load_devices(),
        audio_sources: load_audio_sources(),
        has_nvidia: has_nvidia(),
        has_mjpeg_cuvid: decoders.contains("mjpeg_cuvid"),
        encoders,
    };
    (hardware, load_camera_caps(device))
}

pub fn load_encoders() -> BTreeSet<String> {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_codec_list(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

fn load_decoders() -> BTreeSet<String> {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| parse_codec_list(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

pub fn has_nvidia() -> bool {
    Path::new("/dev/nvidia0").exists()
        || Command::new("nvidia-smi")
            .arg("-L")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
}

fn scan_video_nodes() -> Vec<VideoDevice> {
    let Ok(entries) = fs::read_dir("/dev") else {
        return Vec::new();
    };
    let mut devices = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("video") {
            continue;
        }
        let path = format!("/dev/{name}");
        devices.push(VideoDevice {
            name: path.clone(),
            path,
        });
    }
    devices.sort_by(|a, b| a.path.cmp(&b.path));
    devices
}

pub fn parse_devices(text: &str) -> Vec<VideoDevice> {
    let mut devices: Vec<VideoDevice> = Vec::new();
    let mut current_name = None;

    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let indented = line
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace());
        if !indented {
            current_name = Some(line.trim().trim_end_matches(':').to_string());
            continue;
        }

        let Some(name) = &current_name else {
            continue;
        };
        let path = line.trim();
        if !path.starts_with("/dev/video") {
            continue;
        }
        if devices.last().is_some_and(|device| &device.name == name) {
            continue;
        }
        devices.push(VideoDevice {
            path: path.to_string(),
            name: name.clone(),
        });
    }

    for device in &mut devices {
        device.name.push_str(" (");
        device.name.push_str(&device.path);
        device.name.push(')');
    }
    devices
}

pub fn parse_camera_caps(text: &str) -> CameraCaps {
    let mut caps = CameraCaps::default();
    let mut format_key = None;
    let mut size = None;

    for line in text.lines() {
        let trimmed = line.trim();

        if let Some(start) = trimmed.find("]: '") {
            let rest = &trimmed[start + 4..];
            if let Some(end) = rest.find('\'') {
                let fourcc = &rest[..end];
                let key = ffmpeg_format(fourcc);
                caps.formats
                    .entry(key.clone())
                    .or_insert_with(|| CameraFormat {
                        fourcc: fourcc.to_string(),
                        sizes: BTreeMap::new(),
                    });
                format_key = Some(key);
                size = None;
            }
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Size: Discrete ") {
            size = Some(value.trim().to_string());
            if let (Some(format), Some(size)) = (&format_key, &size) {
                if let Some(entry) = caps.formats.get_mut(format) {
                    entry.sizes.entry(size.clone()).or_default();
                }
            }
            continue;
        }

        if let Some(value) = trimmed.strip_prefix("Size: Stepwise ") {
            let end = value.split(" - ").last().unwrap_or(value).trim();
            size = Some(end.to_string());
            if let (Some(format), Some(size)) = (&format_key, &size) {
                if let Some(entry) = caps.formats.get_mut(format) {
                    entry.sizes.entry(size.clone()).or_default();
                }
            }
            continue;
        }

        if trimmed.starts_with("Interval: Discrete ") || trimmed.starts_with("Interval: Stepwise ")
        {
            let (Some(format), Some(size)) = (&format_key, &size) else {
                continue;
            };
            let Some(fps) = parse_fps(trimmed) else {
                continue;
            };
            if let Some(entry) = caps.formats.get_mut(format) {
                let rates = entry.sizes.entry(size.clone()).or_default();
                if !rates.contains(&fps) {
                    rates.push(fps);
                }
            }
        }
    }

    for format in caps.formats.values_mut() {
        for rates in format.sizes.values_mut() {
            rates.sort_unstable();
            rates.dedup();
        }
    }

    caps
}

fn parse_fps(line: &str) -> Option<u32> {
    let open = line.find('(')?;
    let close = line[open..].find(')')?;
    let inside = &line[open + 1..open + close];
    let value = inside.rsplit(" - ").next().unwrap_or(inside);
    let value = value.split("fps").next()?.trim();
    value.parse::<f32>().ok().map(|fps| fps.round() as u32)
}

fn ffmpeg_format(fourcc: &str) -> String {
    match fourcc.to_ascii_uppercase().as_str() {
        "MJPG" => "mjpeg".to_string(),
        "YUYV" => "yuyv422".to_string(),
        "NV12" => "nv12".to_string(),
        "H264" => "h264".to_string(),
        "GREY" => "gray".to_string(),
        "RGB3" => "rgb24".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

pub fn parse_audio_sources(text: &str) -> Vec<AudioSource> {
    let mut sources = Vec::new();
    let mut name = None;
    let mut description = None;

    let flush = |name: &mut Option<String>,
                 description: &mut Option<String>,
                 sources: &mut Vec<AudioSource>| {
        let Some(current) = name.take() else {
            description.take();
            return;
        };
        let description = description
            .take()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| current.clone());
        if !sources
            .iter()
            .any(|source: &AudioSource| source.name == current)
        {
            sources.push(AudioSource {
                monitor: current.ends_with(".monitor"),
                name: current,
                description,
            });
        }
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Source #") {
            flush(&mut name, &mut description, &mut sources);
        } else if let Some(value) = trimmed.strip_prefix("Name: ") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = trimmed.strip_prefix("Description: ") {
            description = Some(value.trim().to_string());
        }
    }
    flush(&mut name, &mut description, &mut sources);
    sources
}

fn parse_short_sources(text: &str) -> Vec<AudioSource> {
    let mut sources = Vec::new();
    for line in text.lines() {
        let Some(name) = line.split('\t').nth(1) else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        sources.push(AudioSource {
            monitor: name.ends_with(".monitor"),
            name: name.to_string(),
            description: name.to_string(),
        });
    }
    sources
}

fn parse_codec_list(text: &str) -> BTreeSet<String> {
    let mut codecs = BTreeSet::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 2 {
            continue;
        }
        if !fields[0].contains('.') {
            continue;
        }
        if fields[1].starts_with('=') {
            continue;
        }
        codecs.insert(fields[1].to_string());
    }
    codecs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_camera_devices() {
        let text = "Integrated Camera: Integrated C (usb-0000:00:14.0-7):\n\t/dev/video0\n\t/dev/video1\n\t/dev/media0\n\nSecond Camera (usb-1):\n\t/dev/video4\n";
        let devices = parse_devices(text);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].path, "/dev/video0");
        assert!(devices[0].name.contains("Integrated Camera"));
        assert_eq!(devices[1].path, "/dev/video4");
    }

    #[test]
    fn parses_camera_formats_and_fps() {
        let text = "ioctl: VIDIOC_ENUM_FMT\n\t[0]: 'MJPG' (Motion-JPEG, compressed)\n\t\tSize: Discrete 1280x720\n\t\t\tInterval: Discrete 0.033s (30.000 fps)\n\t\tSize: Discrete 640x480\n\t\t\tInterval: Discrete 0.033s (30.000 fps)\n\t[1]: 'YUYV' (YUYV 4:2:2)\n\t\tSize: Discrete 1280x720\n\t\t\tInterval: Discrete 0.100s (10.000 fps)\n";
        let caps = parse_camera_caps(text);
        assert_eq!(caps.formats["mjpeg"].fourcc, "MJPG");
        assert_eq!(caps.formats["mjpeg"].sizes["1280x720"], vec![30]);
        assert_eq!(caps.formats["yuyv422"].sizes["1280x720"], vec![10]);
    }

    #[test]
    fn parses_audio_sources_and_monitors() {
        let text = "Source #51\n\tName: easyeffects_sink.monitor\n\tDescription: Monitor of Easy Effects Sink\n\tMonitor of Sink: easyeffects_sink\nSource #52\n\tName: alsa_input.pci\n\tDescription: Built-in Audio Analog Stereo\n\tMonitor of Sink: n/a\n";
        let sources = parse_audio_sources(text);
        assert_eq!(sources.len(), 2);
        assert!(sources[0].monitor);
        assert!(!sources[1].monitor);
        assert_eq!(sources[1].description, "Built-in Audio Analog Stereo");
    }

    #[test]
    fn parses_encoder_names() {
        let text = " V..... libsvtav1            SVT-AV1 encoder\n V....D h264_nvenc          NVIDIA H.264\n ------ = \n";
        let codecs = parse_codec_list(text);
        assert!(codecs.contains("libsvtav1"));
        assert!(codecs.contains("h264_nvenc"));
        assert!(!codecs.contains("="));
    }
}
