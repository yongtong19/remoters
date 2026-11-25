use std::io::{self, Read, Write};

pub(crate) const PAIR: u8 = 1;
pub(crate) const READY: u8 = 2;
pub(crate) const REQUEST_FRAME: u8 = 3;
pub(crate) const FRAME: u8 = 4;
pub(crate) const POINTER: u8 = 5;
pub(crate) const KEY: u8 = 6;
pub(crate) const SCROLL: u8 = 7;
pub(crate) const RELEASE: u8 = 8;
pub(crate) const ERROR: u8 = 9;
pub(crate) const DISCONNECT: u8 = 10;
pub(crate) const MAX_FRAME: usize = 8 * 1024 * 1024;

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

pub(crate) struct Packet {
    pub kind: u8,
    pub body: Vec<u8>,
}

pub(crate) fn read(stream: &mut impl Read, limit: usize) -> io::Result<Packet> {
    let mut header = [0; 5];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > limit {
        return Err(invalid("Packet exceeds length limit"));
    }
    let mut body = vec![0; len];
    stream.read_exact(&mut body)?;
    Ok(Packet {
        kind: header[0],
        body,
    })
}

pub(crate) fn write(stream: &mut impl Write, kind: u8, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME {
        return Err(invalid("Packet exceeds length limit"));
    }
    let len = (body.len() as u32).to_be_bytes();
    stream.write_all(&[kind, len[0], len[1], len[2], len[3]])?;
    stream.write_all(body)?;
    stream.flush()
}

impl Input {
    pub(crate) fn write(self, stream: &mut impl Write) -> io::Result<()> {
        match self {
            Self::Move(at) => write_pointer(stream, at, 0, Button::Left),
            Self::Button { at, button, down } => {
                write_pointer(stream, at, if down { 1 } else { 2 }, button)
            }
            Self::Key { code, down } => {
                let [a, b] = code.to_be_bytes();
                write(stream, KEY, &[a, b, u8::from(down)])
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
                write(stream, SCROLL, &[a, b, c, d, e, f, g, h])
            }
            Self::Release => write(stream, RELEASE, &[]),
        }
    }

    pub(crate) fn parse(packet: &Packet) -> io::Result<Self> {
        match (packet.kind, packet.body.as_slice()) {
            (POINTER, [a, b, c, d, action, button]) => {
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
            (KEY, [a, b, down @ 0..=1]) => {
                let code = u16::from_be_bytes([*a, *b]);
                if !(1..=254).contains(&code) {
                    return Err(invalid("Unsupported key code"));
                }
                Ok(Self::Key {
                    code,
                    down: *down == 1,
                })
            }
            (SCROLL, [a, b, c, d, e, f, g, h]) => Ok(Self::Scroll {
                at: point(*a, *b, *c, *d),
                vertical: i16::from_be_bytes([*e, *f]),
                horizontal: i16::from_be_bytes([*g, *h]),
            }),
            (RELEASE, []) => Ok(Self::Release),
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

fn write_pointer(stream: &mut impl Write, p: Point, action: u8, button: Button) -> io::Result<()> {
    let [a, b] = p.x.to_be_bytes();
    let [c, d] = p.y.to_be_bytes();
    write(stream, POINTER, &[a, b, c, d, action, button as u8])
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
