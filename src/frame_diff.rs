const TILE_SIZE: u32 = 64;

/// A dirty rectangle (coordinates only, no pixel data).
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Compare two BGRA framebuffers and return dirty rectangles (coordinates only).
/// If `prev` is `None`, the entire frame is returned as dirty.
pub fn compute_dirty_rects(prev: Option<&[u8]>, curr: &[u8], width: u32, height: u32) -> Vec<DirtyRect> {
    let prev = match prev {
        Some(p) if p.len() == curr.len() => p,
        _ => {
            return vec![DirtyRect {
                x: 0,
                y: 0,
                width: width as u16,
                height: height as u16,
            }];
        }
    };

    let stride = (width * 4) as usize;
    let tiles_x = width.div_ceil(TILE_SIZE);
    let tiles_y = height.div_ceil(TILE_SIZE);

    let mut rects = Vec::new();

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let x0 = tx * TILE_SIZE;
            let y0 = ty * TILE_SIZE;
            let tw = (TILE_SIZE).min(width - x0);
            let th = (TILE_SIZE).min(height - y0);

            if is_tile_dirty(prev, curr, x0, y0, tw, th, stride) {
                rects.push(DirtyRect {
                    x: x0 as u16,
                    y: y0 as u16,
                    width: tw as u16,
                    height: th as u16,
                });
            }
        }
    }

    rects
}

fn is_tile_dirty(
    prev: &[u8],
    curr: &[u8],
    x0: u32,
    y0: u32,
    tw: u32,
    th: u32,
    stride: usize,
) -> bool {
    for row in y0..y0 + th {
        let start = (row as usize) * stride + (x0 as usize) * 4;
        let end = start + (tw as usize) * 4;
        if prev[start..end] != curr[start..end] {
            return true;
        }
    }
    false
}
