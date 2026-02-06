mod config;
mod frame_diff;
mod input;
mod kms;
mod vnc;

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch};

use config::Config;
use kms::capture;
use kms::fbdev::FbdevCapture;
use vnc::server::{self, InputEvent};

/// A boxed capture function: each call returns one BGRA frame, or `None` if unchanged.
/// If `force` is true, always capture regardless of whether the framebuffer changed.
type CaptureFn = Box<dyn FnMut(bool) -> Result<Option<Vec<u8>>> + Send>;

/// Try to set up DRM capture for a specific card path.
fn try_drm_capture(path: &str) -> Result<(u32, u32, Vec<u8>, CaptureFn)> {
    let (card, outputs) = capture::open_card_path(path)?;
    let output = &outputs[0];
    let width = output.width;
    let height = output.height;
    tracing::info!("Output: {} ({}x{})", output.connector_name, width, height);
    let mut capturer = capture::Capturer::new(card, output);
    let initial_data = capturer
        .capture(true)?
        .expect("first capture must produce a frame");
    let capture_fn: CaptureFn = Box::new(move |force| capturer.capture(force));
    Ok((width, height, initial_data, capture_fn))
}

/// Try to set up fbdev capture for a specific device path.
fn try_fbdev_capture(path: &str) -> Result<(u32, u32, Vec<u8>, CaptureFn)> {
    let fbdev = FbdevCapture::open(path)?;
    let width = fbdev.width();
    let height = fbdev.height();
    let initial_data = fbdev.capture_frame()?;
    let capture_fn: CaptureFn = Box::new(move |_force| Ok(Some(fbdev.capture_frame()?)));
    Ok((width, height, initial_data, capture_fn))
}

