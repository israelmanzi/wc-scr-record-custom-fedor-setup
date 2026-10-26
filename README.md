# Webcam / Screen Recorder

A small Linux recorder built on FFmpeg. It has two modes:

- **Webcam** — records a camera with a floating live preview.
- **Screen** — records the full screen with the camera as a picture-in-picture overlay.

Recordings are written as crash-safe chunks and finalized into a single MP4.

## Features

- Webcam-only and full-screen recording, controlled by the desktop UI, a
  launcher hotkey, or the CLI.
- Hardware-accelerated HEVC/H.264 encoding (NVENC) and MJPG decoding (NVDEC)
  when an NVIDIA GPU is available.
- Software AV1 (SVT-AV1, low-delay) and H.264 (x264) fallbacks.
- Floating preview: borderless and always on top. When the desktop UI is
  running, the preview is embedded in its window; otherwise the CLI uses a
  separate ffplay window.
- Microphone, system audio, or a mix of both.
- 10-minute Matroska chunks; a crash loses at most the current chunk.
- Lossless remux to a single faststart MP4 on stop.
- Pause and resume without recording the paused interval.
- Disk-space guard, sleep inhibitor, restart and stall watchdogs,
  single-instance locking, and graceful shutdown.
- Short desktop notifications.

## Requirements

- Linux with an X11 session (screen capture uses `x11grab`).
- FFmpeg with the encoders/filters you intend to use. The defaults need
  `libsvtav1`, `h264_nvenc`, `hevc_nvenc`, and `mjpeg_cuvid`.
- PulseAudio or PipeWire (`pulse` input).
- `bash`, `v4l2-utils`, `xdotool`, `xprop`, `fuser` (psmisc), `flock`
  (util-linux), `notify-send`, `systemd-inhibit`, `ffplay`.
- Optional: XFCE for the provided hotkey setup scripts. The CLI works without
  the hotkeys.
- Optional: a Rust toolchain to build the desktop UI.

## Install

```bash
git clone <repository-url>
cd webcam-screen-recorder

cp rec.env.example rec.env
$EDITOR rec.env                 # choose camera, audio sources, output directory

mkdir -p ~/.local/bin
ln -sf "$PWD/bin/rec" ~/.local/bin/rec

# XFCE only:
bin/setup-hotkeys.sh
```

`rec` can now be used from any shell. See [SETUP.md](SETUP.md) for the detailed
configuration walkthrough.

## Usage

| Hotkey | Action |
|---|---|
| `Super+F9` | show/focus the desktop launcher (starts it if needed) |

```bash
rec start                       # webcam + preview
rec start --name "Team standup" # custom session name
rec screen start                # screen + camera PiP
rec screen start --audio=mic    # or both / none (default: system)
rec screen start --name "Demo"  # custom session name
rec pause                       # pause the active recording
rec resume                      # resume the active recording
rec stop                        # stop the active recording
rec status                      # mode, elapsed, chunks, free space, preview
rec list                        # recent sessions
rec open                        # open the recordings directory
rec values                      # print the effective configuration values
rec preview start|stop|status   # screen-mode preview control
rec finalize <session-dir>      # recover an interrupted session
rec path | config               # print the recordings/config paths
```

The two modes are mutually exclusive. Starting the other mode while one is
recording leaves the active session untouched.

## Desktop UI (optional)

`ui/` contains a small Rust/egui control panel that drives the same `rec` CLI.
The window is both the launcher and the live camera preview, so recording uses
only one window.

- The main view has the preview, the recording buttons, the elapsed time, the
  session name, the start timer (0/3/5/10 s), and Menu/Exit buttons.
- The session name is auto-generated while idle and can be edited before
  starting. It is used for the session folder and supports spaces.
- Menu opens a separate window. History lists recent sessions with full names,
  opens the recordings folder, and opens or recovers sessions.
