use std::io;

pub(crate) const READY: u8 = 2;
pub(crate) const POINTER: u8 = 5;
pub(crate) const KEY: u8 = 6;
pub(crate) const SCROLL: u8 = 7;
pub(crate) const RELEASE: u8 = 8;
pub(crate) const DISCONNECT: u8 = 10;
pub(crate) const GAMEPAD: u8 = 11;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GamepadState {
    pub slot: u8,
    pub connected: bool,
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

impl GamepadState {
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Point {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    Move(Point),
    Button {
        at: Point,
        button: Button,
        down: bool,
    },
    Key {
        code: u16,
        down: bool,
    },
    Scroll {
        at: Point,
        vertical: i16,
        horizontal: i16,
    },
    Release,
    Gamepad(GamepadState),
}

impl Input {
    pub(crate) fn encode(self) -> Vec<u8> {
        match self {
            Self::Move(at) => pointer(at, 0, Button::Left),
            Self::Button { at, button, down } => pointer(at, if down { 1 } else { 2 }, button),
            Self::Key { code, down } => {
                let [a, b] = code.to_be_bytes();
                vec![KEY, a, b, u8::from(down)]
            }
            Self::Scroll {
                at,
                vertical,
                horizontal,
            } => {
                let [a, b] = at.x.to_be_bytes();
                let [c, d] = at.y.to_be_bytes();
                let [e, f] = vertical.to_be_bytes();
                let [g, h] = horizontal.to_be_bytes();
                vec![SCROLL, a, b, c, d, e, f, g, h]
            }
            Self::Release => vec![RELEASE],
            Self::Gamepad(state) => {
                let [b0, b1] = state.buttons.to_be_bytes();
                let [lx0, lx1] = state.left_x.to_be_bytes();
                let [ly0, ly1] = state.left_y.to_be_bytes();
                let [rx0, rx1] = state.right_x.to_be_bytes();
                let [ry0, ry1] = state.right_y.to_be_bytes();
                vec![
                    GAMEPAD,
                    state.slot,
                    u8::from(state.connected),
                    b0,
                    b1,
                    state.left_trigger,
                    state.right_trigger,
                    lx0,
                    lx1,
                    ly0,
                    ly1,
                    rx0,
                    rx1,
                    ry0,
                    ry1,
                ]
            }
        }
    }

    pub(crate) fn parse(bytes: &[u8]) -> io::Result<Self> {
        match bytes {
            [POINTER, a, b, c, d, action, button] => {
                let at = point(*a, *b, *c, *d);
                let button = match button {
                    0 => Button::Left,
                    1 => Button::Right,
                    2 => Button::Middle,
                    _ => return Err(invalid("Unsupported mouse button")),
                };
                match action {
                    0 => Ok(Self::Move(at)),
                    1 | 2 => Ok(Self::Button {
                        at,
                        button,
                        down: *action == 1,
                    }),
                    _ => Err(invalid("Unsupported pointer action")),
                }
            }
            [KEY, a, b, down @ 0..=1] => {
                let code = u16::from_be_bytes([*a, *b]);
                if !(1..=254).contains(&code) {
                    return Err(invalid("Unsupported key code"));
                }
                Ok(Self::Key {
                    code,
                    down: *down == 1,
                })
            }
            [SCROLL, a, b, c, d, e, f, g, h] => Ok(Self::Scroll {
                at: point(*a, *b, *c, *d),
                vertical: i16::from_be_bytes([*e, *f]),
                horizontal: i16::from_be_bytes([*g, *h]),
            }),
            [RELEASE] => Ok(Self::Release),
            [
                GAMEPAD,
                slot,
                connected @ 0..=1,
                b0,
                b1,
                left_trigger,
                right_trigger,
                lx0,
                lx1,
                ly0,
                ly1,
                rx0,
                rx1,
                ry0,
                ry1,
            ] => {
                if *slot > 3 {
                    return Err(invalid("Unsupported gamepad slot"));
                }
                Ok(Self::Gamepad(GamepadState {
                    slot: *slot,
                    connected: *connected == 1,
                    buttons: u16::from_be_bytes([*b0, *b1]),
                    left_trigger: *left_trigger,
                    right_trigger: *right_trigger,
                    left_x: i16::from_be_bytes([*lx0, *lx1]),
                    left_y: i16::from_be_bytes([*ly0, *ly1]),
                    right_x: i16::from_be_bytes([*rx0, *rx1]),
                    right_y: i16::from_be_bytes([*ry0, *ry1]),
                }))
            }
            _ => Err(invalid("Unexpected control packet")),
        }
    }
}

fn point(a: u8, b: u8, c: u8, d: u8) -> Point {
    Point {
        x: u16::from_be_bytes([a, b]),
        y: u16::from_be_bytes([c, d]),
    }
}

fn pointer(p: Point, action: u8, button: Button) -> Vec<u8> {
    let [a, b] = p.x.to_be_bytes();
    let [c, d] = p.y.to_be_bytes();
    vec![POINTER, a, b, c, d, action, button as u8]
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamepad_encode_parse_roundtrip() {
        let cases = [
            GamepadState {
                slot: 0,
                connected: true,
                buttons: GamepadState::A
                    | GamepadState::B
                    | GamepadState::X
                    | GamepadState::Y
                    | GamepadState::DPAD_UP
                    | GamepadState::START
                    | GamepadState::LEFT_SHOULDER
                    | GamepadState::RIGHT_THUMB
                    | GamepadState::GUIDE,
                left_trigger: 0xff,
                right_trigger: 128,
                left_x: i16::MIN,
                left_y: i16::MAX,
                right_x: -12345,
                right_y: 23456,
            },
            GamepadState {
                slot: 3,
                connected: false,
                buttons: GamepadState::DPAD_DOWN
                    | GamepadState::DPAD_LEFT
                    | GamepadState::DPAD_RIGHT
                    | GamepadState::BACK
                    | GamepadState::LEFT_THUMB
                    | GamepadState::RIGHT_SHOULDER,
                left_trigger: 0,
                right_trigger: 1,
                left_x: 0,
                left_y: -1,
                right_x: 1,
                right_y: i16::MIN,
            },
        ];

        for state in cases {
            let encoded = Input::Gamepad(state).encode();
            assert_eq!(encoded.len(), 15);
            assert_eq!(encoded[0], GAMEPAD);
            let parsed = Input::parse(&encoded).expect("valid gamepad packet");
            assert_eq!(parsed, Input::Gamepad(state));
        }
    }

    #[test]
    fn gamepad_rejects_invalid_slot_connected_and_length() {
        let mut bytes = Input::Gamepad(GamepadState {
            slot: 0,
            connected: true,
            ..GamepadState::default()
        })
        .encode();

        bytes[1] = 4;
        assert!(Input::parse(&bytes).is_err());

        bytes[1] = 255;
        assert!(Input::parse(&bytes).is_err());

        bytes[1] = 1;
        bytes[2] = 2;
        assert!(Input::parse(&bytes).is_err());

        bytes[2] = 1;
        assert!(Input::parse(&bytes[..14]).is_err());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(Input::parse(&longer).is_err());
    }
}
