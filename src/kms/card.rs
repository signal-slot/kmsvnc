use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, BorrowedFd};

use drm::control::Device as ControlDevice;
use drm::Device;

pub struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Device for Card {}
impl ControlDevice for Card {}

impl Card {
    pub fn open(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        let card = Card(file);
        // Release DRM master so other apps (e.g. EGLFS) can acquire it.
        // kmsvnc only reads framebuffers and doesn't need master privileges.
        let _ = card.release_master_lock();
        Ok(card)
    }
}
