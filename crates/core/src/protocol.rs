use std::io;

pub(crate) const READY: u8 = 2;
pub(crate) const POINTER: u8 = 5;
pub(crate) const KEY: u8 = 6;
pub(crate) const SCROLL: u8 = 7;
pub(crate) const RELEASE: u8 = 8;
pub(crate) const DISCONNECT: u8 = 10;

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