- Settings edits every option in `rec.env` with real controls
  (switches, choices, and sliders). It detects camera devices and their
  formats, resolutions and frame rates, PulseAudio sources, and available
  FFmpeg encoders. Unsupported camera modes are excluded, dependent encoder
  fields are disabled, and detected mode changes need Save. Use Refresh
  hardware after changing cameras or audio devices. Changes apply to the next
  recording; preview resolution applies after restarting the app.
- While the UI is running, recordings started from the UI or the CLI feed the
  UI preview. The screen-mode recording captures the UI window as the PiP.
- While recording, the launcher shows Pause/Resume and Stop buttons. The paused
  view keeps the last frame (or live camera preview) with a Paused overlay, and
  elapsed time tracks active recording time only.
- The launcher is always on top.
- With a non-zero timer, the launcher counts down before recording starts. One
  global timer setting is shared by recording start and resume; it can also be
  changed while paused.
- `Super+F9` shows and focuses the launcher; it does not start a recording.
- Closing the window stops and finalizes an active recording.
- Launch the UI with `PREVIEW_SINK=ffplay` to force the separate ffplay
  preview for UI-started recordings.

Build and run:

```bash
cd ui
cargo build --release
./target/release/recorder-ui
```

The UI finds `rec` in `PATH`; set `REC_BIN=/path/to/bin/rec` to override.

## Configuration

Settings live in `rec.env`. The most important options:

| Key | Meaning |
|---|---|
| `DEVICE` | V4L2 capture device |
| `INPUT_FORMAT`, `VIDEO_SIZE`, `FRAMERATE` | Camera format; must match a mode the camera supports |
| `AUDIO_SOURCE` | Microphone source; `default` uses the system default input |
| `AUDIO_MONITOR` | System-audio source; `auto` derives the monitor of the default sink |
| `VIDEO_CODEC` | `hevc_nvenc` (default, NVIDIA), `libsvtav1` (CPU), `h264_nvenc`, `libx264` |
| `GPU_DECODE` | `1` uses NVDEC for MJPG input with NVENC; `0` uses CPU decode. Non-MJPG input always uses CPU decode |
| `NVENC_CQ`, `NVENC_PRESET` | NVENC quality and effort |
| `CRF`, `PRESET`, `SVT_PRED_STRUCT` | SVT-AV1 quality and prediction structure |
| `SCREEN_CODEC`, `SCREEN_CQ`, `SCREEN_PRESET` | Screen encoder |
| `SCREEN_AUDIO` | `system`, `mic`, `both`, or `none` |
| `SCREEN_SIZE` | `auto` or an explicit `WxH` |
| `PREVIEW_WIDTH`, `PREVIEW_HEIGHT`, `PREVIEW_MARGIN` | Floating preview geometry |
| `SEGMENT_SECONDS` | Chunk length |
| `MIN_FREE_GB` | Stop recording below this free-space reserve |
| `MIC_GAIN`, `MIC_BOOST`, `MIC_CAPTURE` | Optional ALSA mic-gain enforcement |
| `INHIBIT` | Hold a sleep inhibitor while recording |
| `KEEP_PARTS` | Keep the MKV chunks after finalizing |

## How it works

```
camera (V4L2/MJPG) ─┐
                    ├─► encode ─► MKV chunks ─► concat/remux ─► MP4
microphone/audio ───┘

screen (x11grab) ──► encode ─► MKV chunks ─► concat/remux ─► MP4
camera (preview) ──► visible in the captured screen (PiP)
```

- **Webcam mode** runs one FFmpeg process that owns the camera. The preview is
  a second output of the same process. It is written to the desktop UI through a
  rawvideo FIFO, or to a separate ffplay window when the UI is not running.
- **Screen mode** runs FFmpeg on `x11grab`. With the desktop UI, the UI window
  shows the camera and is itself the visual PiP. Without the UI, a separate
  ffplay window is placed on screen and captured. This is intentional: X11
  full-screen capture cannot exclude a window.
- **Chunks** are finalized by the segment muxer. On stop, the chunks are
  stream-copied into an MP4 with the index at the front. No video is re-encoded.
