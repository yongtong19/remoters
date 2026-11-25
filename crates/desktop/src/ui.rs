use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Corners, Div, FontWeight, MouseButton, Window,
    canvas, div, point, prelude::*, px, rgb, size,
};
use gpui_component::{
    Disableable, Sizable, Theme, ThemeMode, TitleBar,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    checkbox::Checkbox,
    input::Input,
    select::Select,
};

use crate::{Desktop, Role, Viewer};

const BACKGROUND: u32 = 0xEFF2EF;
const SIDEBAR: u32 = 0xFAFCF9;
const WHITE: u32 = 0xFFFFFF;
const INK: u32 = 0x192A23;
const MUTED: u32 = 0x52645A;
const BORDER: u32 = 0xD9E2DB;
const ACCENT: u32 = 0x146147;
const ACCENT_LIGHT: u32 = 0xE3F1E7;
const SCREEN: u32 = 0x102923;
const SCREEN_LINE: u32 = 0x31554A;
const SCREEN_INK: u32 = 0xC4DFD3;

pub fn configure_theme(cx: &mut App) {
    Theme::change(ThemeMode::Light, None, cx);
    let theme = Theme::global_mut(cx);
    theme.radius = px(8.);
    theme.colors.background = rgb(WHITE).into();
    theme.colors.foreground = rgb(INK).into();
    theme.colors.muted_foreground = rgb(MUTED).into();
    theme.colors.border = rgb(BORDER).into();
    theme.colors.input = rgb(BORDER).into();
    theme.colors.ring = rgb(ACCENT).into();
    theme.colors.primary = rgb(ACCENT).into();
    theme.colors.primary_hover = rgb(0x1B7456).into();
    theme.colors.primary_active = rgb(0x0F4D39).into();
    theme.colors.primary_foreground = rgb(WHITE).into();
    theme.colors.selection = rgb(0xD3F1DC).into();
    theme.colors.accent = rgb(ACCENT_LIGHT).into();
    theme.colors.title_bar = rgb(SIDEBAR).into();
    theme.colors.title_bar_border = rgb(BORDER).into();
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = f32::from(window.viewport_size().width) < 915.0;
        let sidebar_width = if narrow { 172. } else { 210. };
        let padding = if narrow { 22. } else { 36. };
        let content = match (self.role, self.viewer.as_ref(), self.sharing.is_some()) {
            (Role::Connect, Some(viewer), _) => self.session_panel(viewer, cx).into_any_element(),
            (Role::Connect, None, _) => self.connect_panel(narrow, cx).into_any_element(),
            (Role::Share, _, true) => self.invitation(narrow, cx).into_any_element(),
            (Role::Share, _, false) => self.share_panel(narrow, cx).into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(INK))
            .child(title_bar())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.sidebar(sidebar_width, cx))
                    .child(
                        div()
                            .id("workspace")
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .overflow_y_scroll()
                            .p(px(padding))
                            .child(content),
                    ),
            )
    }
}

fn title_bar() -> TitleBar {
    TitleBar::new()
        .h(px(42.))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size(px(24.))
                        .rounded(px(6.))
                        .bg(rgb(ACCENT))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(monitor_mark(14., 9., 0xDAF1E2)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("LAN Remote"),
                ),
        )
        .child(div().pr_3().child(pill("Local network", false)))
}

