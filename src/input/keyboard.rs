use std::fs::OpenOptions;

use anyhow::{Context, Result};
use input_linux::{EventKind, InputId, Key, UInputHandle};

/// Virtual keyboard backed by uinput.
pub struct VirtualKeyboard {
    handle: UInputHandle<std::fs::File>,
}

impl VirtualKeyboard {
    pub fn new() -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/uinput")
            .context("Cannot open /dev/uinput")?;

        let handle = UInputHandle::new(file);

        handle.set_evbit(EventKind::Key).context("set EV_KEY")?;

        for &key in &ALL_KEYS {
            handle.set_keybit(key).context("set key bit")?;
        }

        let id = InputId {
            bustype: 0x06, // BUS_VIRTUAL
            vendor: 0x1234,
            product: 0x5679,
            version: 1,
        };

        handle
            .create(&id, b"kmsvnc-keyboard", 0, &[])
            .context("create uinput keyboard device")?;

        tracing::info!("Created virtual keyboard");

        std::thread::sleep(std::time::Duration::from_millis(100));

        Ok(Self { handle })
    }

    /// Process a VNC KeyEvent.
    pub fn handle_key(&self, down: bool, keysym: u32) -> Result<()> {
        let Some(code) = keysym_to_linux_key(keysym) else {
            tracing::debug!("Unknown keysym: 0x{keysym:04x}");
            return Ok(());
        };

        let events = [
            make_event(EV_KEY, code, if down { 1 } else { 0 }),
            make_event(EV_SYN, SYN_REPORT, 0),
        ];
        self.handle.write(&events).context("write key events")?;
        Ok(())
    }
}

impl Drop for VirtualKeyboard {
    fn drop(&mut self) {
        if let Err(e) = self.handle.dev_destroy() {
            tracing::warn!("Failed to destroy keyboard device: {e}");
        }
    }
}

const EV_SYN: u16 = input_linux::sys::EV_SYN as u16;
const EV_KEY: u16 = input_linux::sys::EV_KEY as u16;
const SYN_REPORT: u16 = input_linux::sys::SYN_REPORT as u16;

fn make_event(type_: u16, code: u16, value: i32) -> input_linux::sys::input_event {
    let mut ev: input_linux::sys::input_event = unsafe { std::mem::zeroed() };
    ev.type_ = type_;
    ev.code = code;
    ev.value = value;
    ev
}

const ALL_KEYS: [Key; 85] = [
    Key::Esc,
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
    Key::Num0,
    Key::Minus,
    Key::Equal,
    Key::Backspace,
    Key::Tab,
    Key::Q,
    Key::W,
    Key::E,
    Key::R,
    Key::T,
    Key::Y,
    Key::U,
    Key::I,
    Key::O,
    Key::P,
    Key::LeftBrace,
    Key::RightBrace,
    Key::Enter,
    Key::LeftCtrl,
    Key::A,
    Key::S,
    Key::D,
    Key::F,
    Key::G,
    Key::H,
    Key::J,
    Key::K,
    Key::L,
    Key::Semicolon,
    Key::Apostrophe,
    Key::Grave,
    Key::LeftShift,
    Key::Backslash,
    Key::Z,
    Key::X,
    Key::C,
    Key::V,
    Key::B,
    Key::N,
    Key::M,
    Key::Comma,
    Key::Dot,
    Key::Slash,
    Key::RightShift,
    Key::KpAsterisk,
    Key::LeftAlt,
    Key::Space,
    Key::CapsLock,
    Key::F1,
    Key::F2,
    Key::F3,
    Key::F4,
    Key::F5,
    Key::F6,
    Key::F7,
    Key::F8,
    Key::F9,
    Key::F10,
    Key::NumLock,
    Key::ScrollLock,
    Key::Kp7,
    Key::Kp8,
    Key::Kp9,
    Key::KpMinus,
    Key::Kp4,
    Key::Kp5,
    Key::Kp6,
    Key::KpPlus,
    Key::Kp1,
    Key::Kp2,
    Key::Kp3,
    Key::Kp0,
    Key::KpDot,
    Key::F11,
    Key::F12,
];