/// Set up capture with fallback chain: DRM (PRIME/dumb) -> fbdev.
fn setup_capture(config: &Config) -> Result<(u32, u32, Vec<u8>, CaptureFn)> {
    if let Some(ref path) = config.device {
        // User specified a device — try as DRM first, then as fbdev
        match try_drm_capture(path) {
            Ok(result) => return Ok(result),
            Err(drm_err) => {
                tracing::debug!("DRM capture failed for {path}: {drm_err}");
                match try_fbdev_capture(path) {
                    Ok(result) => return Ok(result),
                    Err(fb_err) => {
                        bail!("Cannot use {path} as DRM ({drm_err:#}) or fbdev ({fb_err:#})");
                    }
                }
            }
        }
    }

    // Auto-detect: try all DRM cards first
    match capture::open_card() {
        Ok((card, outputs)) => {
            let output = &outputs[0];
            let width = output.width;
            let height = output.height;
            tracing::info!("Output: {} ({}x{})", output.connector_name, width, height);
            let mut capturer = capture::Capturer::new(card, output);
            let initial_data = capturer
                .capture(true)?
                .expect("first capture must produce a frame");
            let capture_fn: CaptureFn = Box::new(move |force| capturer.capture(force));
            return Ok((width, height, initial_data, capture_fn));
        }
        Err(drm_err) => {
            tracing::debug!("DRM auto-detect failed: {drm_err}");
        }
    }

    // Fall back to fbdev
    let mut fb_entries: Vec<_> = fs::read_dir("/dev")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with("fb")))
        .collect();
    fb_entries.sort_by_key(|e| e.file_name());

    for entry in &fb_entries {
        let path = entry.path();
        let path_str = path.to_string_lossy();
        match try_fbdev_capture(&path_str) {
            Ok(result) => return Ok(result),
            Err(e) => {
                tracing::debug!("fbdev {path_str} failed: {e}");
            }
        }
    }

    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<binary>".into());
    bail!(
        "No usable capture device found. Tried all /dev/dri/card* (DRM) \
         and /dev/fb* (fbdev). Ensure a display is active and the process \
         has CAP_SYS_ADMIN (try: sudo setcap cap_sys_admin+ep {exe})"
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::parse();

    check_permissions();

    let (width, height, initial_data, capture_fn) = setup_capture(&config)?;

    // Frame channel: latest full BGRA buffer
    let (frame_tx, frame_rx) = watch::channel(Arc::new(initial_data));

    // Capture request channel: VNC clients signal when they need a frame
    let (capture_req_tx, capture_req_rx) = std_mpsc::channel::<()>();

    // Input event channel
    let (input_tx, mut input_rx) = mpsc::channel::<InputEvent>(256);

    // Shutdown flag for the capture loop
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_capture = shutdown.clone();

    // Spawn capture loop (on-demand, driven by client requests)
    let capture_handle = tokio::task::spawn_blocking(move || {
        capture_loop(capture_fn, frame_tx, capture_req_rx, shutdown_capture)
    });

    // Spawn input handler
    let input_handle = tokio::spawn(async move { input_loop(&mut input_rx, width, height).await });

    // Share password across client tasks
    let password = Arc::new(config.password);

    // VNC server listen loop
    let addr = format!("{}:{}", config.listen, config.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind to {addr}"))?;
    tracing::info!("VNC server listening on {addr}");

    // Graceful shutdown on Ctrl+C
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutting down...");
        let _ = shutdown_tx.send(()).await;
    });

    loop {
        tokio::select! {
            accept = listener.accept() => {
                let (stream, peer) = accept?;
                tracing::info!("VNC client connected: {peer}");
                let frame_rx = frame_rx.clone();
                let capture_req_tx = capture_req_tx.clone();
                let input_tx = input_tx.clone();
                let password = password.clone();
                let w = width as u16;
                let h = height as u16;
                tokio::spawn(async move {
                    if let Err(e) = server::handle_client(stream, w, h, frame_rx, capture_req_tx, input_tx, password.as_deref()).await {
                        tracing::info!("Client {peer} disconnected: {e}");
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                break;
            }
        }
    }

    // Signal capture loop to stop and wait for it
    shutdown.store(true, Ordering::Relaxed);
    drop(input_tx);
    input_handle.abort();
    let _ = capture_handle.await;

    Ok(())
}

/// Adaptive capture mode: switches between on-demand and polling based on request frequency.
enum CaptureMode {
    /// Wait for explicit capture requests; always force-capture to ensure fresh frames.
    OnDemand,
    /// Actively poll at the given interval; skip unchanged frames to save CPU.
    Polling { interval: Duration },
}

fn capture_loop(
    mut capture_fn: CaptureFn,
    frame_tx: watch::Sender<Arc<Vec<u8>>>,
    capture_req_rx: std_mpsc::Receiver<()>,
    shutdown: Arc<AtomicBool>,
) {
    let mut mode = CaptureMode::OnDemand;
    let mut last_request_time: Option<Instant> = None;
    let mut fast_request_count = 0u32;

    loop {
        let timeout = match mode {
            CaptureMode::OnDemand => Duration::from_millis(100),
            CaptureMode::Polling { interval } => interval,
        };

        match capture_req_rx.recv_timeout(timeout) {
            Ok(()) => {
                // Check request interval to detect high-frequency clients
                let now = Instant::now();
                if let Some(last) = last_request_time {
                    if now.duration_since(last) < Duration::from_millis(100) {
                        fast_request_count += 1;
                        if fast_request_count >= 3 {
                            if matches!(mode, CaptureMode::OnDemand) {
                                tracing::debug!("Switching to polling mode");
                            }
                            mode = CaptureMode::Polling {
                                interval: Duration::from_millis(16), // ~60fps
                            };
                        }
                    } else {
                        fast_request_count = 0;
                    }
                }
                last_request_time = Some(now);

                // Drain any additional queued requests (coalesce)
                while capture_req_rx.try_recv().is_ok() {}

                // Client request: always force=true to guarantee a fresh frame
                do_capture(&mut capture_fn, &frame_tx, true);
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                match mode {
                    CaptureMode::Polling { .. } => {
                        // Check if we should switch back to on-demand
                        if let Some(last) = last_request_time {
                            if Instant::now().duration_since(last) > Duration::from_millis(500) {
                                tracing::debug!("Switching to on-demand mode");
                                mode = CaptureMode::OnDemand;
                                fast_request_count = 0;
                            } else {
                                // Continue polling: capture periodically
                                do_capture(&mut capture_fn, &frame_tx, false);
                            }
                        }
                    }
                    CaptureMode::OnDemand => {
                        // Just check for shutdown
                        if shutdown.load(Ordering::Relaxed) {
                            tracing::debug!("Capture loop shutting down");
                            break;
                        }
                    }
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                tracing::debug!("Capture request channel closed");
                break;
            }
        }
    }
}

/// Perform a capture and send the result if a new frame was obtained.
fn do_capture(
    capture_fn: &mut CaptureFn,
    frame_tx: &watch::Sender<Arc<Vec<u8>>>,
    force: bool,
) {
    match capture_fn(force) {
        Ok(Some(data)) => {
            let _ = frame_tx.send(Arc::new(data));
        }
        Ok(None) => {
            // Frame unchanged — don't notify (polling mode only; next capture comes soon)
        }
        Err(e) => {
            tracing::warn!("Capture failed: {e}");
        }
    }
}

async fn input_loop(input_rx: &mut mpsc::Receiver<InputEvent>, width: u32, height: u32) {
    let mut touch = match input::touch::VirtualTouchscreen::new(width, height) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::warn!("Failed to create virtual touchscreen: {e}");
            tracing::warn!("Touch input will be disabled");
            None
        }
    };

    let keyboard = match input::keyboard::VirtualKeyboard::new() {
        Ok(k) => Some(k),
        Err(e) => {
            tracing::warn!("Failed to create virtual keyboard: {e}");
            tracing::warn!("Keyboard input will be disabled");
            None
        }
    };

    while let Some(event) = input_rx.recv().await {
        match event {
            InputEvent::Pointer { button_mask, x, y } => {
                if let Some(ref mut t) = touch {
                    if let Err(e) = t.handle_pointer(button_mask, x, y) {
                        tracing::warn!("Touch event error: {e}");
                    }
                }
            }
            InputEvent::Key { down, keysym } => {
                if let Some(ref k) = keyboard {
                    if let Err(e) = k.handle_key(down, keysym) {
                        tracing::warn!("Key event error: {e}");
                    }
                }
            }
        }
    }
}

/// Check for required capabilities and permissions, warn early on problems.
fn check_permissions() {
    if !has_cap_sys_admin() {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<binary>".into());
        tracing::warn!(
            "Process lacks CAP_SYS_ADMIN — DRM framebuffer access will likely fail. \
             Run as root or: sudo setcap cap_sys_admin+ep {exe}"
        );
    }

    match std::fs::metadata("/dev/uinput") {
        Ok(meta) => {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/uinput")
            {
                Ok(_) => {}
                Err(_) => {
                    tracing::warn!(
                        "/dev/uinput is not writable — input forwarding will be disabled. \
                         Fix: sudo usermod -aG input $USER (then re-login), \
                         or: sudo chmod 0660 /dev/uinput"
                    );
                }
            }
            let _ = meta;
        }
        Err(_) => {
            tracing::warn!(
                "/dev/uinput does not exist — input forwarding will be disabled. \
                 Fix: sudo modprobe uinput"
            );
        }
    }
}

/// Check whether the current process has CAP_SYS_ADMIN in its effective set.
fn has_cap_sys_admin() -> bool {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return false,
    };
    for line in status.lines() {
        if let Some(hex) = line.strip_prefix("CapEff:\t") {
            let caps = match u64::from_str_radix(hex.trim(), 16) {
                Ok(v) => v,
                Err(_) => return false,
            };
            return (caps & (1 << 21)) != 0;
        }
    }
    false
}
