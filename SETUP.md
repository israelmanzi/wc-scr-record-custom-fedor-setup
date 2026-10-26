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

This binds `Super+F9` to `bin/rec-ui-show`, which shows and focuses the
launcher (starting it if needed), and removes the old `Super+F10` binding.
`bin/setup-hotkey.sh '<Super>F11' /path/to/bin/rec-ui-show` binds a different
key. Confirm the shortcut in XFCE Settings → Keyboard → Application Shortcuts.

## 7. First recording

```bash
rec start
rec status
rec pause
rec resume
rec stop

# With a custom session name:
rec start --name "Team standup"
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

The desktop UI embeds the camera preview in its own window. When the UI is not
running, the CLI uses a small borderless ffplay window instead.

- In webcam mode it is only a preview; it is not recorded.
- In screen mode it is visible on screen and therefore appears in the video as
  the picture-in-picture bubble. With the UI, that bubble is the UI window.
- The recording window follows you across workspaces while a recording is
  active.
- If the separate ffplay preview is closed, the recording continues. Restart it
  with `rec preview start`. Closing the desktop UI stops the recording.

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
- **No preview in the UI** — close other camera applications. If the UI is not
  running, the CLI falls back to ffplay.
- **Settings have no effect** — most values apply to the next recording;
  preview width and height apply after restarting the UI.

Each session directory contains `session-info.txt` and `recorder.log`.

## 11. Desktop UI (optional)

The native control panel lives in `ui/` and drives the same `rec` CLI.

```bash
cd ui
cargo build --release
./target/release/recorder-ui
```

The window combines the launcher and the live preview:

- **Buttons** start webcam or screen recording and stop the active one. While
  recording, Pause/Resume is next to Stop.
- **Exit** closes the launcher and finalizes an active recording.
- The idle view shows an editable, auto-generated session name and a
  start timer (0, 3, 5, or 10 seconds). A non-zero timer counts down before
  recording begins.
- Pausing stops the encoder; resuming starts a new part in the same session.
  The paused interval is left out of the final MP4 and the elapsed timer.
- The paused view has its own Resume timer selector (0/3/5/10 seconds) using
  the same timer as recording start; a non-zero value counts down before
  resuming.
- **Menu → History** lists sessions, opens the recordings folder, and opens or
  recovers recordings.
- **Menu → Settings** edits all `rec.env` values with switches, choices, and
  sliders. Save writes the changed values back to `rec.env`; changes apply to
  the next recording. Preview width and height apply after restarting the app.
- The 354×320 launcher is borderless and always on top; drag the preview to
  move it.
- Menu opens in its own resizable window, so it does not enlarge the launcher.
- While the UI is running, CLI recordings use its preview, so there is no second
  window. Set `PREVIEW_SINK=ffplay` to opt out.
- Closing the window stops and finalizes an active recording.

It finds `rec` in `PATH`; set `REC_BIN=/path/to/bin/rec` to override. The app
window and taskbar use the bundled icon in `ui/assets/`.
