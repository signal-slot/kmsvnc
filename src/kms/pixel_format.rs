use drm_fourcc::DrmFourcc;

/// Convert raw framebuffer pixels to BGRA8888 format (VNC-native byte order: B, G, R, A).
pub fn convert_to_bgra(
    src: &[u8],
    width: u32,
    height: u32,
    pitch: u32,
    format: DrmFourcc,
) -> Result<Vec<u8>, String> {
    match format {
        DrmFourcc::Xrgb8888 => convert_xrgb8888(src, width, height, pitch),
        DrmFourcc::Argb8888 => convert_argb8888(src, width, height, pitch),
        DrmFourcc::Xbgr8888 => convert_xbgr8888(src, width, height, pitch),
        DrmFourcc::Abgr8888 => convert_abgr8888(src, width, height, pitch),
        DrmFourcc::Rgb565 => convert_rgb565(src, width, height, pitch),
        other => Err(format!("Unsupported pixel format: {other:?}")),
    }
}

/// XRGB8888: memory layout [B, G, R, X] per pixel (little-endian u32 = 0xXXRRGGBB)
/// Output BGRA: [B, G, R, 0xFF]
fn convert_xrgb8888(src: &[u8], width: u32, height: u32, pitch: u32) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let row = &src[(y * pitch) as usize..];
        for x in 0..width as usize {
            let off = x * 4;
            dst.push(row[off]);     // B
            dst.push(row[off + 1]); // G
            dst.push(row[off + 2]); // R
            dst.push(0xFF);         // A
        }
    }
    Ok(dst)
}

/// ARGB8888: memory layout [B, G, R, A] per pixel (little-endian u32 = 0xAARRGGBB)
/// Output BGRA: [B, G, R, 0xFF] (force opaque — VNC has no alpha channel)
fn convert_argb8888(src: &[u8], width: u32, height: u32, pitch: u32) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        let row = &src[(y * pitch) as usize..];
        for x in 0..width as usize {
            let off = x * 4;
            dst.push(row[off]);     // B
            dst.push(row[off + 1]); // G
            dst.push(row[off + 2]); // R
            dst.push(0xFF);         // A (force opaque)
        }
    }
    Ok(dst)
}

/// XBGR8888: memory layout [R, G, B, X] per pixel (little-endian u32 = 0xXXBBGGRR)
/// Output BGRA: [B, G, R, 0xFF]
fn convert_xbgr8888(src: &[u8], width: u32, height: u32, pitch: u32) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity((width * height * 4) as usize);
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
    Ok(dst)
}

/// ABGR8888: memory layout [R, G, B, A] per pixel (little-endian u32 = 0xAABBGGRR)
/// Output BGRA: [B, G, R, 0xFF] (force opaque — VNC has no alpha channel)
fn convert_abgr8888(src: &[u8], width: u32, height: u32, pitch: u32) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity((width * height * 4) as usize);
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
    Ok(dst)
}

/// RGB565: memory layout [GGGBBBBB, RRRRRGGG] per pixel (little-endian u16)
/// Output BGRA
fn convert_rgb565(src: &[u8], width: u32, height: u32, pitch: u32) -> Result<Vec<u8>, String> {
    let mut dst = Vec::with_capacity((width * height * 4) as usize);
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
    Ok(dst)
}
