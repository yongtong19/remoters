use gpui::{
    AnyElement, Bounds, Context, CursorStyle, Div, FontWeight, MouseButton, Window, canvas, div,
    point, prelude::*, px, rgb, rgba, size,
};
use gpui_component::{
    IconName,
    button::{Button, ButtonVariants},
};
use remoters_core::session::CursorShape;

use crate::{Desktop, SessionTab, i18n};

use super::{
    session_overlays::stats_hud,
    theme::{
        ACCENT, CONTROL_MARGIN, DANGER, INK, PILL_H, PILL_W, SESSION_BACKGROUND, SUBTLE, TITLEBAR_H,
    },
    widgets::monitor_mark,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ControlLayout {
    pub(super) pill_x: f32,
    pub(super) pill_y: f32,
    pub(super) align_right: bool,
    pub(super) open_above: bool,
}

impl Desktop {
    pub(super) fn session_panel(
        &self,
        tab: &SessionTab,
        window: &Window,
        fullscreen: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<Div> {
        let viewer = &tab.viewer;
        let viewport = window.viewport_size();
        let panel_w = f32::from(viewport.width).max(320.0);
        let panel_h =
            (f32::from(viewport.height) - if fullscreen { 0.0 } else { TITLEBAR_H }).max(240.0);
        let layout = layout_control_pill(
            viewer.menu_pos,
            panel_w,
            panel_h,
            viewer.stats_section_open,
            viewer.quality_expanded,
        );
        let hud_on_right = layout.pill_x < 260.0 && layout.pill_y < 190.0;

        let tab_id = tab.id;
        div()
            .id(("session-panel", tab_id))
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .flex()
            .bg(rgb(SESSION_BACKGROUND))
            .text_color(rgb(INK))
            .drag_over::<gpui::ExternalPaths>(|style, _, _, _| {
                style.border_2().border_color(rgb(ACCENT))
            })
            .on_drop(cx.listener(|desktop, paths: &gpui::ExternalPaths, _, cx| {
                desktop.drop_files(paths, cx)
            }))
            .on_mouse_move(
                cx.listener(move |desktop, event: &gpui::MouseMoveEvent, _, cx| {
                    let Some(viewer) = desktop.active_viewer_mut() else {
                        return;
                    };
                    let Some(mut drag) = viewer.menu_drag else {
                        return;
                    };
                    let dx = f32::from(event.position.x) - drag.pointer_start.0;
                    let dy = f32::from(event.position.y) - drag.pointer_start.1;
                    if !drag.moved && (dx * dx + dy * dy) >= 16.0 {
                        drag.moved = true;
                        viewer.session_menu_open = false;
                    }
                    if drag.moved {
                        viewer.menu_pos = Some(clamp_control_pos(
                            drag.origin_start.0 + dx,
                            drag.origin_start.1 + dy,
                            panel_w,
                            panel_h,
                        ));
                    }
                    viewer.menu_drag = Some(drag);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|desktop, _, window, cx| {
                    desktop.finish_control_drag(window, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|desktop, _, window, cx| {
                    desktop.finish_control_drag(window, cx);
                }),
            )
            .child(if let Some((message, is_error)) = &tab.closed_notice {
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .px_6()
                    .child(monitor_mark(
                        28.,
                        18.,
                        if *is_error { DANGER } else { SUBTLE },
                    ))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(0xFFFFFF))
                            .child(tab.title.clone()),
                    )
                    .child(
                        div()
                            .max_w(px(440.))
                            .text_xs()
                            .text_center()
                            .text_color(rgb(if *is_error { DANGER } else { SUBTLE }))
                            .child(message.clone()),
                    )
                    .child(
                        Button::new(("close-closed-tab", tab_id))
                            .primary()
                            .icon(IconName::Close)
                            .label(i18n::tr().close_tab)
                            .on_click(cx.listener(move |desktop, _, window, cx| {
                                desktop.close_session_tab(tab_id, window, cx);
                            })),
                    )
                    .into_any_element()
            } else if let Some(frame) = &viewer.frame {
                self.video(frame, viewer, cx)
            } else {
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .text_color(rgb(SUBTLE))
                    .child(monitor_mark(22., 14., SUBTLE))
                    .child(i18n::tr().waiting_for_remote_display)
                    .into_any_element()
            })
            .when(
                tab.closed_notice.is_none() && viewer.stats_hud_open && viewer.frame.is_some(),
                |panel| {
                    panel.child(
                        div()
                            .absolute()
                            .top(px(10.))
                            .when(hud_on_right, |el| el.right(px(14.)))
                            .when(!hud_on_right, |el| el.left(px(14.)))
                            .child(stats_hud(&viewer.telemetry)),
                    )
                },
            )
            .when(tab.closed_notice.is_none(), |panel| {
                panel.child(self.session_menu(viewer, fullscreen, layout, panel_w, panel_h, cx))
            })
            .when_some(
                viewer.transfer_notice.as_ref(),
                |panel, (message, error, _, _)| {
                    panel.child(
                        div()
                            .absolute()
                            .bottom(px(16.))
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                div()
                                    .rounded_full()
                                    .px_4()
                                    .py_2()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .shadow_md()
                                    .border_1()
                                    .border_color(rgba(if *error {
                                        0xFF3B304D
                                    } else {
                                        0x00000018
                                    }))
                                    .bg(rgba(if *error { 0xFFF2F0F2 } else { 0xFAFAFCF0 }))
                                    .text_color(rgb(if *error { DANGER } else { INK }))
                                    .child(message.clone()),
                            ),
                    )
                },
            )
    }

    fn finish_control_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.active_viewer_mut() else {
            return;
        };
        let Some(drag) = viewer.menu_drag.take() else {
            return;
        };
        if !drag.moved {
            viewer.session_menu_open = !viewer.session_menu_open;
            if viewer.session_menu_open {
                viewer.telemetry = viewer.connection.telemetry();
            }
        }
        let controlling = viewer.controlling;
        if controlling {
            window.focus(&self.remote_focus);
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn video(
        &self,
        frame: &crate::platform::VideoFrame,
        viewer: &crate::Viewer,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface = frame.surface.clone();
        let dimensions = (frame.width as f32, frame.height as f32);
        let capture_bounds = self.screen_bounds.clone();
        let cursor_style = if viewer.controlling && viewer.menu_drag.is_none() {
            cursor_style_for(viewer.remote_cursor)
        } else {
            CursorStyle::Arrow
        };
        let mut surface = div()
            .id("remote-screen")
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .bg(rgb(SESSION_BACKGROUND))
            .overflow_hidden()
            .cursor(cursor_style)
            .track_focus(&self.remote_focus)
            .child(
                canvas(
                    move |bounds, _, _| capture_bounds.set(Some(bounds)),
                    move |bounds, _, window, _| {
                        let scale = (f32::from(bounds.size.width) / dimensions.0)
                            .min(f32::from(bounds.size.height) / dimensions.1);
                        if scale > 0.0 {
                            let fitted = size(px(dimensions.0 * scale), px(dimensions.1 * scale));
                            let origin = bounds.origin
                                + point(
                                    (bounds.size.width - fitted.width) / 2.0,
                                    (bounds.size.height - fitted.height) / 2.0,
                                );
                            #[cfg(target_os = "windows")]
                            window.paint_surface(Bounds::new(origin, fitted), surface);
                            #[cfg(target_os = "macos")]
                            {
                                use core_foundation::base::TCFType;
                                // SAFETY: The capture wrapper owns this CVPixelBuffer.
                                // GPUI needs a retained reference to the same IOSurface.
                                let buffer = unsafe {
                                    core_video::pixel_buffer::CVPixelBuffer::wrap_under_get_rule(
                                        surface.as_ptr().cast(),
                                    )
                                };
                                window.paint_surface(Bounds::new(origin, fitted), buffer);
                            }
                            #[cfg(target_os = "linux")]
                            {
                                let _ = window.paint_image(
                                    Bounds::new(origin, fitted),
                                    gpui::Corners::default(),
                                    surface,
                                    0,
                                    false,
                                );
                            }
                        }
                    },
                )
                .size_full(),
            )
            .on_key_down(
                cx.listener(|desktop, event: &gpui::KeyDownEvent, window, cx| {
                    desktop.key(&event.keystroke, true, window, cx)
                }),
            )
            .on_key_up(
                cx.listener(|desktop, event: &gpui::KeyUpEvent, window, cx| {
                    desktop.key(&event.keystroke, false, window, cx)
                }),
            )
            .on_modifiers_changed(cx.listener(
                |desktop, event: &gpui::ModifiersChangedEvent, window, cx| {
                    desktop.change_modifiers(event.modifiers, window, cx)
                },
            ))
            .on_mouse_move(
                cx.listener(|desktop, event: &gpui::MouseMoveEvent, window, cx| {
                    if let Some(viewer) = desktop.active_viewer()
                        && viewer.controlling
                        && viewer.menu_drag.is_none()
                        && let Some(at) =
                            desktop.coordinates(event.position, !desktop.pressed_buttons.is_empty())
                    {
                        desktop.send(remoters_core::Input::Move(at), window, cx);
                    }
                }),
            )
            .on_scroll_wheel(
                cx.listener(|desktop, event, window, cx| desktop.scroll(event, window, cx)),
            );

        for (local, remote) in [
            (MouseButton::Left, crate::Button::Left),
            (MouseButton::Right, crate::Button::Right),
            (MouseButton::Middle, crate::Button::Middle),
        ] {
            surface = surface
                .on_mouse_down(
                    local,
                    cx.listener(move |desktop, event: &gpui::MouseDownEvent, window, cx| {
                        desktop.mouse_button(remote, true, event.position, window, cx);
                    }),
                )
                .on_mouse_up(
                    local,
                    cx.listener(move |desktop, event: &gpui::MouseUpEvent, window, cx| {
                        desktop.mouse_button(remote, false, event.position, window, cx);
                    }),
                )
                .on_mouse_up_out(
                    local,
                    cx.listener(move |desktop, event: &gpui::MouseUpEvent, window, cx| {
                        desktop.mouse_button(remote, false, event.position, window, cx);
                    }),
                );
        }
        surface.into_any_element()
    }
}

fn clamp_control_pos(x: f32, y: f32, panel_w: f32, panel_h: f32) -> (f32, f32) {
    let max_x = (panel_w - PILL_W - CONTROL_MARGIN).max(CONTROL_MARGIN);
    let max_y = (panel_h - PILL_H - CONTROL_MARGIN).max(CONTROL_MARGIN);
    (
        x.clamp(CONTROL_MARGIN, max_x),
        y.clamp(CONTROL_MARGIN, max_y),
    )
}

fn layout_control_pill(
    pos: Option<(f32, f32)>,
    panel_w: f32,
    panel_h: f32,
    stats_open: bool,
    quality_open: bool,
) -> ControlLayout {
    let default = (
        (panel_w - PILL_W - 14.0).max(CONTROL_MARGIN),
        CONTROL_MARGIN,
    );
    let (pill_x, pill_y) = match pos {
        Some((x, y)) => clamp_control_pos(x, y, panel_w, panel_h),
        None => default,
    };
    let align_right = pill_x + PILL_W / 2.0 >= panel_w / 2.0;
    let estimated_menu_h =
        286.0 + if stats_open { 160.0 } else { 0.0 } + if quality_open { 340.0 } else { 0.0 };
    let open_above = pill_y + PILL_H + 6.0 + estimated_menu_h > panel_h - CONTROL_MARGIN
        && pill_y > panel_h / 2.0;
    ControlLayout {
        pill_x,
        pill_y,
        align_right,
        open_above,
    }
}

#[cfg(target_os = "linux")]
pub(super) fn sync_linux_video_atlas(viewer: Option<&crate::Viewer>, window: &mut Window) {
    use std::{cell::RefCell, sync::Arc};

    thread_local! {
        static ACTIVE_IMAGE: RefCell<Option<Arc<gpui::RenderImage>>> = const { RefCell::new(None) };
    }

    let current = viewer
        .and_then(|v| v.frame.as_ref())
        .map(|frame| frame.surface.clone());

    ACTIVE_IMAGE.with(|cell| {
        let mut slot = cell.borrow_mut();
        let changed = match (&*slot, &current) {
            (Some(prev), Some(next)) => prev.id != next.id,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        if changed {
            if let Some(prev) = slot.take() {
                let _ = window.drop_image(prev);
            }
            *slot = current;
        }
    });
}

pub(crate) fn cursor_style_for(shape: CursorShape) -> CursorStyle {
    match shape {
        CursorShape::Arrow => CursorStyle::Arrow,
        CursorShape::IBeam => CursorStyle::IBeam,
        CursorShape::Crosshair => CursorStyle::Crosshair,
        CursorShape::ClosedHand => CursorStyle::ClosedHand,
        CursorShape::OpenHand => CursorStyle::OpenHand,
        CursorShape::PointingHand => CursorStyle::PointingHand,
        CursorShape::ResizeLeft => CursorStyle::ResizeLeft,
        CursorShape::ResizeRight => CursorStyle::ResizeRight,
        CursorShape::ResizeLeftRight => CursorStyle::ResizeLeftRight,
        CursorShape::ResizeUp => CursorStyle::ResizeUp,
        CursorShape::ResizeDown => CursorStyle::ResizeDown,
        CursorShape::ResizeUpDown => CursorStyle::ResizeUpDown,
        CursorShape::ResizeUpLeftDownRight => CursorStyle::ResizeUpLeftDownRight,
        CursorShape::ResizeUpRightDownLeft => CursorStyle::ResizeUpRightDownLeft,
        CursorShape::ResizeColumn => CursorStyle::ResizeColumn,
        CursorShape::ResizeRow => CursorStyle::ResizeRow,
        CursorShape::IBeamVertical => CursorStyle::IBeamCursorForVerticalLayout,
        CursorShape::OperationNotAllowed => CursorStyle::OperationNotAllowed,
        CursorShape::DragLink => CursorStyle::DragLink,
        CursorShape::DragCopy => CursorStyle::DragCopy,
        CursorShape::ContextualMenu => CursorStyle::ContextualMenu,
        CursorShape::Hidden => CursorStyle::None,
    }
}

#[cfg(test)]
mod tests {
    use gpui::CursorStyle;
    use remoters_core::session::CursorShape;

    use super::{
        CONTROL_MARGIN, PILL_H, PILL_W, clamp_control_pos, cursor_style_for, layout_control_pill,
    };

    #[test]
    fn maps_every_cursor_shape_to_gpui_cursor_style() {
        let cases = [
            (CursorShape::Arrow, CursorStyle::Arrow),
            (CursorShape::IBeam, CursorStyle::IBeam),
            (CursorShape::Crosshair, CursorStyle::Crosshair),
            (CursorShape::ClosedHand, CursorStyle::ClosedHand),
            (CursorShape::OpenHand, CursorStyle::OpenHand),
            (CursorShape::PointingHand, CursorStyle::PointingHand),
            (CursorShape::ResizeLeft, CursorStyle::ResizeLeft),
            (CursorShape::ResizeRight, CursorStyle::ResizeRight),
            (CursorShape::ResizeLeftRight, CursorStyle::ResizeLeftRight),
            (CursorShape::ResizeUp, CursorStyle::ResizeUp),
            (CursorShape::ResizeDown, CursorStyle::ResizeDown),
            (CursorShape::ResizeUpDown, CursorStyle::ResizeUpDown),
            (
                CursorShape::ResizeUpLeftDownRight,
                CursorStyle::ResizeUpLeftDownRight,
            ),
            (
                CursorShape::ResizeUpRightDownLeft,
                CursorStyle::ResizeUpRightDownLeft,
            ),
            (CursorShape::ResizeColumn, CursorStyle::ResizeColumn),
            (CursorShape::ResizeRow, CursorStyle::ResizeRow),
            (
                CursorShape::IBeamVertical,
                CursorStyle::IBeamCursorForVerticalLayout,
            ),
            (
                CursorShape::OperationNotAllowed,
                CursorStyle::OperationNotAllowed,
            ),
            (CursorShape::DragLink, CursorStyle::DragLink),
            (CursorShape::DragCopy, CursorStyle::DragCopy),
            (CursorShape::ContextualMenu, CursorStyle::ContextualMenu),
            (CursorShape::Hidden, CursorStyle::None),
        ];
        for (shape, expected) in cases {
            assert_eq!(cursor_style_for(shape), expected);
        }
    }

    #[test]
    fn control_pill_layout_defaults_clamps_and_flips_by_quadrant() {
        let def = layout_control_pill(None, 1110.0, 665.0, true, false);
        assert!((def.pill_x - (1110.0 - PILL_W - 14.0)).abs() < 0.01);
        assert_eq!(def.pill_y, CONTROL_MARGIN);
        assert!(def.align_right);
        assert!(!def.open_above);

        let clamped = clamp_control_pos(-50.0, 2000.0, 1110.0, 665.0);
        assert_eq!(clamped.0, CONTROL_MARGIN);
        assert!((clamped.1 - (665.0 - PILL_H - CONTROL_MARGIN)).abs() < 0.01);

        let left_bottom = layout_control_pill(Some((24.0, 600.0)), 1110.0, 665.0, true, true);
        assert!(!left_bottom.align_right);
        assert!(left_bottom.open_above);
    }
}
