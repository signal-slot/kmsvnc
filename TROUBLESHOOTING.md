# Troubleshooting

## `sudo: kmsvnc: command not found`

`sudo` uses a restricted `secure_path` that typically doesn't include `~/.cargo/bin`. Use `$(which kmsvnc)` to resolve the full path before passing it to sudo:

```bash
sudo $(which kmsvnc)
```

## "No DRI card with active outputs found"

- Ensure `/dev/dri/card*` devices exist. If not, check that the GPU driver is loaded (`lsmod | grep drm`).
- The process needs `CAP_SYS_ADMIN` to read framebuffer handles. Run as root or grant the capability:
  ```bash
  sudo setcap cap_sys_admin+ep $(which kmsvnc)
  ```

## "Framebuffer has non-linear modifier"

The GPU is using a tiled/compressed framebuffer layout that cannot be read via mmap. This is common with Intel and AMD GPUs using modifiers like `I915_FORMAT_MOD_Y_TILED`. Possible workarounds:

- Force the compositor to use a linear framebuffer (e.g., `KWIN_DRM_NO_MODIFIERS=1` for KDE)
- Try a different DRM device with `--device /dev/dri/card1`

## Black screen or "Capture failed" in logs

- The CRTC's framebuffer may have changed format or become inaccessible. Run with `RUST_LOG=debug` to see the detected DRM format and modifier.
- If using NVIDIA proprietary drivers, KMS capture may not be supported. Use `nouveau` or a different GPU.

## Touch/keyboard input not working

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

## "PRIME export failed" / dumb buffer fallback

Some DRM drivers (e.g., `simpledrm`, `vkms`) don't support PRIME fd export. kmsvnc automatically falls back to dumb buffer mmap in this case. Run with `RUST_LOG=debug` to see which path is used.

## Using fbdev instead of DRM

If DRM is unavailable (no `/dev/dri/card*` devices, or no GPU driver loaded), kmsvnc falls back to the Linux framebuffer device (`/dev/fb0`). You can also force fbdev explicitly:

```bash
sudo kmsvnc --device /dev/fb0
```

Note: fbdev does not require `CAP_SYS_ADMIN`, but the device file must be readable.

## "Address already in use" when starting

Another process is using port 5900. Either stop it or use a different port:

```bash
kmsvnc --port 5901
```

## Wrong colors in VNC viewer

Run with `RUST_LOG=info` and check the "Client SetPixelFormat" log line. The server should convert pixels to the client's requested format automatically. If colors are still wrong, the DRM framebuffer may be in an unsupported format — check the `FB2: format=...` debug log.