- **The camera is exclusive.** A second process cannot open the same V4L2
  device while the recorder is running.

## Audio

- Webcam mode records the microphone.
- Screen mode records system audio by default. Use `--audio=mic` for narration
  or `--audio=both` to mix both.
- Use headphones when the microphone is enabled, otherwise the microphone will
  re-record the speakers.
- `MIC_GAIN=1` applies the configured ALSA capture values before every take.
  The control names are hardware-specific; leave `MIC_GAIN=0` if they do not
  apply to your machine.
- Audio is captured raw; no noise suppression or high-pass is applied.

## Reliability

- Chunks are flushed roughly once per second, so a hard crash loses about the
  last second.
- If FFmpeg exits unexpectedly, the supervisor restarts it into the same
  session.
- A progress watchdog restarts FFmpeg and the preview if the current chunk
  stops growing.
- A disk watchdog stops the recording cleanly below `MIN_FREE_GB`.
- A `systemd-inhibit` lock prevents idle suspend while recording.
- `flock` serializes starts and stops; session directories get a suffix if two
  starts land in the same second.
- Stops are graceful: `rec stop` writes `q` to FFmpeg through a control FIFO;
  SIGINT is the fallback.
- Pause stops FFmpeg cleanly and resume starts a new part in the same session.
  Finalization concatenates the parts, so paused time is not in the MP4.
- `rec finalize <dir>` is idempotent and can recover a session after a crash.
- When the UI preview is active, the recording stops cleanly if the UI exits;
  closing the window also stops an active recording.

## Compatibility

- **HEVC** is the default webcam/screen codec. It is supported by Kdenlive,
  Shotcut, VLC, mpv, and YouTube. Some editors and upload targets are more
  restrictive; use `h264_nvenc` or `libx264` for maximum compatibility.
- **AV1** (`libsvtav1`) is available as a CPU fallback. It is smaller than
  H.264 but requires an AV1-capable editor/player.
- The GPU webcam path outputs full-range BT.470BG, tagged with
  `hevc_metadata`/`h264_metadata`. If an editor mishandles full-range video, set
  `GPU_DECODE=0` for standard TV-range BT.709.
- NVIDIA Ampere and newer support HEVC/H.264 NVENC and MJPG NVDEC, but not AV1
  encoding.

## Troubleshooting

- **Camera unavailable** — another application holds the V4L2 device. Close it
  and retry.
- **No audio** — check `pactl list short sources` and set `AUDIO_SOURCE` or
  `AUDIO_MONITOR` accordingly.
- **Hotkey does nothing** — run `bin/setup-hotkeys.sh` in an XFCE session, or bind
  `bin/rec-ui-show` manually.
- **GPU encoder error** — set `GPU_DECODE=0`, or use
  `VIDEO_CODEC="libsvtav1"`.
- **Washed-out or crushed video in an editor** — the GPU webcam path is
  full-range; enable the editor's full-range handling or set
  `GPU_DECODE=0`.
- **Interrupted recording** — run `rec finalize <session-dir>`.

Logs are in each session's `recorder.log`; supervisor failures are in
`supervisor.log`.

## Project layout

```
bin/rec                  control CLI
bin/rec-supervisor       recording loop, watchdogs, restarts
bin/rec-preview          screen-mode preview window
bin/rec-finalize         chunk concatenation and MP4 remux
bin/rec-ui-show           show/focus the desktop launcher
bin/setup-hotkeys.sh     bind Super+F9 in XFCE
bin/setup-hotkey.sh      bind a single shortcut
rec.env.example          configuration template
ui/                      Rust/egui desktop control panel
SETUP.md                 setup walkthrough
README.md                this file
```

The desktop UI source is split between `ui/src/main.rs` (window, preview, and
control flow) and `ui/src/config.rs` (the `rec.env` schema and editor).

Recordings are written under `OUTPUT_DIR` and are not part of the repository.
