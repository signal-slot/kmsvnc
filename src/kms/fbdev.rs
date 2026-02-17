use std::ffi::{c_int, c_ulong, c_void};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::ptr;

use anyhow::{bail, Context, Result};
use drm_fourcc::DrmFourcc;
use rustix::mm::{self, MapFlags, ProtFlags};

use super::pixel_format;

const FBIOGET_VSCREENINFO: c_ulong = 0x4600;
const FBIOGET_FSCREENINFO: c_ulong = 0x4602;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FbBitfield {
    offset: u32,
    length: u32,
    msb_right: u32,
}

/// Kernel's `struct fb_var_screeninfo` (160 bytes).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct FbVarScreeninfo {
    xres: u32,
    yres: u32,
    xres_virtual: u32,
    yres_virtual: u32,
    xoffset: u32,
    yoffset: u32,
    bits_per_pixel: u32,
    grayscale: u32,
    red: FbBitfield,
    green: FbBitfield,
    blue: FbBitfield,
    transp: FbBitfield,
    nonstd: u32,
    activate: u32,
    height: u32,
    width: u32,
    accel_flags: u32,
    pixclock: u32,
    left_margin: u32,
    right_margin: u32,
    upper_margin: u32,
    lower_margin: u32,
    hsync_len: u32,
    vsync_len: u32,
    sync: u32,
    vmode: u32,
    rotate: u32,
    colorspace: u32,
    reserved: [u32; 4],
}

/// Kernel's `struct fb_fix_screeninfo`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct FbFixScreeninfo {
    id: [u8; 16],
    smem_start: c_ulong,
    smem_len: u32,
    type_: u32,
    type_aux: u32,
    visual: u32,
    xpanstep: u16,
    ypanstep: u16,
    ywrapstep: u16,
    _pad: u16,
    line_length: u32,
    mmio_start: c_ulong,
    mmio_len: u32,
    accel: u32,
    capabilities: u16,
    reserved: [u16; 2],
    _pad2: u16,
}

extern "C" {
    fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
}

pub struct FbdevCapture {
    _file: File,
    width: u32,
    height: u32,
    stride: u32,
    xoffset: u32,
    yoffset: u32,
    format: DrmFourcc,
    mmap_ptr: *mut c_void,
    mmap_size: usize,
}

// The mmap pointer is read-only and the mapped region does not change.
unsafe impl Send for FbdevCapture {}

impl FbdevCapture {
    pub fn open(path: &str) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .with_context(|| format!("Cannot open {path}"))?;

        let fd = file.as_raw_fd();

        let var = unsafe {
            let mut var = FbVarScreeninfo::default();
            if ioctl(fd, FBIOGET_VSCREENINFO, &mut var as *mut FbVarScreeninfo) < 0 {
                bail!(
                    "FBIOGET_VSCREENINFO failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            var
        };

        let fix = unsafe {
            let mut fix: FbFixScreeninfo = std::mem::zeroed();
            if ioctl(fd, FBIOGET_FSCREENINFO, &mut fix as *mut FbFixScreeninfo) < 0 {
                bail!(
                    "FBIOGET_FSCREENINFO failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            fix
        };

        let format = match (
            var.bits_per_pixel,
            var.red.offset,
            var.green.offset,
            var.blue.offset,
            var.transp.length,
        ) {
            (32, 16, 8, 0, 0) => DrmFourcc::Xrgb8888,
            (32, 16, 8, 0, 8) => DrmFourcc::Argb8888,
            (32, 0, 8, 16, 0) => DrmFourcc::Xbgr8888,
            (32, 0, 8, 16, 8) => DrmFourcc::Abgr8888,
            (16, 11, 5, 0, _) => DrmFourcc::Rgb565,
            (bpp, r, g, b, a) => {
                bail!(
                    "Unsupported fbdev pixel format: {bpp}bpp \
                     red.offset={r} green.offset={g} blue.offset={b} transp.length={a}"
                );
            }
        };

        let mmap_size = fix.smem_len as usize;
        let mmap_ptr = unsafe {
            mm::mmap(
                ptr::null_mut(),
                mmap_size,
                ProtFlags::READ,
                MapFlags::SHARED,
                &file,
                0,
            )
            .context("fbdev mmap failed")?
        };

        tracing::info!(
            "fbdev: {path} {}x{} {format:?}, stride={}, mmap_size={}",
            var.xres,
            var.yres,
            fix.line_length,
            mmap_size,
        );

        Ok(FbdevCapture {
            _file: file,
            width: var.xres,
            height: var.yres,
            stride: fix.line_length,
            xoffset: var.xoffset,
            yoffset: var.yoffset,
            format,
            mmap_ptr,
            mmap_size,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn capture_frame_into(&self, dst: &mut Vec<u8>) -> Result<()> {
        let bpp = match self.format {
            DrmFourcc::Rgb565 => 2u32,
            _ => 4u32,
        };

        // Compute start offset from xoffset/yoffset
        let start = (self.yoffset as usize) * (self.stride as usize)
            + (self.xoffset as usize) * (bpp as usize);
        let needed = (self.height as usize) * (self.stride as usize);

        if start + needed > self.mmap_size {
            bail!(
                "fbdev mmap too small: need {} bytes at offset {}, have {}",
                needed,
                start,
                self.mmap_size
            );
        }

        let raw = unsafe {
            let base = (self.mmap_ptr as *const u8).add(start);
            std::slice::from_raw_parts(base, needed)
        };

        pixel_format::convert_to_bgra_into(dst, raw, self.width, self.height, self.stride, self.format)
            .map_err(|e| anyhow::anyhow!(e))
    }

    pub fn capture_frame(&self) -> Result<Vec<u8>> {
        let mut dst = Vec::new();
        self.capture_frame_into(&mut dst)?;
        Ok(dst)
    }
}

impl Drop for FbdevCapture {
    fn drop(&mut self) {
        unsafe {
            let _ = mm::munmap(self.mmap_ptr, self.mmap_size);
        }
    }
}
