# Setup

This guide walks through configuring the recorder on a Linux/X11 system.

## 1. Prerequisites

Install the runtime dependencies for your distribution. On Fedora:

```bash
sudo dnf install ffmpeg v4l-utils xdotool xprop psmisc util-linux \
  libnotify systemd
```

You also need PulseAudio or PipeWire. XFCE is required only for the provided
hotkey scripts; the CLI works in any X11 session.

The default webcam codec uses NVIDIA NVENC/NVDEC. Without an NVIDIA GPU, set
`VIDEO_CODEC="libsvtav1"` in `rec.env` (or use `libx264`).

## 2. Clone and configure

```bash
git clone <repository-url>
cd webcam-screen-recorder
cp rec.env.example rec.env
```

Edit `rec.env`:

- `DEVICE` — your V4L2 capture device, normally `/dev/video0`.
- `INPUT_FORMAT`, `VIDEO_SIZE`, `FRAMERATE` — a format the camera supports.
- `AUDIO_SOURCE` — microphone source; `default` is usually correct.
- `AUDIO_MONITOR` — `auto` derives the default sink's monitor for system audio.
- `OUTPUT_DIR` — where recordings are stored.

## 3. Check the camera

```bash
v4l2-ctl --list-devices
v4l2-ctl -d /dev/video0 --list-formats-ext
```

Choose a compressed format for higher frame rates. Many cameras expose MJPEG
and a raw format; the raw format often has a lower maximum frame rate.

## 4. Check audio sources

```bash
pactl list short sources
```

- Use a microphone source for `AUDIO_SOURCE`. `default` follows the system
  default input.
- `AUDIO_MONITOR="auto"` derives `<default sink>.monitor`; to set it explicitly,
  run `pactl get-default-sink` and append `.monitor`.
- With the microphone enabled, use headphones to avoid re-recording speakers.

## 5. Choose the codec

The default configuration uses HEVC NVENC with NVDEC decoding:

```bash
VIDEO_CODEC="hevc_nvenc"
GPU_DECODE=1
```

Alternatives:

| Goal | Configuration |
|---|---|
| CPU AV1 (no NVIDIA required) | `VIDEO_CODEC="libsvtav1"`, `CRF=31`, `PRESET=8` |
| Maximum compatibility | `VIDEO_CODEC="libx264"`, `CRF=20`, `PRESET=veryfast` |
| H.264 hardware | `VIDEO_CODEC="h264_nvenc"` |
| TV-range BT.709 with NVENC | `GPU_DECODE=0` |

NVENC uses `NVENC_CQ` and `NVENC_PRESET`; SVT-AV1 uses `CRF` and `PRESET`.
`GPU_DECODE` applies only to NVENC modes. See the comments in `rec.env.example`
for the full option list.

## 6. Install the command and hotkeys

```bash
mkdir -p ~/.local/bin
ln -sf "$PWD/bin/rec" ~/.local/bin/rec
```

For XFCE:

```bash
bin/setup-hotkeys.sh
```

This binds `Super+F9` to webcam mode and `Super+F10` to screen mode.
`bin/setup-hotkey.sh '<Super>F11' screen` binds a different key. Confirm the
shortcuts in XFCE Settings → Keyboard → Application Shortcuts.

## 7. First recording

```bash
rec start
rec status
rec stop
```

For screen recording:

```bash
rec screen start
rec screen start --audio=mic
rec screen stop
```

The first stop finalizes the chunks into an MP4. Check the result:

```bash
rec list
rec open
```

## 8. Preview window

The preview is a small borderless window, always on top and sticky across
workspaces. Move it with Alt+F7 (XFCE) or your window manager's move shortcut.

- In webcam mode it is only a preview; it is not recorded.
- In screen mode it is part of the captured screen and therefore appears in the
  video as the picture-in-picture bubble.
- If the screen-mode preview is closed, the recording continues. Restart it with
  `rec preview start`.

## 9. Configuration reference

| Key | Purpose |
|---|---|
| `SEGMENT_SECONDS` | Chunk length; lower values reduce crash exposure |
| `MIN_FREE_GB` | Stop when free space drops below this reserve |
| `INHIBIT` | Hold a sleep inhibitor while recording |
| `KEEP_PARTS` | Keep the MKV chunks after the MP4 is verified |
| `PREVIEW_WIDTH`, `PREVIEW_HEIGHT` | Preview/PiP size |
| `SCREEN_AUDIO` | Default screen audio mode |
| `MIC_GAIN`, `MIC_BOOST`, `MIC_CAPTURE` | Optional ALSA gain enforcement |

## 10. Troubleshooting

- **Camera busy** — another process holds the device. Close it and retry.
- **No audio** — verify the source names with `pactl list short sources`.
- **Hotkeys do nothing** — run the setup script in an XFCE session, or bind the
  commands manually.
- **NVENC fails** — set `GPU_DECODE=0` or switch to `libsvtav1`.
- **Color/levels look wrong in an editor** — the GPU path is full-range
  BT.470BG; enable full-range handling or use `GPU_DECODE=0`.
- **Interrupted session** — run `rec finalize <session-dir>`.

Each session directory contains `session-info.txt` and `recorder.log`.

## 11. Desktop UI (optional)

The native control panel lives in `ui/` and drives the same `rec` CLI.

```bash
cd ui
cargo build --release
./target/release/recorder-ui
```

It finds `rec` in `PATH`; set `REC_BIN=/path/to/bin/rec` to override. The UI
shows the live status, starts and stops both modes, selects screen audio, opens
recordings, and finalizes chunk-only sessions. The camera preview remains a
separate window.