impl Desktop {
    fn sidebar(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let navigation = [
            (Role::Connect, "nav-connect", "Connect", "↗"),
            (Role::Share, "nav-share", "Share", "↙"),
        ];
        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(SIDEBAR))
            .border_r_1()
            .border_color(rgb(BORDER))
            .px_4()
            .py_5()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(navigation.into_iter().map(|(role, id, title, icon)| {
                        let selected = self.role == role;
                        let style = ButtonCustomVariant::new(cx)
                            .color(rgb(if selected { ACCENT_LIGHT } else { SIDEBAR }).into())
                            .foreground(rgb(if selected { ACCENT } else { MUTED }).into())
                            .hover(rgb(ACCENT_LIGHT).into())
                            .active(rgb(0xD4EADB).into());
                        Button::new(id)
                            .custom(style)
                            .w_full()
                            .justify_start()
                            .large()
                            .disabled(!self.idle())
                            .child(
                                div()
                                    .w_full()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(div().text_size(px(17.)).child(icon))
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(title)),
                            )
                            .on_click(cx.listener(move |desktop, _, _, cx| {
                                if desktop.idle() && desktop.role != role {
                                    desktop.role = role;
                                    desktop.status = None;
                                    cx.notify();
                                }
                            }))
                    })),
            )
    }

    fn connect_panel(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let form = card()
            .w(px(if narrow { 300. } else { 365. }))
            .flex_none()
            .gap_5()
            .child(field(
                "Host address",
                Input::new(&self.address).large().w_full(),
            ))
            .child(field(
                "Pairing code",
                Input::new(&self.code).large().w_full(),
            ))
            .children(self.notice())
            .child(
                Button::new("connect")
                    .primary()
                    .large()
                    .w_full()
                    .label("Connect")
                    .on_click(cx.listener(|desktop, _, _, cx| desktop.connect(cx))),
            );
        page(
            "Connect to a device",
            "Enter the address and pairing code shown on the host’s Share screen.",
        )
        .child(
            div().w_full().flex().gap_5().child(form).child(
                preview_panel(
                    "Remote display",
                    "The host screen appears here after you connect.",
                )
                .flex_1()
                .min_w_0(),
            ),
        )
    }

    fn share_panel(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let form = card()
            .w(px(if narrow { 300. } else { 395. }))
            .flex_none()
            .gap_5()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .when(narrow, |fields| fields.flex_col())
                    .child(field(
                        "Local IPv4 address",
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Select::new(&self.local_ip).large().flex_1().min_w_0())
                            .child(Button::new("refresh").label("Refresh").large().on_click(
                                cx.listener(|desktop, _, window, cx| {
                                    desktop.refresh_addresses(window, cx)
                                }),
                            )),
                    ))
                    .child(
                        div()
                            .w(px(88.))
                            .flex_none()
                            .child(field("Port", Input::new(&self.port).large().w_full())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        Checkbox::new("view-only")
                            .checked(self.view_only)
                            .label("View only")
                            .on_click(cx.listener(|desktop, value, _, cx| {
                                desktop.view_only = *value;
                                cx.notify();
                            })),
                    )
                    .child(muted("View-only guests can see your screen.").ml(px(23.))),
            )
            .children(self.notice())
            .child(
                Button::new("start-sharing")
                    .primary()
                    .large()
                    .w_full()
                    .label("Start sharing")
                    .on_click(cx.listener(|desktop, _, _, cx| desktop.share(cx))),
            );
        page(
            "Share this device",
            "Choose the address on the same local network as your guest.",
        )
        .child(
            div()
                .flex()
                .gap_5()
                .items_start()
                .child(form)
                .child(share_help().flex_1().min_w_0()),
        )
    }

    fn invitation(&self, narrow: bool, cx: &mut Context<Self>) -> Div {
        let sharing = self.sharing.as_ref().expect("active sharing panel");
        let address = sharing.host.address.to_string();
        let code = if self.reveal {
            sharing
                .host
                .code
                .as_bytes()
                .chunks(25)
                .map(String::from_utf8_lossy)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            "•••• •••• •••• ••••".to_string()
        };
        let peer = sharing.peer.map(|peer| peer.ip().to_string());
        let form =
            card()
                .w(px(if narrow { 325. } else { 455. }))
                .gap_4()
                .flex_none()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(18.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Invitation"),
                        )
                        .child(pill(
                            if peer.is_some() {
                                "Connected"
                            } else {
                                "Waiting for guest"
                            },
                            peer.is_some(),
                        )),
                )
                .child(invite_field(
                    "Host address",
                    address,
                    Button::new("copy-address")
                        .label("Copy")
                        .on_click(cx.listener(|desktop, _, _, cx| desktop.copy_invite(false, cx))),
                ))
                .child(invite_field(
                    "Pairing code",
                    code,
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("reveal-code")
                                .ghost()
                                .label(if self.reveal { "Hide" } else { "Show" })
                                .on_click(cx.listener(|desktop, _, _, cx| {
                                    desktop.reveal = !desktop.reveal;
                                    cx.notify();
                                })),
                        )
                        .child(Button::new("copy-code").label("Copy").on_click(
                            cx.listener(|desktop, _, _, cx| desktop.copy_invite(true, cx)),
                        )),
                ))
                .children(self.notice())
                .child(
                    div()
                        .pt_3()
                        .border_t_1()
                        .border_color(rgb(BORDER))
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(muted(if self.view_only {
                            "View-only access"
                        } else {
                            "Screen, keyboard and mouse"
                        }))
                        .child(
                            Button::new("stop-sharing")
                                .danger()
                                .label("Stop sharing")
                                .on_click(cx.listener(|desktop, _, _, cx| {
                                    desktop.sharing = None;
                                    desktop.reveal = false;
                                    desktop.status = Some(("Sharing stopped.".into(), false));
                                    cx.notify();
                                })),
                        ),
                );
        page(
            "Sharing this device",
            peer.map_or_else(
                || "Give the guest your address and pairing code.".to_string(),
                |peer| format!("Connected to {peer}."),
            ),
        )
        .child(
            div()
                .flex()
                .gap_5()
                .items_start()
                .child(form)
                .child(share_help().flex_1().min_w_0()),
        )
    }

    fn copy_invite(&mut self, pairing: bool, cx: &mut Context<Self>) {
        if let Some(sharing) = &self.sharing {
            let (value, message) = if pairing {
                (sharing.host.code.clone(), "Pairing code copied.")
            } else {
                (sharing.host.address.to_string(), "Address copied.")
            };
            cx.write_to_clipboard(ClipboardItem::new_string(value));
            self.status = Some((message.into(), false));
            cx.notify();
        }
    }

    fn notice(&self) -> Option<Div> {
        self.status.as_ref().map(|(message, error)| {
            div()
                .w_full()
                .rounded(px(9.))
                .px_3()
                .py_2()
                .text_size(px(13.))
                .bg(rgb(if *error { 0xFAEDEA } else { ACCENT_LIGHT }))
                .text_color(rgb(if *error { 0x8F342E } else { ACCENT }))
                .child(message.clone())
        })
    }

    fn session_panel(&self, viewer: &Viewer, cx: &mut Context<Self>) -> Div {
        let ready = viewer.frame.is_some();
        let mode = if self.controlling {
            "Controlling"
        } else if !ready {
            "Connecting"
        } else if viewer.can_control {
            "Viewing"
        } else {
            "View only"
        };
        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(25.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Remote session"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(muted(viewer.address.to_string()).font_family("monospace"))
                            .child(pill(mode, ready)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .when(ready && viewer.can_control, |el| {
                        el.child(
                            Button::new("take-control")
                                .primary()
                                .label(if self.controlling {
                                    "Release input"
                                } else {
                                    "Take control"
                                })
                                .on_click(cx.listener(|desktop, _, window, cx| {
                                    if desktop.controlling {
                                        desktop.release(window, cx);
                                    } else {
                                        desktop.controlling = true;
                                        window.focus(&desktop.remote_focus);
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .child(
                        Button::new("disconnect")
                            .label(if ready { "Disconnect" } else { "Cancel" })
                            .on_click(cx.listener(|desktop, _, window, cx| {
                                desktop.finish_viewer(window);
                                desktop.status = Some(("Disconnected.".into(), false));
                                cx.notify();
                            })),
                    ),
            );
        div()
            .flex_1()
            .min_h(px(350.))
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(toolbar)
            .when(self.controlling, |el| {
                el.child(muted(
                    "Press Ctrl + Alt / Option + Esc to release keyboard and mouse.",
                ))
            })
            .child(if let Some(frame) = &viewer.frame {
                self.video(frame, cx)
            } else {
                preview_panel(
                    "Waiting for the host",
                    "The remote display will appear here.",
                )
                .flex_1()
                .into_any_element()
            })
    }

    fn video(&self, frame: &crate::platform::VideoFrame, cx: &mut Context<Self>) -> AnyElement {
        let image = frame.image.clone();
        let dimensions = (frame.width as f32, frame.height as f32);
        let capture_bounds = self.screen_bounds.clone();
        let mut surface = div()
            .id("remote-screen")
            .flex_1()
            .min_h_0()
            .w_full()
            .relative()
            .rounded(px(17.))
            .bg(rgb(SCREEN))
            .overflow_hidden()
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
                            let _ = window.paint_image(
                                Bounds::new(origin, fitted),
                                Corners::default(),
                                image,
                                0,
                                false,
                            );
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
                    if desktop.controlling {
                        if let Some(at) =
                            desktop.coordinates(event.position, !desktop.pressed_buttons.is_empty())
                        {
                            desktop.send(lan_remote_core::Input::Move(at), window, cx);
                        }
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

fn page(title: &str, description: impl Into<gpui::SharedString>) -> Div {
    div().w_full().flex().flex_col().gap_5().child(
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_2()
            .pb_2()
            .child(
                div()
                    .text_size(px(34.))
                    .line_height(px(41.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title.to_string()),
            )
            .child(muted(description).text_size(px(15.))),
    )
}

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(17.))
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(WHITE))
        .p_5()
}

fn muted(message: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(13.))
        .text_color(rgb(MUTED))
        .child(message.into())
}

fn field(label: &str, element: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .min_w_0()
        .w_full()
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(label.to_string()),
        )
        .child(element)
}

fn pill(label: &str, active: bool) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .rounded_full()
        .py_1()
        .px_2()
        .bg(rgb(if active { ACCENT_LIGHT } else { 0xECF0EA }))
        .text_size(px(12.))
        .text_color(rgb(if active { ACCENT } else { MUTED }))
        .child(
            div()
                .size(px(6.))
                .rounded_full()
                .bg(rgb(if active { ACCENT } else { 0x87988D })),
        )
        .child(label.to_string())
}

fn monitor_mark(width: f32, height: f32, tint: u32) -> Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .w(px(width))
                .h(px(height))
                .rounded(px(width * 0.1))
                .border_1()
                .border_color(rgb(tint)),
        )
        .child(div().w(px(width * 0.08)).h(px(height * 0.19)).bg(rgb(tint)))
        .child(div().w(px(width * 0.37)).h(px(1.)).bg(rgb(tint)))
}

fn preview_panel(title: &str, detail: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .justify_center()
        .items_center()
        .min_h(px(295.))
        .p_5()
        .rounded(px(17.))
        .bg(rgb(SCREEN))
        .text_color(rgb(SCREEN_INK))
        .gap_4()
        .child(
            div()
                .size(px(106.))
                .rounded(px(25.))
                .bg(rgb(0x1A3A31))
                .border_1()
                .border_color(rgb(SCREEN_LINE))
                .flex()
                .items_center()
                .justify_center()
                .child(monitor_mark(52., 35., 0x8CC8AE)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(17.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(0xA2BFB1))
                        .text_center()
                        .child(detail.to_string()),
                ),
        )
}

fn invite_field(label: &str, value: String, controls: impl IntoElement) -> Div {
    field(
        label,
        div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(10.))
            .bg(rgb(0xF3F7F3))
            .child(
                div()
                    .text_size(px(13.))
                    .font_family("monospace")
                    .overflow_hidden()
                    .child(value),
            )
            .child(div().flex().justify_end().child(controls)),
    )
}

fn share_help() -> Div {
    card()
        .gap_3()
        .child(
            div()
                .text_size(px(18.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("On the guest device"),
        )
        .child(muted("Open LAN Remote and select Connect."))
        .child(muted(
            "Enter the host address and pairing code from this screen.",
        ))
}
