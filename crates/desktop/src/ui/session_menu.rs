use gpui::{Context, CursorStyle, Div, FontWeight, MouseButton, div, prelude::*, px, rgb, rgba};
use gpui_component::{Icon, IconName};

use crate::{Desktop, Viewer, i18n, metrics::format_latency};

use super::{
    session::ControlLayout,
    session_overlays::{quality_inset, stats_inset},
    theme::{ACCENT, CONTROL_MARGIN, DANGER, INK, MUTED, ONLINE, PILL_H, PILL_W, SUBTLE},
    widgets::quality_summary_label,
};

impl Desktop {
    pub(super) fn session_menu(
        &self,
        viewer: &Viewer,
        fullscreen: bool,
        layout: ControlLayout,
        panel_w: f32,
        panel_h: f32,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = i18n::tr();
        let ready = viewer.frame.is_some();
        let mode = if viewer.controlling {
            t.session_controlling
        } else if !ready {
            t.session_connecting
        } else if viewer.can_control {
            t.session_viewing
        } else {
            t.view_only
        };
        let open = viewer.session_menu_open;
        let toggle = div()
            .id("session-menu-toggle")
            .occlude()
            .cursor_pointer()
            .rounded_full()
            .px_3()
            .py(px(6.))
            .bg(rgba(0xF5F5F7E6))
            .hover(|style| style.bg(rgba(0xFFFFFFF5)))
            .border_1()
            .border_color(rgba(0x0000001C))
            .shadow_md()
            .flex()
            .items_center()
            .gap_2()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |desktop, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(viewer) = desktop.active_viewer_mut() {
                        viewer.menu_drag = Some(crate::MenuDrag {
                            pointer_start: (
                                f32::from(event.position.x),
                                f32::from(event.position.y),
                            ),
                            origin_start: (layout.pill_x, layout.pill_y),
                            moved: false,
                        });
                    }
                    cx.stop_propagation();
                }),
            )
            .child(if viewer.controlling {
                div()
                    .size(px(7.))
                    .rounded_full()
                    .bg(rgb(ONLINE))
                    .into_any_element()
            } else {
                Icon::new(IconName::Settings2)
                    .size(px(14.))
                    .text_color(rgb(INK))
                    .into_any_element()
            })
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(INK))
                    .child(if viewer.controlling {
                        t.session_controlling
                    } else {
                        t.control_pill
                    }),
            )
            .child(
                Icon::new(if open {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .size(px(12.))
                .text_color(rgb(MUTED)),
            );

        let container = div()
            .absolute()
            .cursor(CursorStyle::Arrow)
            .gap(px(6.))
            .when(layout.align_right, |el| {
                el.right(px((panel_w - layout.pill_x - PILL_W).max(CONTROL_MARGIN)))
                    .items_end()
            })
            .when(!layout.align_right, |el| {
                el.left(px(layout.pill_x)).items_start()
            })
            .when(layout.open_above, |el| {
                el.bottom(px((panel_h - layout.pill_y - PILL_H).max(CONTROL_MARGIN)))
                    .flex()
                    .flex_col_reverse()
            })
            .when(!layout.open_above, |el| {
                el.top(px(layout.pill_y)).flex().flex_col()
            });

        if !open {
            return container.child(toggle);
        }

        let shortcut = if cfg!(target_os = "macos") {
            "⌃⌥⎋"
        } else {
            "Ctrl+Alt+Esc"
        };
        let summary_latency = if viewer.telemetry.e2e_ms > 0.0 {
            viewer.telemetry.e2e_ms
        } else {
            viewer.telemetry.rtt_ms
        };
        let stats_summary = format!(
            "{:.0} fps · {}",
            viewer.telemetry.fps,
            format_latency(summary_latency)
        );
        let quality_summary = quality_summary_label(
            viewer.telemetry.codec,
            viewer.telemetry.hdr,
            viewer.active_bitrate_mbps,
        );
        let connected_gamepads = viewer.connection.connected_gamepads();
        let max_menu_h = if layout.open_above {
            (layout.pill_y - 6.0 - CONTROL_MARGIN).max(220.0)
        } else {
            (panel_h - layout.pill_y - PILL_H - 6.0 - CONTROL_MARGIN).max(220.0)
        };

        let menu =
            div()
                .id("session-control-menu")
                .occlude()
                .w(px(296.))
                .max_h(px(max_menu_h))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(2.))
                .p(px(6.))
                .rounded(px(14.))
                .shadow_lg()
                .bg(rgba(0xFAFAFCF2))
                .border_1()
                .border_color(rgba(0x0000001A))
                .child(
                    div()
                        .px(px(10.))
                        .py(px(6.))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(div().size(px(7.)).rounded_full().bg(rgb(
                                            if viewer.controlling { ONLINE } else { SUBTLE },
                                        )))
                                        .child(
                                            div()
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(INK))
                                                .child(mode),
                                        ),
                                )
                                .children(viewer.frame.as_ref().map(|frame| {
                                    div()
                                        .flex_none()
                                        .font_family("monospace")
                                        .text_color(rgb(SUBTLE))
                                        .child(format!("{}×{}", frame.width, frame.height))
                                })),
                        )
                        .child(
                            div()
                                .pl(px(15.))
                                .text_size(px(11.))
                                .font_family("monospace")
                                .text_color(rgb(MUTED))
                                .truncate()
                                .child(viewer.address.to_string()),
                        ),
                )
                .child(menu_divider())
                .when(ready && viewer.can_control, |menu| {
                    menu.child(
                        menu_row(
                            "take-control",
                            IconName::SquareTerminal,
                            if viewer.controlling {
                                t.release_input
                            } else {
                                t.take_control
                            },
                            false,
                            div()
                                .text_xs()
                                .font_family("monospace")
                                .text_color(rgb(SUBTLE))
                                .child(shortcut),
                        )
                        .on_click(cx.listener(|desktop, _, window, cx| {
                            let was_controlling = desktop.is_controlling();
                            if let Some(viewer) = desktop.active_viewer_mut() {
                                viewer.session_menu_open = false;
                            }
                            if was_controlling {
                                desktop.release(window, cx);
                            } else {
                                if let Some(viewer) = desktop.active_viewer_mut() {
                                    viewer.controlling = true;
                                }
                                window.focus(&desktop.remote_focus);
                                cx.notify();
                            }
                        })),
                    )
                })
                .when(ready, |menu| {
                    menu.child(
                        menu_row(
                            "session-stats-toggle",
                            IconName::ChartPie,
                            t.stats,
                            false,
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(rgb(SUBTLE))
                                .child(stats_summary)
                                .child(
                                    Icon::new(if viewer.stats_section_open {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size(px(12.))
                                    .text_color(rgb(SUBTLE)),
                                ),
                        )
                        .on_click(cx.listener(|desktop, _, _, cx| {
                            if let Some(viewer) = desktop.active_viewer_mut() {
                                viewer.stats_section_open = !viewer.stats_section_open;
                                if viewer.stats_section_open {
                                    viewer.telemetry = viewer.connection.telemetry();
                                }
                            }
                            cx.notify();
                        })),
                    )
                })
                .when(ready && viewer.stats_section_open, |menu| {
                    menu.child(stats_inset(&viewer.telemetry))
                })
                .when(ready, |menu| {
                    menu.child(
                        menu_row(
                            "session-quality-toggle",
                            IconName::Settings2,
                            t.quality,
                            false,
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_xs()
                                .text_color(rgb(SUBTLE))
                                .child(quality_summary)
                                .child(
                                    Icon::new(if viewer.quality_expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size(px(12.))
                                    .text_color(rgb(SUBTLE)),
                                ),
                        )
                        .on_click(cx.listener(|desktop, _, _, cx| {
                            if let Some(viewer) = desktop.active_viewer_mut() {
                                viewer.quality_expanded = !viewer.quality_expanded;
                                if viewer.quality_expanded {
                                    viewer.telemetry = viewer.connection.telemetry();
                                }
                            }
                            cx.notify();
                        })),
                    )
                })
                .when(ready && viewer.quality_expanded, |menu| {
                    menu.child(quality_inset(viewer, cx))
                })
                .when(ready, |menu| {
                    menu.child(
                        menu_row(
                            "session-stats-hud",
                            IconName::Inspector,
                            if viewer.stats_hud_open {
                                t.hide_stats_overlay
                            } else {
                                t.pin_stats_overlay
                            },
                            false,
                            div().when(viewer.stats_hud_open, |slot| {
                                slot.child(
                                    Icon::new(IconName::Check)
                                        .size(px(14.))
                                        .text_color(rgb(ACCENT)),
                                )
                            }),
                        )
                        .on_click(cx.listener(|desktop, _, window, cx| {
                            let mut refocus = false;
                            if let Some(viewer) = desktop.active_viewer_mut() {
                                viewer.stats_hud_open = !viewer.stats_hud_open;
                                if viewer.stats_hud_open {
                                    viewer.telemetry = viewer.connection.telemetry();
                                }
                                refocus = viewer.controlling;
                            }
                            if refocus {
                                window.focus(&desktop.remote_focus);
                            }
                            cx.notify();
                        })),
                    )
                })
                .when(ready && connected_gamepads > 0, |menu| {
                    menu.child(gamepad_status_row(connected_gamepads))
                })
                .when(ready, |menu| {
                    menu.child(
                        menu_row(
                            "session-audio-mute",
                            IconName::Bell,
                            if viewer.audio_muted {
                                t.unmute_audio
                            } else {
                                t.mute_audio
                            },
                            false,
                            div().when(viewer.audio_muted, |slot| {
                                slot.child(div().text_xs().text_color(rgb(SUBTLE)).child(t.muted))
                            }),
                        )
                        .on_click(cx.listener(|desktop, _, window, cx| {
                            let mut refocus = false;
                            if let Some(viewer) = desktop.active_viewer_mut() {
                                viewer.audio_muted = !viewer.audio_muted;
                                refocus = viewer.controlling;
                            }
                            desktop.sync_session_audio_mute();
                            if refocus {
                                window.focus(&desktop.remote_focus);
                            }
                            cx.notify();
                        })),
                    )
                })
                .when(ready, |menu| {
                    menu.child(menu_divider()).child(
                        menu_row(
                            "session-fullscreen",
                            if fullscreen {
                                IconName::Minimize
                            } else {
                                IconName::Maximize
                            },
                            if fullscreen {
                                t.exit_fullscreen
                            } else {
                                t.fullscreen
                            },
                            false,
                            div(),
                        )
                        .on_click(cx.listener(|desktop, _, window, cx| {
                            let mut refocus = false;
                            if let Some(viewer) = desktop.active_viewer_mut() {
                                viewer.session_menu_open = false;
                                refocus = viewer.controlling;
                            }
                            desktop.screen_bounds.set(None);
                            window.toggle_fullscreen();
                            if refocus {
                                window.focus(&desktop.remote_focus);
                            }
                            cx.notify();
                        })),
                    )
                })
                .child(menu_divider())
                .child(
                    menu_row(
                        "disconnect",
                        IconName::ExternalLink,
                        if ready { t.disconnect } else { t.cancel },
                        true,
                        div(),
                    )
                    .on_click(cx.listener(|desktop, _, window, cx| {
                        desktop.close_active_session_tab(window, cx);
                    })),
                );

        container
            .occlude()
            .on_mouse_down_out(cx.listener(|desktop, _, _, cx| {
                if let Some(viewer) = desktop.active_viewer_mut() {
                    viewer.session_menu_open = false;
                }
                cx.notify();
            }))
            .child(toggle)
            .child(menu)
    }
}

fn menu_divider() -> Div {
    div().h(px(1.)).mx(px(8.)).my(px(4.)).bg(rgba(0x00000012))
}

fn gamepad_status_row(count: u8) -> Div {
    div()
        .h(px(30.))
        .px(px(10.))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(div().size(px(7.)).mx(px(4.)).rounded_full().bg(rgb(ONLINE)))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(INK))
                        .child(i18n::tr().gamepad),
                ),
        )
        .child(
            div()
                .text_xs()
                .font_family("monospace")
                .text_color(rgb(MUTED))
                .child(i18n::fmt_gamepads_connected(count)),
        )
}

fn menu_row(
    id: &'static str,
    icon: IconName,
    label: &str,
    destructive: bool,
    trailing: impl IntoElement,
) -> gpui::Stateful<Div> {
    let tint = if destructive { DANGER } else { INK };
    div()
        .id(id)
        .h(px(34.))
        .px(px(10.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(move |style| style.bg(rgba(if destructive { 0xFF3B3014 } else { 0x0000000D })))
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .child(Icon::new(icon).size(px(15.)).text_color(rgb(tint)))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(tint))
                        .child(label.to_string()),
                ),
        )
        .child(trailing)
}