/// Map X11 keysym to Linux KEY_* code.
fn keysym_to_linux_key(keysym: u32) -> Option<u16> {
    use input_linux::sys::*;

    let code: i32 = match keysym {
        // TTY function keys
        0xff08 => KEY_BACKSPACE,
        0xff09 => KEY_TAB,
        0xff0d => KEY_ENTER,
        0xff1b => KEY_ESC,
        0xffff => KEY_DELETE,

        // Cursor control
        0xff50 => KEY_HOME,
        0xff51 => KEY_LEFT,
        0xff52 => KEY_UP,
        0xff53 => KEY_RIGHT,
        0xff54 => KEY_DOWN,
        0xff55 => KEY_PAGEUP,
        0xff56 => KEY_PAGEDOWN,
        0xff57 => KEY_END,
        0xff63 => KEY_INSERT,

        // Function keys
        0xffbe => KEY_F1,
        0xffbf => KEY_F2,
        0xffc0 => KEY_F3,
        0xffc1 => KEY_F4,
        0xffc2 => KEY_F5,
        0xffc3 => KEY_F6,
        0xffc4 => KEY_F7,
        0xffc5 => KEY_F8,
        0xffc6 => KEY_F9,
        0xffc7 => KEY_F10,
        0xffc8 => KEY_F11,
        0xffc9 => KEY_F12,

        // Modifier keys
        0xffe1 => KEY_LEFTSHIFT,
        0xffe2 => KEY_RIGHTSHIFT,
        0xffe3 => KEY_LEFTCTRL,
        0xffe4 => KEY_RIGHTCTRL,
        0xffe5 => KEY_CAPSLOCK,
        0xffe9 => KEY_LEFTALT,
        0xffea => KEY_RIGHTALT,
        0xffeb => KEY_LEFTMETA,
        0xffec => KEY_RIGHTMETA,

        // Keypad
        0xffb0 => KEY_KP0,
        0xffb1 => KEY_KP1,
        0xffb2 => KEY_KP2,
        0xffb3 => KEY_KP3,
        0xffb4 => KEY_KP4,
        0xffb5 => KEY_KP5,
        0xffb6 => KEY_KP6,
        0xffb7 => KEY_KP7,
        0xffb8 => KEY_KP8,
        0xffb9 => KEY_KP9,
        0xff8d => KEY_KPENTER,
        0xffaf => KEY_KPSLASH,
        0xffaa => KEY_KPASTERISK,
        0xffad => KEY_KPMINUS,
        0xffab => KEY_KPPLUS,
        0xffae => KEY_KPDOT,

        // Misc
        0xff14 => KEY_SCROLLLOCK,
        0xff7f => KEY_NUMLOCK,
        0xff61 => KEY_SYSRQ,

        // Space
        0x0020 => KEY_SPACE,

        // Numbers 0-9
        0x0030 => KEY_0,
        0x0031 => KEY_1,
        0x0032 => KEY_2,
        0x0033 => KEY_3,
        0x0034 => KEY_4,
        0x0035 => KEY_5,
        0x0036 => KEY_6,
        0x0037 => KEY_7,
        0x0038 => KEY_8,
        0x0039 => KEY_9,

        // Lowercase letters
        0x0061 => KEY_A,
        0x0062 => KEY_B,
        0x0063 => KEY_C,
        0x0064 => KEY_D,
        0x0065 => KEY_E,
        0x0066 => KEY_F,
        0x0067 => KEY_G,
        0x0068 => KEY_H,
        0x0069 => KEY_I,
        0x006a => KEY_J,
        0x006b => KEY_K,
        0x006c => KEY_L,
        0x006d => KEY_M,
        0x006e => KEY_N,
        0x006f => KEY_O,
        0x0070 => KEY_P,
        0x0071 => KEY_Q,
        0x0072 => KEY_R,
        0x0073 => KEY_S,
        0x0074 => KEY_T,
        0x0075 => KEY_U,
        0x0076 => KEY_V,
        0x0077 => KEY_W,
        0x0078 => KEY_X,
        0x0079 => KEY_Y,
        0x007a => KEY_Z,

        // Uppercase letters (same key codes, shift is separate)
        0x0041 => KEY_A,
        0x0042 => KEY_B,
        0x0043 => KEY_C,
        0x0044 => KEY_D,
        0x0045 => KEY_E,
        0x0046 => KEY_F,
        0x0047 => KEY_G,
        0x0048 => KEY_H,
        0x0049 => KEY_I,
        0x004a => KEY_J,
        0x004b => KEY_K,
        0x004c => KEY_L,
        0x004d => KEY_M,
        0x004e => KEY_N,
        0x004f => KEY_O,
        0x0050 => KEY_P,
        0x0051 => KEY_Q,
        0x0052 => KEY_R,
        0x0053 => KEY_S,
        0x0054 => KEY_T,
        0x0055 => KEY_U,
        0x0056 => KEY_V,
        0x0057 => KEY_W,
        0x0058 => KEY_X,
        0x0059 => KEY_Y,
        0x005a => KEY_Z,

        // Symbols (mapped to their unshifted key)
        0x0021 => KEY_1,          // !
        0x0040 => KEY_2,          // @
        0x0023 => KEY_3,          // #
        0x0024 => KEY_4,          // $
        0x0025 => KEY_5,          // %
        0x005e => KEY_6,          // ^
        0x0026 => KEY_7,          // &
        0x002a => KEY_8,          // *
        0x0028 => KEY_9,          // (
        0x0029 => KEY_0,          // )
        0x002d => KEY_MINUS,      // -
        0x005f => KEY_MINUS,      // _
        0x003d => KEY_EQUAL,      // =
        0x002b => KEY_EQUAL,      // +
        0x005b => KEY_LEFTBRACE,  // [
        0x007b => KEY_LEFTBRACE,  // {
        0x005d => KEY_RIGHTBRACE, // ]
        0x007d => KEY_RIGHTBRACE, // }
        0x005c => KEY_BACKSLASH,  // backslash
        0x007c => KEY_BACKSLASH,  // |
        0x003b => KEY_SEMICOLON,  // ;
        0x003a => KEY_SEMICOLON,  // :
        0x0027 => KEY_APOSTROPHE, // '
        0x0022 => KEY_APOSTROPHE, // "
        0x0060 => KEY_GRAVE,      // `
        0x007e => KEY_GRAVE,      // ~
        0x002c => KEY_COMMA,      // ,
        0x003c => KEY_COMMA,      // <
        0x002e => KEY_DOT,        // .
        0x003e => KEY_DOT,        // >
        0x002f => KEY_SLASH,      // /
        0x003f => KEY_SLASH,      // ?

        _ => return None,
    };

    Some(code as u16)
}
