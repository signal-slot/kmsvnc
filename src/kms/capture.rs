use std::fs;
use std::os::fd::{AsFd, OwnedFd};
use std::ptr;

use anyhow::{bail, Context, Result};
use drm::control::{connector, crtc, framebuffer, Device as ControlDevice};
use drm_fourcc::{DrmFourcc, DrmModifier};
use rustix::mm::{self, MapFlags, ProtFlags};

use super::card::Card;
use super::pixel_format;

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<binary>".into())
}

/// Active output: connector -> encoder -> CRTC chain.
pub struct ActiveOutput {
    pub connector_name: String,
    pub crtc_handle: crtc::Handle,
    pub width: u32,
    pub height: u32,
    pub fb_handle: framebuffer::Handle,
}

/// Captured frame in BGRA pixel format.
#[allow(dead_code)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// BGRA pixel data, row-major, 4 bytes per pixel, no padding.
    pub data: Vec<u8>,
}

/// Open the first DRI card that has connected outputs.
pub fn open_card() -> Result<(Card, Vec<ActiveOutput>)> {
    let mut entries: Vec<_> = fs::read_dir("/dev/dri")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("card"))
        })
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in &entries {
        let path = entry.path();
        let path_str = path.to_string_lossy();
        let card = match Card::open(&path_str) {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Cannot open {path_str}: {e}");
                continue;
            }
        };

        match probe_outputs(&card) {
            Ok(outputs) if !outputs.is_empty() => {
                tracing::info!(
                    "KMS: using {path_str} with {} active output(s)",
                    outputs.len()
                );
                return Ok((card, outputs));
            }
            Ok(_) => {
                tracing::debug!("{path_str}: no active outputs");
            }
            Err(e) => {
                tracing::debug!("{path_str}: probe failed: {e}");
            }
        }
    }

    bail!(
        "No DRI card with active outputs found. \
         Ensure /dev/dri/card* exists and the process has CAP_SYS_ADMIN \
         (try: sudo setcap cap_sys_admin+ep {})",
        exe_path()
    )
}

/// Open a specific DRI card by path.
pub fn open_card_path(path: &str) -> Result<(Card, Vec<ActiveOutput>)> {
    let card = Card::open(path).with_context(|| format!("Cannot open {path}"))?;
    let outputs = probe_outputs(&card)?;
    if outputs.is_empty() {
        bail!("{path}: no active outputs found");
    }
    tracing::info!("KMS: using {path} with {} active output(s)", outputs.len());
    Ok((card, outputs))
}

fn probe_outputs(card: &Card) -> Result<Vec<ActiveOutput>> {
    let res = card.resource_handles()?;
    let mut outputs = Vec::new();

    for &conn_h in res.connectors() {
        let conn = card.get_connector(conn_h, false)?;
        if conn.state() != connector::State::Connected {
            continue;
        }

        let enc_h = match conn.current_encoder() {
            Some(h) => h,
            None => continue,
        };
        let enc = card.get_encoder(enc_h)?;
        let crtc_h = match enc.crtc() {
            Some(h) => h,
            None => continue,
        };
        let crtc_info = card.get_crtc(crtc_h)?;
        let mode = match crtc_info.mode() {
            Some(m) => m,
            None => continue,
        };
        let fb_h = match crtc_info.framebuffer() {
            Some(h) => h,
            None => continue,
        };

        let (w, h) = mode.size();
        outputs.push(ActiveOutput {
            connector_name: format!("{conn}"),
            crtc_handle: crtc_h,
            width: w as u32,
            height: h as u32,
            fb_handle: fb_h,
        });
    }

    Ok(outputs)
}

/// Capture a single frame from the given output.
pub fn capture_frame(card: &Card, output: &ActiveOutput) -> Result<Frame> {
    // Re-read CRTC to get current framebuffer (may change due to page-flipping)
    let crtc_info = card
        .get_crtc(output.crtc_handle)
        .context("Failed to get CRTC")?;
    let fb_handle = crtc_info.framebuffer().unwrap_or(output.fb_handle);

    // Try GET_FB2 first for pixel format info, fall back to GET_FB
    match capture_fb2(card, fb_handle, output.width, output.height) {
        Ok(frame) => Ok(frame),
        Err(fb2_err) => {
            tracing::debug!("GET_FB2 failed ({fb2_err}), trying GET_FB");
            capture_fb1(card, fb_handle, output.width, output.height)
        }
    }
}

