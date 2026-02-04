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
        Ok(Card(file))
    }
}
