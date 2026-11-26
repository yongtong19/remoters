/// Cross-platform representation of Windows virtual-key (`1..=254`) mappings for Linux X11/Wayland.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum LinuxKey {
    Alt,
    AltRight,
    Backspace,
    Cancel,
    CapsLock,
    Clear,
    Control,
    ControlLeft,
    ControlRight,
    Delete,
    DownArrow,
    End,
    Escape,
    Execute,
    F(u8),
    Hangul,
    Hanja,
    Help,
    Home,
    Insert,
    Kanji,
    LeftArrow,
    MediaNextTrack,
    MediaPlayPause,
    MediaPrevTrack,
    MediaStop,
    Menu,
    ModeChange,
    NumLock,
    Numpad(u8),
    NumpadAdd,
    NumpadDecimal,
    NumpadDivide,
    NumpadMultiply,
    NumpadSeparator,
    NumpadSubtract,
    PageDown,
    PageUp,
    Pause,
    Print,
    PrintScreen,
    Return,
    RightArrow,
    ScrollLock,
    Select,
    Shift,
    ShiftLeft,
    ShiftRight,
    Space,
    SuperLeft,
    SuperRight,
    Tab,
    UpArrow,
    VolumeDown,
    VolumeMute,
    VolumeUp,
    BrowserBack,
    BrowserForward,
    BrowserRefresh,
    BrowserStop,
    BrowserSearch,
    BrowserFavorites,
    BrowserHome,
    LaunchMail,
    LaunchMedia,
    LaunchApp1,
    LaunchApp2,
    Sleep,
    Zoom,
    Char(char),
    Keysym(u32),
}

