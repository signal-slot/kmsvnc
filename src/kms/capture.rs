use std::ffi::c_void;
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

// ---------------------------------------------------------------------------
// Persistent DRM capturer with mmap cache
// ---------------------------------------------------------------------------

const MAX_CACHE_ENTRIES: usize = 4;

struct CachedBuffer {
    fb_key: u32,
    gem_handle: drm::buffer::Handle,
    ptr: *mut c_void,
    size: usize,
    format: DrmFourcc,
    pitch: u32,
    _prime_fd: Option<OwnedFd>,
}

pub struct Capturer {
    card: Card,
    crtc_handle: crtc::Handle,
    default_fb: framebuffer::Handle,
    width: u32,
    height: u32,
    use_fb2: Option<bool>,
    use_prime: Option<bool>,
    cache: Vec<CachedBuffer>,
}

// SAFETY: The mmap pointers in CachedBuffer are read-only and their backing
// resources (prime fd or card fd) are kept alive by Capturer.
unsafe impl Send for Capturer {}

impl Capturer {
    pub fn new(card: Card, output: &ActiveOutput) -> Self {
        Self {
            crtc_handle: output.crtc_handle,
            default_fb: output.fb_handle,
            width: output.width,
            height: output.height,
            use_fb2: None,
            use_prime: None,
            cache: Vec::new(),
            card,
        }
    }

    pub fn capture(&mut self) -> Result<Vec<u8>> {
        let crtc_info = self
            .card
            .get_crtc(self.crtc_handle)
            .context("Failed to get CRTC")?;
        let fb_handle = crtc_info.framebuffer().unwrap_or(self.default_fb);
        let fb_key = u32::from(fb_handle);

        // Cache hit — read directly from persistent mmap
        if let Some(entry) = self.cache.iter().find(|e| e.fb_key == fb_key) {
            let raw =
                unsafe { std::slice::from_raw_parts(entry.ptr.cast::<u8>(), entry.size) };
            return pixel_format::convert_to_bgra(
                raw,
                self.width,
                self.height,
                entry.pitch,
                entry.format,
            )
            .map_err(|e| anyhow::anyhow!(e));
        }

        // Cache miss — map the buffer
        let entry = self.map_buffer(fb_handle)?;
        let raw = unsafe { std::slice::from_raw_parts(entry.ptr.cast::<u8>(), entry.size) };
        let result = pixel_format::convert_to_bgra(
            raw,
            self.width,
            self.height,
            entry.pitch,
            entry.format,
        )
        .map_err(|e| anyhow::anyhow!(e))?;

        // Evict oldest entry if cache is full
        if self.cache.len() >= MAX_CACHE_ENTRIES {
            let evicted = self.cache.remove(0);
            self.evict_entry(evicted);
        }
        self.cache.push(entry);

        Ok(result)
    }

    fn map_buffer(&mut self, fb_handle: framebuffer::Handle) -> Result<CachedBuffer> {
        // Try FB2 first (gives pixel format), latch choice after first success/failure
        match self.use_fb2 {
            Some(true) | None => match self.map_fb2(fb_handle) {
                Ok(entry) => {
                    self.use_fb2 = Some(true);
                    return Ok(entry);
                }
                Err(e) => {
                    if self.use_fb2 == Some(true) {
                        return Err(e);
                    }
                    tracing::debug!("GET_FB2 failed ({e}), trying GET_FB");
                }
            },
            Some(false) => {}
        }

        let entry = self.map_fb1(fb_handle)?;
        self.use_fb2 = Some(false);
        Ok(entry)
    }

    fn map_fb2(&mut self, fb_handle: framebuffer::Handle) -> Result<CachedBuffer> {
        let info = self
            .card
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

        self.map_gem_cached(fb_handle, gem_handle, pitch, format)
    }

    fn map_fb1(&mut self, fb_handle: framebuffer::Handle) -> Result<CachedBuffer> {
        let info = self
            .card
            .get_framebuffer(fb_handle)
            .context("GET_FB failed")?;

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
            _ => bail!("Unsupported framebuffer format: {bpp}bpp depth={depth}"),
        };

        self.map_gem_cached(fb_handle, gem_handle, pitch, format)
    }

    fn map_gem_cached(
        &mut self,
        fb_handle: framebuffer::Handle,
        gem_handle: drm::buffer::Handle,
        pitch: u32,
        format: DrmFourcc,
    ) -> Result<CachedBuffer> {
        let size = (self.height as usize) * (pitch as usize);
        let fb_key = u32::from(fb_handle);

        // Try PRIME first, latch choice after first success/failure
        match self.use_prime {
            Some(true) | None => {
                match self.map_prime_cached(fb_key, gem_handle, size, format, pitch) {
                    Ok(entry) => {
                        self.use_prime = Some(true);
                        return Ok(entry);
                    }
                    Err(e) => {
                        if self.use_prime == Some(true) {
                            return Err(e);
                        }
                        tracing::debug!("PRIME mmap failed ({e}), trying dumb buffer mmap");
                    }
                }
            }
            Some(false) => {}
        }

        let entry = self.map_dumb_cached(fb_key, gem_handle, size, format, pitch)?;
        self.use_prime = Some(false);
        Ok(entry)
    }

    fn map_prime_cached(
        &self,
        fb_key: u32,
        gem_handle: drm::buffer::Handle,
        size: usize,
        format: DrmFourcc,
        pitch: u32,
    ) -> Result<CachedBuffer> {
        let prime_fd: OwnedFd = self
            .card
            .buffer_to_prime_fd(gem_handle, drm::RDWR)
            .context("PRIME export failed")?;

        let ptr = unsafe {
            mm::mmap(
                ptr::null_mut(),
                size,
                ProtFlags::READ,
                MapFlags::SHARED,
                &prime_fd,
                0,
            )
            .context("PRIME mmap failed")?
        };

        Ok(CachedBuffer {
            fb_key,
            gem_handle,
            ptr,
            size,
            format,
            pitch,
            _prime_fd: Some(prime_fd),
        })
    }

    fn map_dumb_cached(
        &self,
        fb_key: u32,
        gem_handle: drm::buffer::Handle,
        size: usize,
        format: DrmFourcc,
        pitch: u32,
    ) -> Result<CachedBuffer> {
        let map_result =
            drm_ffi::mode::dumbbuffer::map(self.card.as_fd(), u32::from(gem_handle), 0, 0)
                .context("DRM_IOCTL_MODE_MAP_DUMB failed")?;

        let ptr = unsafe {
            mm::mmap(
                ptr::null_mut(),
                size,
                ProtFlags::READ,
                MapFlags::SHARED,
                self.card.as_fd(),
                map_result.offset,
            )
            .context("dumb buffer mmap failed")?
        };

        Ok(CachedBuffer {
            fb_key,
            gem_handle,
            ptr,
            size,
            format,
            pitch,
            _prime_fd: None,
        })
    }

    fn evict_entry(&self, entry: CachedBuffer) {
        unsafe {
            let _ = mm::munmap(entry.ptr, entry.size);
        }
        let _ = self.card.close_buffer(entry.gem_handle);
    }
}

impl Drop for Capturer {
    fn drop(&mut self) {
        for entry in self.cache.drain(..) {
            unsafe {
                let _ = mm::munmap(entry.ptr, entry.size);
            }
            let _ = self.card.close_buffer(entry.gem_handle);
        }
    }
}
