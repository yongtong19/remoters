# Remoters on Ubuntu / Linux

Remoters runs on Ubuntu 22.04 LTS and Ubuntu 24.04 LTS (both viewer and host modes) using GPUI's Vulkan/Blade renderer, `ffmpeg` for H.264/HEVC video encode and decode, `xcap` for display capture, `enigo` (X11/XTest) for remote mouse/keyboard injection, PipeWire/PulseAudio (`pw-cat`, `pacat`, `parec`, `pactl`) for low-latency audio, `wl-clipboard`/`xclip`/`xsel` for clipboard sync, and Linux `evdev` + `/dev/uinput` for USB/Bluetooth gamepads.

## 1. Install Ubuntu Build & Runtime Dependencies

```bash
sudo apt-get update
sudo apt-get install -y \
  build-essential \
  pkg-config \
  cmake \
  ffmpeg \
  libxkbcommon-dev \
  libxkbcommon-x11-dev \
  libwayland-dev \
  libvulkan-dev \
  libegl-dev \
  libgbm-dev \
  libpipewire-0.3-dev \
  libdrm-dev \
  libclang-dev \
  libasound2-dev \
  libxcb1-dev \
  libxcb-shape0-dev \
  libxcb-xfixes0-dev \
  libssl-dev \
  pipewire-bin \
  pulseaudio-utils \
  xclip \
  wl-clipboard
```

Install Rust (1.100 or newer per workspace `rust-version`):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 2. Build

From the repository root:

```bash
./packaging/linux/ubuntu-build.sh
```

Or run Cargo directly:

```bash
cargo build --release --locked -p remoters
```

The compiled binary is written to `target/release/remoters`.

To install system-wide with the desktop launcher:

```bash
sudo install -Dm755 target/release/remoters /usr/local/bin/remoters
sudo install -Dm644 packaging/linux/remoters.desktop /usr/local/share/applications/remoters.desktop
```

## 3. Runtime Notes (X11 vs Wayland, Video, Audio, Clipboard)

- **Video (`ffmpeg`)**: Ensure `ffmpeg` is on `PATH`. Remoters automatically probes NVIDIA **NVENC** (`av1_nvenc` / `hevc_nvenc` / `h264_nvenc`, including 10-bit HDR10 `p010le`) and **CUVID** (`av1_cuvid` / `hevc_cuvid` / `h264_cuvid`) on NVIDIA GPUs (verified on Ubuntu 26.04 + RTX 4080), falling back cleanly to software (`libx264` / `libx265` / `libsvtav1` / `libdav1d`). You can override encoder/decoder selection with:
  ```bash
  REMOTERS_LINUX_ENCODER=nvenc|vaapi|software remoters
  REMOTERS_LINUX_DECODER=cuvid|software remoters
  ```
- **X11 vs Wayland**:
  - GPUI renders on both Wayland and X11 via Vulkan (`libvulkan1`, `mesa-vulkan-drivers`).
  - Remote mouse/keyboard host control uses `enigo` (`x11rb` / XTest). For full desktop control across all applications on Ubuntu, use an **Ubuntu on Xorg** session (or XWayland). On Wayland-native compositors, pointer/keyboard injection into native Wayland windows may be restricted by the compositor.
- **Audio**:
  - Viewer playback uses `pw-cat` (PipeWire) and falls back automatically to `pacat` (`pulseaudio-utils`).
  - Host system audio loopback uses `parec` against `@DEFAULT_MONITOR@` (or the default sink's `.monitor` source from `pactl`).
- **Clipboard**:
  - Uses `wl-copy` / `wl-paste` on Wayland (`WAYLAND_DISPLAY` set) and `xclip` / `xsel` on X11 (`DISPLAY` set), supporting both text and PNG/image clipboard sync.

## 4. Virtual Xbox 360 Gamepad Hosting (`/dev/uinput` udev rule)

Viewer gamepad capture reads USB and Bluetooth controllers from `/dev/input/event*`.
Host virtual Xbox 360 controller injection creates virtual devices through `/dev/uinput`.

By default, `/dev/uinput` is only writable by `root`. To allow your user account to host virtual controllers without running Remoters as root:

1. Ensure the `uinput` kernel module is loaded at boot:
   ```bash
   echo uinput | sudo tee /etc/modules-load.d/uinput.conf
   sudo modprobe uinput
   ```
2. Add a udev rule granting `input` group access to `/dev/uinput`:
   ```bash
   echo 'KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"' \
     | sudo tee /etc/udev/rules.d/99-remoters-uinput.rules
   ```
3. Add your user to the `input` group and reload udev:
   ```bash
   sudo usermod -aG input "$USER"
   sudo udevadm control --reload-rules
   sudo udevadm trigger
   ```
4. Log out and log back in for the new group membership to take effect.
