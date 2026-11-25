use std::collections::HashSet;

use anyhow::{Result, anyhow};
use core_graphics::{
    event::{CGEvent, CGEventFlags, CGEventTapLocation, CGEventType, CGMouseButton, EventField},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};
use objc2::{ffi, runtime::AnyClass, sel};

/// GPUI 0.2.2 converts these AppKit events but only registers the left-drag
/// selector on its view. Add the missing selectors using the same handler.
pub fn install_drag_handlers() {
    let Some(view) = AnyClass::get(c"GPUIView") else {
        return;
    };
    let Some(handler) = view.instance_method(sel!(mouseDragged:)) else {
        return;
    };
    for selector in [sel!(otherMouseDragged:), sel!(rightMouseDragged:)] {
        // SAFETY: Each selector has the same one-NSEvent-argument ABI as
        // mouseDragged:. The GPUI handler reads the event's type and ignores
        // the selector. class_addMethod preserves implementations already
        // registered on GPUIView.
        unsafe {
            ffi::class_addMethod(
                std::ptr::from_ref(view).cast_mut(),
                selector,
                handler.implementation(),
                ffi::method_getTypeEncoding(handler),
            );
        }
    }
}

pub(super) struct MiddleMouse {
    source: CGEventSource,
    marker: i64,
    pub(super) position: (i32, i32),
}

impl MiddleMouse {
    pub(super) fn new(marker: i64) -> Result<Self> {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
            .map_err(|_| anyhow!("Could not create a macOS mouse event source."))?;
        Ok(Self {
            source,
            marker,
            position: (0, 0),
        })
    }

    pub(super) fn button(&self, down: bool, keys: &HashSet<u16>) -> Result<()> {
        let kind = if down {
            CGEventType::OtherMouseDown
        } else {
            CGEventType::OtherMouseUp
        };
        let event = self.event(kind, self.position, keys)?;
        event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, 1);
        event.post(CGEventTapLocation::HID);
        Ok(())
    }

    pub(super) fn drag(&mut self, position: (i32, i32), keys: &HashSet<u16>) -> Result<()> {
        let event = self.event(CGEventType::OtherMouseDragged, position, keys)?;
        event.set_integer_value_field(
            EventField::MOUSE_EVENT_DELTA_X,
            i64::from(position.0) - i64::from(self.position.0),
        );
        event.set_integer_value_field(
            EventField::MOUSE_EVENT_DELTA_Y,
            i64::from(position.1) - i64::from(self.position.1),
        );
        event.post(CGEventTapLocation::HID);
        self.position = position;
        Ok(())
    }

    fn event(
        &self,
        kind: CGEventType,
        position: (i32, i32),
        keys: &HashSet<u16>,
    ) -> Result<CGEvent> {
        let event = CGEvent::new_mouse_event(
            self.source.clone(),
            kind,
            CGPoint::new(f64::from(position.0), f64::from(position.1)),
            CGMouseButton::Center,
        )
        .map_err(|_| anyhow!("Could not create a macOS middle-mouse event."))?;
        let mut flags = CGEventFlags::empty();
        for (code, flag) in [
            (0xA0, CGEventFlags::CGEventFlagShift),
            (0xA2, CGEventFlags::CGEventFlagControl),
            (0xA4, CGEventFlags::CGEventFlagAlternate),
            (0x5B, CGEventFlags::CGEventFlagCommand),
        ] {
            if keys.contains(&code) {
                flags.insert(flag);
            }
        }
        event.set_flags(flags);
        event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, 2);
        event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, self.marker);
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_drag_keeps_its_button_coordinate_and_modifier() -> Result<()> {
        let mouse = MiddleMouse::new(21)?;
        let event = mouse.event(
            CGEventType::OtherMouseDragged,
            (43, 95),
            &HashSet::from([0xA0]),
        )?;
        assert!(matches!(event.get_type(), CGEventType::OtherMouseDragged));
        assert_eq!(
            event.get_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER),
            2
        );
        assert_eq!((event.location().x, event.location().y), (43., 95.));
        assert!(event.get_flags().contains(CGEventFlags::CGEventFlagShift));
        Ok(())
    }
}
