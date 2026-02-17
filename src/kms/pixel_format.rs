use drm_fourcc::DrmFourcc;

/// Convert raw framebuffer pixels to BGRA8888 format into a caller-provided buffer.
/// The buffer is cleared and resized as needed.
pub fn convert_to_bgra_into(
    dst: &mut Vec<u8>,
    src: &[u8],
    width: u32,
    height: u32,
    pitch: u32,
    format: DrmFourcc,
) -> Result<(), String> {
    match format {
        DrmFourcc::Xrgb8888 | DrmFourcc::Argb8888 => copy_rows_into(dst, src, width, height, pitch),
        DrmFourcc::Xbgr8888 => convert_xbgr8888_into(dst, src, width, height, pitch),
        DrmFourcc::Abgr8888 => convert_abgr8888_into(dst, src, width, height, pitch),
        DrmFourcc::Rgb565 => convert_rgb565_into(dst, src, width, height, pitch),
        other => return Err(format!("Unsupported pixel format: {other:?}")),
    }
    Ok(())
}

/// Row-copy for formats whose memory layout matches VNC's BGRX byte order.
fn copy_rows_into(dst: &mut Vec<u8>, src: &[u8], width: u32, height: u32, pitch: u32) {
    let row_bytes = (width * 4) as usize;
    let total = row_bytes * height as usize;
    dst.clear();
    dst.reserve(total);
    if pitch as usize == row_bytes {
        dst.extend_from_slice(&src[..total]);
    } else {
        for y in 0..height as usize {
            let row_start = y * pitch as usize;
            dst.extend_from_slice(&src[row_start..row_start + row_bytes]);
        }
    }
}

/// XBGR8888: memory layout [R, G, B, X] per pixel (little-endian u32 = 0xXXBBGGRR)
/// Output BGRA: [B, G, R, 0xFF]
fn convert_xbgr8888_into(dst: &mut Vec<u8>, src: &[u8], width: u32, height: u32, pitch: u32) {
    let total = (width * height * 4) as usize;
    dst.clear();
    dst.reserve(total);
    for y in 0..height {
        let row = &src[(y * pitch) as usize..];
        for x in 0..width as usize {
            let off = x * 4;
            dst.push(row[off + 2]); // B
            dst.push(row[off + 1]); // G
            dst.push(row[off]);     // R
            dst.push(0xFF);         // A
        }
    }
}

/// ABGR8888: memory layout [R, G, B, A] per pixel (little-endian u32 = 0xAABBGGRR)
/// Output BGRA: [B, G, R, 0xFF] (force opaque -- VNC has no alpha channel)
fn convert_abgr8888_into(dst: &mut Vec<u8>, src: &[u8], width: u32, height: u32, pitch: u32) {
    let total = (width * height * 4) as usize;
    dst.clear();
    dst.reserve(total);
    for y in 0..height {
        let row = &src[(y * pitch) as usize..];
        for x in 0..width as usize {
            let off = x * 4;
            dst.push(row[off + 2]); // B
            dst.push(row[off + 1]); // G
            dst.push(row[off]);     // R
            dst.push(0xFF);         // A (force opaque)
        }
    }
}

/// RGB565: memory layout [GGGBBBBB, RRRRRGGG] per pixel (little-endian u16)
/// Output BGRA
fn convert_rgb565_into(dst: &mut Vec<u8>, src: &[u8], width: u32, height: u32, pitch: u32) {
    let total = (width * height * 4) as usize;
    dst.clear();
    dst.reserve(total);
    for y in 0..height {
        let row = &src[(y * pitch) as usize..];
        for x in 0..width as usize {
            let off = x * 2;
            let lo = row[off] as u16;
            let hi = row[off + 1] as u16;
            let pixel = lo | (hi << 8);
            let r = ((pixel >> 11) & 0x1F) as u8;
            let g = ((pixel >> 5) & 0x3F) as u8;
            let b = (pixel & 0x1F) as u8;
            dst.push((b << 3) | (b >> 2)); // B
            dst.push((g << 2) | (g >> 4)); // G
            dst.push((r << 3) | (r >> 2)); // R
            dst.push(0xFF);                // A
        }
    }
}
