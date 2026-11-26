use anyhow::{Context, Result, anyhow};
use enigo::{Coordinate, Direction, Enigo, Keyboard, Mouse, Settings};

use super::keymap::vk_to_enigo_key;

#[allow(dead_code)]
pub struct InputInjector {
    enigo: Enigo,
}

#[allow(dead_code)]
impl InputInjector {
    pub fn new() -> Result<Self> {
        let settings = Settings {
            open_prompt_to_get_permissions: false,
            ..Settings::default()
        };
        let enigo = Enigo::new(&settings)
            .map_err(|err| anyhow!("Input permission is unavailable: {err}"))?;
        Ok(Self { enigo })
    }

    pub fn move_mouse_abs(&mut self, x: i32, y: i32) -> Result<()> {
        move_pointer(&mut self.enigo, x, y)
    }

    pub fn key(&mut self, vk: u16, direction: Direction) -> Result<bool> {
        inject_key(&mut self.enigo, vk, direction)
    }

    pub fn button(&mut self, button: enigo::Button, direction: Direction) -> Result<()> {
        self.enigo
            .button(button, direction)
            .context("Failed to inject mouse button on Linux.")
    }

    pub fn scroll(&mut self, length: i32, axis: enigo::Axis) -> Result<()> {
        self.enigo
            .scroll(length, axis)
            .context("Failed to inject scroll on Linux.")
    }
}

pub fn move_pointer(input: &mut Enigo, x: i32, y: i32) -> Result<()> {
    input
        .move_mouse(x.max(0), y.max(0), Coordinate::Abs)
        .map_err(|err| anyhow!("Linux rejected pointer movement: {err}"))
}

pub fn inject_key(input: &mut Enigo, code: u16, direction: Direction) -> Result<bool> {
    let Some(key) = vk_to_enigo_key(code) else {
        return Ok(false);
    };
    input
        .key(key, direction)
        .map_err(|err| anyhow!("Linux key injection failed for VK {code:#04x}: {err}"))?;
    Ok(true)
}