fn capture_fb2(
    card: &Card,
    fb_handle: framebuffer::Handle,
    width: u32,
    height: u32,
) -> Result<Frame> {
    let info = card
        .get_planar_framebuffer(fb_handle)
        .context("GET_FB2 failed")?;

    if let Some(modifier) = info.modifier() {
        if modifier != DrmModifier::Linear {
            bail!(
                "Framebuffer has non-linear modifier ({modifier:?}); \
                 tiled buffers cannot be read via mmap"
            );
        }
    }

    let gem_handle = info.buffers()[0].context("No buffer handle in framebuffer")?;
    let pitch = info.pitches()[0];
    let format = info.pixel_format();
    tracing::debug!(
        "FB2: format={format:?}, pitch={pitch}, modifier={:?}",
        info.modifier()
    );

    let raw = mmap_gem_buffer(card, gem_handle, height, pitch)?;
    let bgra = pixel_format::convert_to_bgra(&raw, width, height, pitch, format)
        .map_err(|e| anyhow::anyhow!(e))?;

    let _ = card.close_buffer(gem_handle);

    Ok(Frame {
        width,
        height,
        data: bgra,
    })
}

fn capture_fb1(
    card: &Card,
    fb_handle: framebuffer::Handle,
    width: u32,
    height: u32,
) -> Result<Frame> {
    let info = card.get_framebuffer(fb_handle).context("GET_FB failed")?;

    let gem_handle = info.buffer().with_context(|| {
        format!(
            "No buffer handle from GET_FB. \
             CAP_SYS_ADMIN is required (try: sudo setcap cap_sys_admin+ep {})",
            exe_path()
        )
    })?;

    let pitch = info.pitch();
    let bpp = info.bpp();
    let depth = info.depth();

    let format = match (bpp, depth) {
        (32, 24) => DrmFourcc::Xrgb8888,
        (32, 32) => DrmFourcc::Argb8888,
        (16, 16) => DrmFourcc::Rgb565,
        _ => {
            let _ = card.close_buffer(gem_handle);
            bail!("Unsupported framebuffer format: {bpp}bpp depth={depth}");
        }
    };

    let raw = mmap_gem_buffer(card, gem_handle, height, pitch)?;
    let bgra = pixel_format::convert_to_bgra(&raw, width, height, pitch, format).map_err(|e| {
        let _ = card.close_buffer(gem_handle);
        anyhow::anyhow!(e)
    })?;

    let _ = card.close_buffer(gem_handle);

    Ok(Frame {
        width,
        height,
        data: bgra,
    })
}

fn mmap_gem_buffer(
    card: &Card,
    gem_handle: drm::buffer::Handle,
    height: u32,
    pitch: u32,
) -> Result<Vec<u8>> {
    match mmap_prime(card, gem_handle, height, pitch) {
        Ok(data) => Ok(data),
        Err(prime_err) => {
            tracing::debug!("PRIME mmap failed ({prime_err}), trying dumb buffer mmap");
            mmap_dumb(card, gem_handle, height, pitch)
        }
    }
}

fn mmap_prime(
    card: &Card,
    gem_handle: drm::buffer::Handle,
    height: u32,
    pitch: u32,
) -> Result<Vec<u8>> {
    let prime_fd: OwnedFd = card
        .buffer_to_prime_fd(gem_handle, drm::RDWR)
        .context("PRIME export failed")?;

    let size = (height as usize) * (pitch as usize);

    let data = unsafe {
        let ptr = mm::mmap(
            ptr::null_mut(),
            size,
            ProtFlags::READ,
            MapFlags::SHARED,
            &prime_fd,
            0,
        )
        .context("PRIME mmap failed")?;

        let slice = std::slice::from_raw_parts(ptr.cast::<u8>(), size);
        let buf = slice.to_vec();
        let _ = mm::munmap(ptr, size);
        buf
    };

    Ok(data)
}

fn mmap_dumb(
    card: &Card,
    gem_handle: drm::buffer::Handle,
    height: u32,
    pitch: u32,
) -> Result<Vec<u8>> {
    let map_result = drm_ffi::mode::dumbbuffer::map(card.as_fd(), u32::from(gem_handle), 0, 0)
        .context("DRM_IOCTL_MODE_MAP_DUMB failed")?;

    let size = (height as usize) * (pitch as usize);

    let data = unsafe {
        let ptr = mm::mmap(
            ptr::null_mut(),
            size,
            ProtFlags::READ,
            MapFlags::SHARED,
            card.as_fd(),
            map_result.offset,
        )
        .context("dumb buffer mmap failed")?;

        let slice = std::slice::from_raw_parts(ptr.cast::<u8>(), size);
        let buf = slice.to_vec();
        let _ = mm::munmap(ptr, size);
        buf
    };

    Ok(data)
}
