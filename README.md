# Webcam / Screen Recorder

A small Linux recorder built on FFmpeg. It has two modes:

- **Webcam** — records a camera with a floating live preview.
- **Screen** — records the full screen with the camera as a picture-in-picture overlay.

Recordings are written as crash-safe chunks and finalized into a single MP4.

## Features

- Webcam-only and full-screen recording, controlled by hotkeys or CLI.
- Hardware-accelerated HEVC/H.264 encoding (NVENC) and MJPG decoding (NVDEC)
  when an NVIDIA GPU is available.
- Software AV1 (SVT-AV1, low-delay) and H.264 (x264) fallbacks.
- Floating preview: borderless, always-on-top, and sticky across XFCE
  workspaces.
- Microphone, system audio, or a mix of both.
- 10-minute Matroska chunks; a crash loses at most the current chunk.
- Lossless remux to a single faststart MP4 on stop.
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
| `Super+F9` | toggle webcam recording + preview |
| `Super+F10` | toggle screen recording + camera PiP |

```bash
rec start                       # webcam + preview
rec screen start                # screen + camera PiP
rec screen start --audio=mic    # or both / none (default: system)
rec stop                        # stop the active recording
rec status                      # mode, elapsed, chunks, free space, preview
rec list                        # recent sessions
rec open                        # open the recordings directory
rec preview start|stop|status   # screen-mode preview control
rec finalize <session-dir>      # recover an interrupted session
rec path | config               # print the recordings/config paths
```

The two modes are mutually exclusive. Pressing the other hotkey while one is
recording leaves the active session untouched.

## Desktop UI (optional)

`ui/` contains a small Rust/egui control panel that drives the same `rec` CLI.
It shows the live state, starts and stops both modes, selects screen audio, opens
recordings, and finalizes chunk-only sessions.

Build and run:

```bash
cd ui
cargo build --release
./target/release/recorder-ui
```

The UI finds `rec` in `PATH`; set `REC_BIN=/path/to/bin/rec` to override. It does
not replace the CLI or the hotkeys, and the camera preview remains a separate
window controlled by the recorder.

## Configuration

Settings live in `rec.env`. The most important options:

| Key | Meaning |
|---|---|
| `DEVICE` | V4L2 capture device |
| `INPUT_FORMAT`, `VIDEO_SIZE`, `FRAMERATE` | Camera format; must match a mode the camera supports |
| `AUDIO_SOURCE` | Microphone source; `default` uses the system default input |
| `AUDIO_MONITOR` | System-audio source; `auto` derives the monitor of the default sink |
| `VIDEO_CODEC` | `hevc_nvenc` (default, NVIDIA), `libsvtav1` (CPU), `h264_nvenc`, `libx264` |
| `GPU_DECODE` | `1` uses NVDEC for NVENC modes; `0` uses CPU MJPG decode |
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
  a second output of the same process, delivered through a rawvideo FIFO to
  `ffplay`.
- **Screen mode** runs FFmpeg on `x11grab` and a separate `ffplay` preview. The
  preview window is visible on screen, so it is captured as the PiP. This is
  intentional: X11 full-screen capture cannot exclude a window.
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
- `rec finalize <dir>` is idempotent and can recover a session after a crash.

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
- **Hotkeys do nothing** — run `bin/setup-hotkeys.sh` in an XFCE session, or bind
  `bin/rec toggle` / `bin/rec screen toggle` manually.
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
bin/setup-hotkeys.sh     bind Super+F9/F10 in XFCE
bin/setup-hotkey.sh      bind a single shortcut
rec.env.example          configuration template
ui/                      Rust/egui desktop control panel
SETUP.md                 setup walkthrough
README.md                this file
```

Recordings are written under `OUTPUT_DIR` and are not part of the repository.