/// Maps a Windows Virtual-Key (`1..=254`) wire code to a [`LinuxKey`].
pub fn vk_to_linux_key(vk: u16) -> Option<LinuxKey> {
    Some(match vk {
        // 0x01, 0x02, 0x04, 0x05, 0x06 are mouse buttons handled via Input::Button.
        0x03 => LinuxKey::Cancel,
        0x08 => LinuxKey::Backspace,
        0x09 => LinuxKey::Tab,
        0x0C => LinuxKey::Clear,
        0x0D => LinuxKey::Return,
        0x10 => LinuxKey::Shift,
        0x11 => LinuxKey::Control,
        0x12 => LinuxKey::Alt,
        0x13 => LinuxKey::Pause,
        0x14 => LinuxKey::CapsLock,
        0x15 => LinuxKey::Hangul,
        0x17 => LinuxKey::Keysym(0xFF33), // Junja -> Hangul_Janja / Romaja
        0x18 => LinuxKey::Keysym(0xFF34), // Final -> Hangul_End
        0x19 => LinuxKey::Hanja,
        0x1B => LinuxKey::Escape,
        0x1C => LinuxKey::Keysym(0xFF23), // Convert -> Henkan
        0x1D => LinuxKey::Keysym(0xFF22), // NonConvert -> Muhenkan
        0x1E => LinuxKey::Keysym(0xFF2E), // Accept -> Hangul_Jeonja
        0x1F => LinuxKey::ModeChange,
        0x20 => LinuxKey::Space,
        0x21 => LinuxKey::PageUp,
        0x22 => LinuxKey::PageDown,
        0x23 => LinuxKey::End,
        0x24 => LinuxKey::Home,
        0x25 => LinuxKey::LeftArrow,
        0x26 => LinuxKey::UpArrow,
        0x27 => LinuxKey::RightArrow,
        0x28 => LinuxKey::DownArrow,
        0x29 => LinuxKey::Select,
        0x2A => LinuxKey::Print,
        0x2B => LinuxKey::Execute,
        0x2C => LinuxKey::PrintScreen,
        0x2D => LinuxKey::Insert,
        0x2E => LinuxKey::Delete,
        0x2F => LinuxKey::Help,
        0x30..=0x39 => LinuxKey::Char(char::from(vk as u8)),
        0x41..=0x5A => LinuxKey::Char(char::from((vk as u8).to_ascii_lowercase())),
        0x5B => LinuxKey::SuperLeft,
        0x5C => LinuxKey::SuperRight,
        0x5D => LinuxKey::Menu,
        0x5F => LinuxKey::Sleep,
        0x60..=0x69 => LinuxKey::Numpad((vk - 0x60) as u8),
        0x6A => LinuxKey::NumpadMultiply,
        0x6B => LinuxKey::NumpadAdd,
        0x6C => LinuxKey::NumpadSeparator,
        0x6D => LinuxKey::NumpadSubtract,
        0x6E => LinuxKey::NumpadDecimal,
        0x6F => LinuxKey::NumpadDivide,
        0x70..=0x87 => LinuxKey::F((vk - 0x6F) as u8),
        0x90 => LinuxKey::NumLock,
        0x91 => LinuxKey::ScrollLock,
        0xA0 => LinuxKey::ShiftLeft,
        0xA1 => LinuxKey::ShiftRight,
        0xA2 => LinuxKey::ControlLeft,
        0xA3 => LinuxKey::ControlRight,
        0xA4 => LinuxKey::Alt,
        0xA5 => LinuxKey::AltRight,
        0xA6 => LinuxKey::BrowserBack,
        0xA7 => LinuxKey::BrowserForward,
        0xA8 => LinuxKey::BrowserRefresh,
        0xA9 => LinuxKey::BrowserStop,
        0xAA => LinuxKey::BrowserSearch,
        0xAB => LinuxKey::BrowserFavorites,
        0xAC => LinuxKey::BrowserHome,
        0xAD => LinuxKey::VolumeMute,
        0xAE => LinuxKey::VolumeDown,
        0xAF => LinuxKey::VolumeUp,
        0xB0 => LinuxKey::MediaNextTrack,
        0xB1 => LinuxKey::MediaPrevTrack,
        0xB2 => LinuxKey::MediaStop,
        0xB3 => LinuxKey::MediaPlayPause,
        0xB4 => LinuxKey::LaunchMail,
        0xB5 => LinuxKey::LaunchMedia,
        0xB6 => LinuxKey::LaunchApp1,
        0xB7 => LinuxKey::LaunchApp2,
        0xBA => LinuxKey::Char(';'),
        0xBB => LinuxKey::Char('='),
        0xBC => LinuxKey::Char(','),
        0xBD => LinuxKey::Char('-'),
        0xBE => LinuxKey::Char('.'),
        0xBF => LinuxKey::Char('/'),
        0xC0 => LinuxKey::Char('`'),
        0xDB => LinuxKey::Char('['),
        0xDC => LinuxKey::Char('\\'),
        0xDD => LinuxKey::Char(']'),
        0xDE => LinuxKey::Char('\''),
        0xDF => LinuxKey::Keysym(0x00A7), // OEM_8 -> section
        0xE2 => LinuxKey::Char('<'),      // OEM_102 -> ISO <>
        0xE5 => LinuxKey::Kanji,          // ProcessKey -> Kanji
        0xFB => LinuxKey::Zoom,
        _ => return None,
    })
}

/// Returns the X11 keysym for [`LinuxKey`] variants mapped via `enigo::Key::Other`.
pub fn linux_key_to_keysym(key: LinuxKey) -> Option<u32> {
    Some(match key {
        LinuxKey::AltRight => 0xFFEA,        // Alt_R
        LinuxKey::SuperRight => 0xFFEC,      // Super_R
        LinuxKey::NumpadSeparator => 0xFFAC, // KP_Separator
        LinuxKey::BrowserBack => 0x1008_FF26,
        LinuxKey::BrowserForward => 0x1008_FF27,
        LinuxKey::BrowserRefresh => 0x1008_FF29,
        LinuxKey::BrowserStop => 0x1008_FF28,
        LinuxKey::BrowserSearch => 0x1008_FF1B,
        LinuxKey::BrowserFavorites => 0x1008_FF30,
        LinuxKey::BrowserHome => 0x1008_FF18,
        LinuxKey::LaunchMail => 0x1008_FF19,
        LinuxKey::LaunchMedia => 0x1008_FF32,
        LinuxKey::LaunchApp1 => 0x1008_FF40,
        LinuxKey::LaunchApp2 => 0x1008_FF41,
        LinuxKey::Sleep => 0x1008_FF2F,
        LinuxKey::Zoom => 0x1008_FF8B,
        LinuxKey::Keysym(sym) => sym,
        _ => return None,
    })
}

