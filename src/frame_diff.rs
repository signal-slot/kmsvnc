use std::sync::atomic::{AtomicU64, Ordering};

pub const TILE_SIZE: u32 = 64;

/// A dirty rectangle (coordinates only, no pixel data).
pub struct DirtyRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Lock-free dirty tile accumulator shared between capture and VNC threads.
///
/// The capture thread sets bits for tiles that changed.
/// The VNC server drains (reads + clears) accumulated bits to get dirty rects.
/// Supports up to 512 tiles (e.g., 22×22 tiles for 1408×1408 at 64px tiles).
pub struct DirtyTiles {
    bits: [AtomicU64; 8],
    tiles_x: u32,
    tiles_y: u32,
    width: u32,
    height: u32,
}

impl DirtyTiles {
    pub fn new(width: u32, height: u32) -> Self {
        let tiles_x = width.div_ceil(TILE_SIZE);
        let tiles_y = height.div_ceil(TILE_SIZE);
        assert!(
            (tiles_x * tiles_y) as usize <= 512,
            "Too many tiles ({tiles_x}x{tiles_y}), max 512"
        );
        Self {
            bits: std::array::from_fn(|_| AtomicU64::new(0)),
            tiles_x,
            tiles_y,
            width,
            height,
        }
    }

    /// Mark a tile as dirty (by tile index).
    #[inline]
    pub fn set(&self, tile_idx: usize) {
        let word = tile_idx / 64;
        let bit = tile_idx % 64;
        self.bits[word].fetch_or(1 << bit, Ordering::Relaxed);
    }

    /// Mark all tiles as dirty.
    pub fn set_all(&self) {
        let total = (self.tiles_x * self.tiles_y) as usize;
        for word in 0..(total / 64) {
            self.bits[word].store(u64::MAX, Ordering::Relaxed);
        }
        let remaining = total % 64;
        if remaining > 0 {
            let mask = (1u64 << remaining) - 1;
            self.bits[total / 64].fetch_or(mask, Ordering::Relaxed);
        }
    }

    /// Atomically drain all dirty bits and convert to DirtyRect list.
    pub fn drain_to_rects(&self) -> Vec<DirtyRect> {
        // Atomically swap all words to 0
        let mut words = [0u64; 8];
        for (i, w) in words.iter_mut().enumerate() {
            *w = self.bits[i].swap(0, Ordering::Relaxed);
        }

        let mut rects = Vec::new();
        for ty in 0..self.tiles_y {
            for tx in 0..self.tiles_x {
                let idx = (ty * self.tiles_x + tx) as usize;
                let word = idx / 64;
                let bit = idx % 64;
                if words[word] & (1 << bit) != 0 {
                    let x0 = tx * TILE_SIZE;
                    let y0 = ty * TILE_SIZE;
                    rects.push(DirtyRect {
                        x: x0 as u16,
                        y: y0 as u16,
                        width: TILE_SIZE.min(self.width - x0) as u16,
                        height: TILE_SIZE.min(self.height - y0) as u16,
                    });
                }
            }
        }
        rects
    }
}
