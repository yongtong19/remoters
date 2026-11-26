mod cli;
mod clipboard;
mod discovery;
mod input;
mod session_lifecycle;
mod state;

pub(crate) use cli::{DesktopAssets, StartupOptions};
pub(crate) use state::{ActiveTab, Desktop, MenuDrag, Role, SessionTab, SessionTabStatus, Viewer};