#[cfg(target_os = "linux")]
impl From<LinuxKey> for enigo::Key {
    fn from(key: LinuxKey) -> Self {
        match key {
            LinuxKey::Alt => Self::Alt,
            LinuxKey::Backspace => Self::Backspace,
            LinuxKey::Cancel => Self::Cancel,
            LinuxKey::CapsLock => Self::CapsLock,
            LinuxKey::Clear => Self::Clear,
            LinuxKey::Control => Self::Control,
            LinuxKey::ControlLeft => Self::LControl,
            LinuxKey::ControlRight => Self::RControl,
            LinuxKey::Delete => Self::Delete,
            LinuxKey::DownArrow => Self::DownArrow,
            LinuxKey::End => Self::End,
            LinuxKey::Escape => Self::Escape,
            LinuxKey::Execute => Self::Execute,
            LinuxKey::F(n) => match n {
                1 => Self::F1,
                2 => Self::F2,
                3 => Self::F3,
                4 => Self::F4,
                5 => Self::F5,
                6 => Self::F6,
                7 => Self::F7,
                8 => Self::F8,
                9 => Self::F9,
                10 => Self::F10,
                11 => Self::F11,
                12 => Self::F12,
                13 => Self::F13,
                14 => Self::F14,
                15 => Self::F15,
                16 => Self::F16,
                17 => Self::F17,
                18 => Self::F18,
                19 => Self::F19,
                20 => Self::F20,
                21 => Self::F21,
                22 => Self::F22,
                23 => Self::F23,
                24 => Self::F24,
                _ => Self::Other(0),
            },
            LinuxKey::Hangul => Self::Hangul,
            LinuxKey::Hanja => Self::Hanja,
            LinuxKey::Help => Self::Help,
            LinuxKey::Home => Self::Home,
            LinuxKey::Insert => Self::Insert,
            LinuxKey::Kanji => Self::Kanji,
            LinuxKey::LeftArrow => Self::LeftArrow,
            LinuxKey::MediaNextTrack => Self::MediaNextTrack,
            LinuxKey::MediaPlayPause => Self::MediaPlayPause,
            LinuxKey::MediaPrevTrack => Self::MediaPrevTrack,
            LinuxKey::MediaStop => Self::MediaStop,
            LinuxKey::Menu => Self::LMenu,
            LinuxKey::ModeChange => Self::ModeChange,
            LinuxKey::NumLock => Self::Numlock,
            LinuxKey::Numpad(d) => match d {
                0 => Self::Numpad0,
                1 => Self::Numpad1,
                2 => Self::Numpad2,
                3 => Self::Numpad3,
                4 => Self::Numpad4,
                5 => Self::Numpad5,
                6 => Self::Numpad6,
                7 => Self::Numpad7,
                8 => Self::Numpad8,
                9 => Self::Numpad9,
                _ => Self::Other(0),
            },
            LinuxKey::NumpadAdd => Self::Add,
            LinuxKey::NumpadDecimal => Self::Decimal,
            LinuxKey::NumpadDivide => Self::Divide,
            LinuxKey::NumpadMultiply => Self::Multiply,
            LinuxKey::NumpadSubtract => Self::Subtract,
            LinuxKey::PageDown => Self::PageDown,
            LinuxKey::PageUp => Self::PageUp,
            LinuxKey::Pause => Self::Pause,
            LinuxKey::Print | LinuxKey::PrintScreen => Self::PrintScr,
            LinuxKey::Return => Self::Return,
            LinuxKey::RightArrow => Self::RightArrow,
            LinuxKey::ScrollLock => Self::ScrollLock,
            LinuxKey::Select => Self::Select,
            LinuxKey::Shift => Self::Shift,
            LinuxKey::ShiftLeft => Self::LShift,
            LinuxKey::ShiftRight => Self::RShift,
            LinuxKey::Space => Self::Space,
            LinuxKey::SuperLeft => Self::Meta,
            LinuxKey::Tab => Self::Tab,
            LinuxKey::UpArrow => Self::UpArrow,
            LinuxKey::VolumeDown => Self::VolumeDown,
            LinuxKey::VolumeMute => Self::VolumeMute,
            LinuxKey::VolumeUp => Self::VolumeUp,
            LinuxKey::Char(ch) => Self::Unicode(ch),
            other => Self::Other(linux_key_to_keysym(other).unwrap_or(0)),
        }
    }
}

