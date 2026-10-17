# emeetctl

**Website and downloads:** https://nicholaswm.github.io/emeetctl/

Open source Linux control for the **EMEET SmartCam S600L** webcam (USB `328f:00ef`): the ring light (RGB + fill light), the image controls and the built-in microphone, with a **GUI**, a **CLI** and an **MCP server** so AI agents can drive the camera too.

The vendor app (EMEET Studio) only exists for Windows and macOS. The light protocol was reverse engineered from a passive USB capture of EMEET Studio 2.0.4 (firmware 1.0.19), with no decompilation; see [Protocol](#protocol).

> Not affiliated with or endorsed by EMEET. "EMEET" is used only to name the supported hardware.
> The UI and messages are in Brazilian Portuguese for now.

## What it does

| | GUI | CLI | MCP |
|---|---|---|---|
| Fill light on/off | ✔ | `emeetctl on/off fill` | `set_light_power` |
| RGB ring on/off | ✔ | `emeetctl on/off rgb` | `set_light_power` |
| Modes: cool, warm, balanced, ambient, rainbow | ✔ | `emeetctl mode …` / `rainbow` | `set_light_mode` |
| **Any** RGB color (the vendor app has 6 presets) | ✔ | `emeetctl color ff0000` | `set_light_color` |
| Light brightness | ✔ | `emeetctl brightness 40` | `set_light_brightness` |
| Image: brightness, contrast, saturation, hue, gamma, gain, sharpness, white balance, exposure, focus, zoom, anti-flicker, backlight | ✔ | `emeetctl image list/get/set/reset` | `list_image_controls`, `set_image_control`, `reset_image_controls` |
| Built-in microphone: gain (0–20 dB), mute, live level meter, make it the default input, who is recording from it | ✔ | `emeetctl mic status/gain/mute/unmute/default/level` | `get_mic_status`, `set_mic_gain`, `set_mic_mute`, `set_mic_default` |
| Live camera preview (MJPEG 1280×720, YUYV fallback; released when paused) | ✔ | | |
| Device info (serial, firmware) | ✔ | `emeetctl info` | `get_status` |

Firmware update is deliberately **not** supported. The vendor HID channel may carry firmware commands, so `emeetctl` refuses to send any opcode that was not observed in the capture.

## Install

Requires Rust 1.81+ and the ALSA headers (`sudo apt install libasound2-dev`). The microphone status and meter use PipeWire (`pw-dump`, `pw-record`, `wpctl`); gain and mute work with plain ALSA.

```bash
cargo install --path crates/emeetctl-cli   # emeetctl
cargo install --path crates/emeetctl-gui   # emeetctl-gui
cargo install --path crates/emeetctl-mcp   # emeetctl-mcp
```

The light uses `/dev/hidrawN`, which is root-only by default. Install the udev rule once:

```bash
sudo cp udev/70-emeetctl.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger --subsystem-match=hidraw --action=change
```

The image controls use `/dev/videoN` and work for users in the `video` group (or the logged-in seat).

Optional launcher: `cp emeetctl.desktop ~/.local/share/applications/`.

## CLI

```bash
emeetctl status                    # light state
emeetctl color 123456              # any color; puts the ring in fixed-color mode
emeetctl rainbow
emeetctl mode warm                 # cool | warm | balanced | ambient
emeetctl brightness 30             # % of the current mode's range (--raw 0xNN for the raw value)
emeetctl off rgb                   # rgb (ring) | fill (whole fill light)
emeetctl image list
emeetctl image set focus_automatic_continuous off
emeetctl image set focus_absolute 420
emeetctl image set zoom_absolute 200
emeetctl image reset
emeetctl mic gain 13               # microphone gain in dB
emeetctl mic level                 # live level meter (Ctrl+C to stop)
emeetctl --json status             # machine-readable output
```

## MCP server

`emeetctl-mcp` speaks MCP over stdio. Example for Claude Code:

```bash
claude mcp add emeetctl -- emeetctl-mcp
```

Tools: `get_status`, `set_light_power`, `set_light_color`, `set_light_mode`, `set_light_brightness`, `list_image_controls`, `set_image_control`, `reset_image_controls`, `get_mic_status`, `set_mic_gain`, `set_mic_mute`, `set_mic_default`. Each call opens the device, acts and closes it, so the camera can be unplugged between calls.

## Protocol

32-byte HID reports on the vendor interface (interface 4, interrupt OUT `0x01` / IN `0x84`):

```
09 02 02 OP LL LL LL LL <data> 00…   LL = data length (u16 big-endian, repeated). No checksum.
```

| op | meaning | data |
|---|---|---|
| `00` / `01` | power set / get | `ff 0X 00 00` fill light · `03 01 01 01` / `03 00 00 00` RGB ring |
| `02` / `03` | ring color set / get | `00 RR GG BB` |
| `04` / `05` | brightness set / get | `MM BB` (MM = mode; rainbow uses the ambient key `03`) |
| `06` / `07` | mode set / get | `00 MM` with 0 cool, 1 warm, 2 balanced, 3 ambient, 4 rainbow |

Every SET is acknowledged with a single `20` byte. The camera also pushes state frames on its own when the mode changes, so replies are matched by opcode.

Brightness ranges seen in the vendor slider: ambient/rainbow `0x14–0xff`, cool `0x37–0x64`. Warm and balanced were not measured (assumed equal to cool).

## Camera quirks

- **Digital zoom only applies in MJPEG below 1080p.** Measured frame by frame: zoom works in MJPEG 2560×1440, 1280×960, 1280×720, 1024×576, 960×720, 800×600, 640×480 and 640×360, and is ignored in MJPEG 1920×1080, 3840×2160 and every YUYV mode (EMEET's FAQ also says the field of view is fixed in 4K and 1080p). If zoom "does nothing", check the resolution/format your video app is using.
- Switching the ring to a color or mode may turn the fill light flag on by itself.

## Not mapped yet

The UVC Extension Unit `{46394292-0cd0-4ae3-8783-3133f9eaaa3b}` (10 controls) is still unknown. Candidates from public docs: image flip/mirror, factory reset, microphone noise reduction, boot light behaviour. Contributions with USB captures are welcome.

## Layout

```
crates/emeetctl-core   device discovery (sysfs), HID light protocol, V4L2 controls
crates/emeetctl-cli    `emeetctl`
crates/emeetctl-gui    `emeetctl-gui` (egui)
crates/emeetctl-mcp    `emeetctl-mcp` (MCP over stdio)
udev/                  hidraw permission rule
```

## License

MIT OR Apache-2.0.
