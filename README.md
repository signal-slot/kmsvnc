# kmsvnc

A lightweight VNC server for Linux that captures the screen directly from the KMS/DRM framebuffer and forwards input via virtual uinput devices. Works independently of any display server — it operates the same whether X11, Wayland, or nothing at all is running.

## Features

- **KMS/DRM screen capture** — reads the GPU framebuffer directly, works without a display server
- **Dumb buffer fallback** — works with simpledrm, vkms, and other drivers that lack PRIME export
- **Linux fbdev fallback** — captures from `/dev/fb*` when DRM is unavailable entirely
- **Minimal RFB protocol** — standard VNC clients (TigerVNC, Remmina, KRDC, etc.) connect out of the box
- **Virtual touch input** — VNC pointer events are translated to Linux multitouch events via uinput
- **Virtual keyboard** — VNC key events are mapped from X11 keysyms to Linux input codes
- **Incremental updates** — 64px tile-based dirty rectangle detection to reduce bandwidth
- **Pixel format negotiation** — respects client `SetPixelFormat` requests (any bpp/endianness/shifts)
- **Multiple DRM formats** — XRGB8888, ARGB8888, XBGR8888, ABGR8888, RGB565

## Requirements

- Linux with KMS/DRM support, or a Linux framebuffer device (`/dev/fb*`)
- `CAP_SYS_ADMIN` capability (for DRM framebuffer access; not needed for fbdev)
- `/dev/uinput` write access (for input forwarding)
- Rust toolchain (to build)

## Building

```bash
cargo build --release
```

## Usage

```bash
# Run as root (simplest)
sudo ./target/release/kmsvnc

# Or grant capabilities
sudo setcap cap_sys_admin+ep ./target/release/kmsvnc
sudo usermod -aG input $USER  # for /dev/uinput access (re-login required)
./target/release/kmsvnc
```

Then connect any VNC client to `localhost:5900`.

### Options

```
--device <path>    Capture device path: /dev/dri/card*, /dev/fb* (default: auto-detect)
--port <port>      VNC listen port (default: 5900)
--fps <fps>        Capture frame rate (default: 30)
--listen <addr>    Listen address (default: 0.0.0.0)
```

### Logging

Control log verbosity with the `RUST_LOG` environment variable:

```bash
RUST_LOG=info sudo ./target/release/kmsvnc    # default useful output
RUST_LOG=debug sudo ./target/release/kmsvnc   # detailed diagnostics
```

## Troubleshooting

### "No DRI card with active outputs found"

- Ensure `/dev/dri/card*` devices exist. If not, check that the GPU driver is loaded (`lsmod | grep drm`).
- The process needs `CAP_SYS_ADMIN` to read framebuffer handles. Run as root or grant the capability:
  ```bash
  sudo setcap cap_sys_admin+ep ./target/release/kmsvnc
  ```

### "Framebuffer has non-linear modifier"

The GPU is using a tiled/compressed framebuffer layout that cannot be read via mmap. This is common with Intel and AMD GPUs using modifiers like `I915_FORMAT_MOD_Y_TILED`. Possible workarounds:

- Force the compositor to use a linear framebuffer (e.g., `KWIN_DRM_NO_MODIFIERS=1` for KDE)
- Try a different DRM device with `--device /dev/dri/card1`

### Black screen or "Capture failed" in logs

- The CRTC's framebuffer may have changed format or become inaccessible. Run with `RUST_LOG=debug` to see the detected DRM format and modifier.
- If using NVIDIA proprietary drivers, KMS capture may not be supported. Use `nouveau` or a different GPU.

### Touch/keyboard input not working

- Check that `/dev/uinput` exists: `ls -l /dev/uinput`. If missing, load the module:
  ```bash
  sudo modprobe uinput
  ```
- Check write permission. Either run as root or add your user to the `input` group:
  ```bash
  sudo usermod -aG input $USER
  ```
  Re-login is required after adding the group.
- Verify the virtual devices were created: `evtest` should list `kmsvnc-touch` and `kmsvnc-keyboard`.

### "PRIME export failed" / dumb buffer fallback

Some DRM drivers (e.g., `simpledrm`, `vkms`) don't support PRIME fd export. kmsvnc automatically falls back to dumb buffer mmap in this case. Run with `RUST_LOG=debug` to see which path is used.

### Using fbdev instead of DRM

If DRM is unavailable (no `/dev/dri/card*` devices, or no GPU driver loaded), kmsvnc falls back to the Linux framebuffer device (`/dev/fb0`). You can also force fbdev explicitly:

```bash
sudo ./target/release/kmsvnc --device /dev/fb0
```

Note: fbdev does not require `CAP_SYS_ADMIN`, but the device file must be readable.

### "Address already in use" when starting

Another process is using port 5900. Either stop it or use a different port:

```bash
./target/release/kmsvnc --port 5901
```

### Wrong colors in VNC viewer

Run with `RUST_LOG=info` and check the "Client SetPixelFormat" log line. The server should convert pixels to the client's requested format automatically. If colors are still wrong, the DRM framebuffer may be in an unsupported format — check the `FB2: format=...` debug log.

## Limitations

- Raw encoding only (no compression — best used on LAN)
- No authentication or encryption
- Uses the first connected display output
- Clipboard forwarding not implemented