#[cfg(target_os = "linux")]
pub fn vk_to_enigo_key(vk: u16) -> Option<enigo::Key> {
    vk_to_linux_key(vk).map(enigo::Key::from)
}

#[cfg(test)]
mod tests {
    use super::{LinuxKey, linux_key_to_keysym, vk_to_linux_key};

    #[test]
    fn maps_full_vk_range_without_panicking() {
        for vk in 1u16..=254 {
            let mapped = vk_to_linux_key(vk);
            if let Some(key) = mapped {
                let _ = linux_key_to_keysym(key);
            }
        }
        // Mouse VKs are handled via Input::Button, not keyboard keys.
        assert_eq!(vk_to_linux_key(0x01), None);
        assert_eq!(vk_to_linux_key(0x02), None);
        assert_eq!(vk_to_linux_key(0x04), None);
    }

    #[test]
    fn maps_alphanumeric_modifiers_function_and_oem_keys() {
        assert_eq!(vk_to_linux_key(0x41), Some(LinuxKey::Char('a')));
        assert_eq!(vk_to_linux_key(0x5A), Some(LinuxKey::Char('z')));
        assert_eq!(vk_to_linux_key(0x30), Some(LinuxKey::Char('0')));
        assert_eq!(vk_to_linux_key(0x39), Some(LinuxKey::Char('9')));

        assert_eq!(vk_to_linux_key(0x08), Some(LinuxKey::Backspace));
        assert_eq!(vk_to_linux_key(0x09), Some(LinuxKey::Tab));
        assert_eq!(vk_to_linux_key(0x0D), Some(LinuxKey::Return));
        assert_eq!(vk_to_linux_key(0x1B), Some(LinuxKey::Escape));
        assert_eq!(vk_to_linux_key(0x20), Some(LinuxKey::Space));

        assert_eq!(vk_to_linux_key(0x5B), Some(LinuxKey::SuperLeft));
        assert_eq!(vk_to_linux_key(0x5C), Some(LinuxKey::SuperRight));
        assert_eq!(vk_to_linux_key(0xA0), Some(LinuxKey::ShiftLeft));
        assert_eq!(vk_to_linux_key(0xA1), Some(LinuxKey::ShiftRight));
        assert_eq!(vk_to_linux_key(0xA2), Some(LinuxKey::ControlLeft));
        assert_eq!(vk_to_linux_key(0xA3), Some(LinuxKey::ControlRight));
        assert_eq!(vk_to_linux_key(0xA4), Some(LinuxKey::Alt));
        assert_eq!(vk_to_linux_key(0xA5), Some(LinuxKey::AltRight));
        assert_eq!(linux_key_to_keysym(LinuxKey::AltRight), Some(0xFFEA));
        assert_eq!(linux_key_to_keysym(LinuxKey::SuperRight), Some(0xFFEC));

        for idx in 1..=24 {
            assert_eq!(
                vk_to_linux_key(0x6F + u16::from(idx)),
                Some(LinuxKey::F(idx))
            );
        }
        for digit in 0..=9 {
            assert_eq!(
                vk_to_linux_key(0x60 + u16::from(digit)),
                Some(LinuxKey::Numpad(digit))
            );
        }

        assert_eq!(vk_to_linux_key(0xBA), Some(LinuxKey::Char(';')));
        assert_eq!(vk_to_linux_key(0xBB), Some(LinuxKey::Char('=')));
        assert_eq!(vk_to_linux_key(0xBC), Some(LinuxKey::Char(',')));
        assert_eq!(vk_to_linux_key(0xBD), Some(LinuxKey::Char('-')));
        assert_eq!(vk_to_linux_key(0xBE), Some(LinuxKey::Char('.')));
        assert_eq!(vk_to_linux_key(0xBF), Some(LinuxKey::Char('/')));
        assert_eq!(vk_to_linux_key(0xC0), Some(LinuxKey::Char('`')));
        assert_eq!(vk_to_linux_key(0xDB), Some(LinuxKey::Char('[')));
        assert_eq!(vk_to_linux_key(0xDC), Some(LinuxKey::Char('\\')));
        assert_eq!(vk_to_linux_key(0xDD), Some(LinuxKey::Char(']')));
        assert_eq!(vk_to_linux_key(0xDE), Some(LinuxKey::Char('\'')));
    }
}
