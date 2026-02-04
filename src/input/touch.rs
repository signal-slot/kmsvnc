use std::fs::OpenOptions;

use anyhow::{Context, Result};
use input_linux::{
    AbsoluteAxis, AbsoluteInfo, AbsoluteInfoSetup, EventKind, InputId, InputProperty,
    Key, UInputHandle,
};

/// Virtual touchscreen backed by uinput.
pub struct VirtualTouchscreen {
    handle: UInputHandle<std::fs::File>,
    tracking_id: i32,
    is_touching: bool,
    last_x: u16,
    last_y: u16,
}

impl VirtualTouchscreen {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .context("Cannot open /dev/uinput. Ensure the user has permission (try: sudo usermod -aG input $USER)")?;

        let handle = UInputHandle::new(file);

        handle.set_evbit(EventKind::Absolute).context("set EV_ABS")?;
        handle.set_evbit(EventKind::Key).context("set EV_KEY")?;
        handle.set_keybit(Key::ButtonTouch).context("set BTN_TOUCH")?;
        handle.set_absbit(AbsoluteAxis::MultitouchSlot).context("set ABS_MT_SLOT")?;
        handle.set_absbit(AbsoluteAxis::MultitouchTrackingId).context("set ABS_MT_TRACKING_ID")?;
        handle.set_absbit(AbsoluteAxis::MultitouchPositionX).context("set ABS_MT_POSITION_X")?;
        handle.set_absbit(AbsoluteAxis::MultitouchPositionY).context("set ABS_MT_POSITION_Y")?;
        handle.set_propbit(InputProperty::Direct).context("set INPUT_PROP_DIRECT")?;

        let id = InputId {
            bustype: 0x06, // BUS_VIRTUAL
            vendor: 0x1234,
            product: 0x5678,
            version: 1,
        };

        let abs = [
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::MultitouchSlot,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: 9,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::MultitouchTrackingId,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: 65535,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::MultitouchPositionX,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: width as i32 - 1,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
            AbsoluteInfoSetup {
                axis: AbsoluteAxis::MultitouchPositionY,
                info: AbsoluteInfo {
                    value: 0,
                    minimum: 0,
                    maximum: height as i32 - 1,
                    fuzz: 0,
                    flat: 0,
                    resolution: 0,
                },
            },
        ];

        handle
            .create(&id, b"kmsvnc-touch", 0, &abs)
            .context("create uinput touch device")?;

        tracing::info!("Created virtual touchscreen ({}x{})", width, height);

        // Give udev time to create the device node
        std::thread::sleep(std::time::Duration::from_millis(100));

        Ok(Self {
            handle,
            tracking_id: 0,
            is_touching: false,
            last_x: 0,
            last_y: 0,
        })
    }

    /// Process a VNC PointerEvent.
    /// button_mask bit 0 = left click = touch.
    pub fn handle_pointer(&mut self, button_mask: u8, x: u16, y: u16) -> Result<()> {
        let touching = (button_mask & 1) != 0;

        if touching && !self.is_touching {
            self.tracking_id = (self.tracking_id + 1) % 65536;
            self.touch_down(x, y)?;
            self.is_touching = true;
        } else if touching && self.is_touching && (x != self.last_x || y != self.last_y) {
            self.touch_move(x, y)?;
        } else if !touching && self.is_touching {
            self.touch_up()?;
            self.is_touching = false;
        }

        self.last_x = x;
        self.last_y = y;
        Ok(())
    }

    fn touch_down(&self, x: u16, y: u16) -> Result<()> {
        let events = [
            make_event(EV_ABS, ABS_MT_SLOT, 0),
            make_event(EV_ABS, ABS_MT_TRACKING_ID, self.tracking_id),
            make_event(EV_ABS, ABS_MT_POSITION_X, x as i32),
            make_event(EV_ABS, ABS_MT_POSITION_Y, y as i32),
            make_event(EV_KEY, BTN_TOUCH, 1),
            make_event(EV_SYN, SYN_REPORT, 0),
        ];
        self.handle.write(&events).context("write touch_down")?;
        Ok(())
    }

    fn touch_move(&self, x: u16, y: u16) -> Result<()> {
        let events = [
            make_event(EV_ABS, ABS_MT_SLOT, 0),
            make_event(EV_ABS, ABS_MT_POSITION_X, x as i32),
            make_event(EV_ABS, ABS_MT_POSITION_Y, y as i32),
            make_event(EV_SYN, SYN_REPORT, 0),
        ];
        self.handle.write(&events).context("write touch_move")?;
        Ok(())
    }

    fn touch_up(&self) -> Result<()> {
        let events = [
            make_event(EV_ABS, ABS_MT_SLOT, 0),
            make_event(EV_ABS, ABS_MT_TRACKING_ID, -1),
            make_event(EV_KEY, BTN_TOUCH, 0),
            make_event(EV_SYN, SYN_REPORT, 0),
        ];
        self.handle.write(&events).context("write touch_up")?;
        Ok(())
    }
}

impl Drop for VirtualTouchscreen {
    fn drop(&mut self) {
        if let Err(e) = self.handle.dev_destroy() {
            tracing::warn!("Failed to destroy touch device: {e}");
        }
    }
}

const EV_SYN: u16 = input_linux::sys::EV_SYN as u16;
const EV_KEY: u16 = input_linux::sys::EV_KEY as u16;
const EV_ABS: u16 = input_linux::sys::EV_ABS as u16;
const SYN_REPORT: u16 = input_linux::sys::SYN_REPORT as u16;
const BTN_TOUCH: u16 = input_linux::sys::BTN_TOUCH as u16;
const ABS_MT_SLOT: u16 = input_linux::sys::ABS_MT_SLOT as u16;
const ABS_MT_TRACKING_ID: u16 = input_linux::sys::ABS_MT_TRACKING_ID as u16;
const ABS_MT_POSITION_X: u16 = input_linux::sys::ABS_MT_POSITION_X as u16;
const ABS_MT_POSITION_Y: u16 = input_linux::sys::ABS_MT_POSITION_Y as u16;

fn make_event(type_: u16, code: u16, value: i32) -> input_linux::sys::input_event {
    let mut ev: input_linux::sys::input_event = unsafe { std::mem::zeroed() };
    ev.type_ = type_;
    ev.code = code;
    ev.value = value;
    ev
}
