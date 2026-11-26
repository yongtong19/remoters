use gpui::{
    AnyElement, Context, Corner, Div, FontWeight, MouseButton, anchored, deferred, div, point,
    prelude::*, px, rgb, rgba,
};
use gpui_component::{
    Disableable, Icon, IconName, Sizable, TitleBar,
    button::{Button, ButtonCustomVariant, ButtonVariants},
};
use remoters_core::network::discovery::DiscoveredHost;

use crate::{
    ActiveTab, Desktop, Role, SessionTab, SessionTabStatus,
    account_ui::AccountSelection,
    i18n::{self, Locale},
};

use super::{
    theme::{
        ACCENT, ACCENT_LIGHT, BORDER, CANVAS, DANGER, INK, MUTED, ONLINE, SIDEBAR, SIDEBAR_ACTIVE,
        SIDEBAR_HOVER, SUBTLE, SURFACE, TITLEBAR_H, refresh_icon,
    },
    widgets::{monitor_mark, platform_letter, status_capsule, subtle_chip},
};

const CONNECTING_AMBER: u32 = 0xFF9500;

impl Desktop {
    pub(super) fn title_bar(&self, cx: &mut Context<Self>) -> TitleBar {
        let t = i18n::tr();
        let home_active = self.active_tab == ActiveTab::Home;

        let home_tab = div()
            .id("tab-home")
            .h(px(26.))
            .flex_none()
            .cursor_pointer()
            .rounded(px(7.))
            .px(px(9.))
            .flex()
            .items_center()
            .gap(px(6.))
            .text_xs()
            .font_weight(if home_active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .text_color(rgb(if home_active { INK } else { MUTED }))
            .when(home_active, |el| {
                el.bg(rgb(SIDEBAR_ACTIVE))
                    .border_1()
                    .border_color(rgb(BORDER))
            })
            .when(!home_active, |el| {
                el.border_1()
                    .border_color(rgba(0x00000000))
                    .hover(|style| style.bg(rgb(SIDEBAR_HOVER)).text_color(rgb(INK)))
            })
            .on_click(cx.listener(|desktop, _, window, cx| {
                desktop.activate_tab(ActiveTab::Home, window, cx);
            }))
            .child(
                Icon::new(IconName::LayoutDashboard)
                    .size(px(13.))
                    .text_color(rgb(if home_active { ACCENT } else { MUTED })),
            )
            .child(t.home_tab);

        let session_tabs: Vec<AnyElement> = self
            .sessions
            .iter()
            .map(|tab| self.session_tab_pill(tab, cx))
            .collect();

        let add_tab = div()
            .id("tab-new")
            .size(px(24.))
            .flex_none()
            .cursor_pointer()
            .rounded(px(6.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(rgb(MUTED))
            .hover(|style| style.bg(rgb(SIDEBAR_HOVER)).text_color(rgb(INK)))
            .on_click(cx.listener(|desktop, _, window, cx| {
                desktop.open_new_session_tab(window, cx);
            }))
            .child(Icon::new(IconName::Plus).size(px(13.)));

        let lang_switcher = self.language_switcher(cx);

        TitleBar::new()
            .h(px(TITLEBAR_H))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .overflow_hidden()
                    .child(
                        div()
                            .id("titlebar-brand")
                            .flex_none()
                            .cursor_pointer()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .on_click(cx.listener(|desktop, _, window, cx| {
                                desktop.activate_tab(ActiveTab::Home, window, cx);
                            }))
                            .child(
                                div()
                                    .size(px(22.))
                                    .rounded(px(6.))
                                    .bg(rgb(ACCENT))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(monitor_mark(13., 8., CANVAS)),
                            )
                            .when(self.sessions.is_empty(), |brand| {
                                brand.child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(INK))
                                        .child(t.app_name),
                                )
                            }),
                    )
                    .child(div().w(px(1.)).h(px(15.)).flex_none().bg(rgb(BORDER)))
                    .child(home_tab)
                    .children(session_tabs)
                    .child(add_tab),
            )
            .child(
                div()
                    .flex_none()
                    .pr_3()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(self.sessions.is_empty(), |right| {
                        right.child(subtle_chip(t.lan_vpn_chip))
                    })
                    .child(lang_switcher),
            )
    }

    fn language_switcher(&self, cx: &mut Context<Self>) -> Div {
        let active_locale = i18n::locale();
        let menu_open = self.locale_menu_open;

        let trigger = div()
            .id("locale-switcher")
            .h(px(24.))
            .flex_none()
            .cursor_pointer()
            .rounded_full()
            .pl(px(8.))
            .pr(px(7.))
            .bg(rgb(if menu_open { SIDEBAR_ACTIVE } else { SURFACE }))
            .border_1()
            .border_color(rgb(if menu_open { ACCENT } else { BORDER }))
            .hover(|style| style.bg(rgb(SIDEBAR_HOVER)))
            .flex()
            .items_center()
            .gap(px(5.))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |desktop, _, _, cx| {
                    cx.stop_propagation();
                    desktop.locale_menu_open = !menu_open;
                    cx.notify();
                }),
            )
            .child(
                Icon::new(IconName::Globe)
                    .size(px(12.))
                    .text_color(rgb(if menu_open { ACCENT } else { MUTED })),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(INK))
                    .child(active_locale.short_label()),
            )
            .child(
                Icon::new(if menu_open {
                    IconName::ChevronUp
                } else {
                    IconName::ChevronDown
                })
                .size(px(10.))
                .text_color(rgb(SUBTLE)),
            );

        div()
            .relative()
            .flex_none()
            .child(trigger)
            .when(menu_open, |container| {
                let items = Locale::ALL.into_iter().enumerate().map(|(index, loc)| {
                    let selected = loc == active_locale;
                    div()
                        .id(("locale-option", index))
                        .w_full()
                        .cursor_pointer()
                        .rounded(px(7.))
                        .px(px(9.))
                        .py(px(6.))
                        .bg(rgb(if selected { ACCENT_LIGHT } else { CANVAS }))
                        .hover(move |style| {
                            style.bg(rgb(if selected {
                                ACCENT_LIGHT
                            } else {
                                SIDEBAR_HOVER
                            }))
                        })
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |desktop, _, window, cx| {
                            cx.stop_propagation();
                            desktop.set_locale(loc, window, cx);
                        }))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(if selected {
                                    FontWeight::SEMIBOLD
                                } else {
                                    FontWeight::MEDIUM
                                })
                                .text_color(rgb(if selected { ACCENT } else { INK }))
                                .child(loc.native_name()),
                        )
                        .when(selected, |row| {
                            row.child(
                                Icon::new(IconName::Check)
                                    .size(px(13.))
                                    .text_color(rgb(ACCENT)),
                            )
                        })
                });

                let menu = div()
                    .id("locale-menu")
                    .occlude()
                    .w(px(164.))
                    .rounded(px(10.))
                    .p(px(4.))
                    .bg(rgb(CANVAS))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .shadow_lg()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_mouse_down_out(cx.listener(|desktop, _, _, cx| {
                        if desktop.locale_menu_open {
                            desktop.locale_menu_open = false;
                            cx.notify();
                        }
                    }))
                    .children(items);

                container.child(
                    div().absolute().top_full().right_0().w_0().h_0().child(
                        deferred(
                            anchored()
                                .anchor(Corner::TopRight)
                                .offset(point(px(0.), px(6.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(menu),
                        )
                        .with_priority(10),
                    ),
                )
            })
    }

    fn session_tab_pill(&self, tab: &SessionTab, cx: &mut Context<Self>) -> AnyElement {
        let tab_id = tab.id;
        let active = self.active_tab == ActiveTab::Session(tab_id);
        let dot_color = match tab.status() {
            SessionTabStatus::Connecting => CONNECTING_AMBER,
            SessionTabStatus::Live => ONLINE,
            SessionTabStatus::Closed => DANGER,
        };
        div()
            .id(("tab-session", tab_id))
            .h(px(26.))
            .max_w(px(176.))
            .min_w(px(72.))
            .cursor_pointer()
            .rounded(px(7.))
            .pl(px(9.))
            .pr(px(5.))
            .flex()
            .items_center()
            .gap(px(6.))
            .when(active, |el| {
                el.bg(rgb(SIDEBAR_ACTIVE))
                    .border_1()
                    .border_color(rgb(BORDER))
            })
            .when(!active, |el| {
                el.bg(rgb(SURFACE))
                    .border_1()
                    .border_color(rgba(0x00000000))
                    .hover(|style| style.bg(rgb(SIDEBAR_HOVER)))
            })
            .on_click(cx.listener(move |desktop, _, window, cx| {
                desktop.activate_tab(ActiveTab::Session(tab_id), window, cx);
            }))
            .child(
                div()
                    .size(px(7.))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(dot_color)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_xs()
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(rgb(if active { INK } else { MUTED }))
                    .truncate()
                    .child(tab.title.clone()),
            )
            .child(
                div()
                    .id(("tab-close", tab_id))
                    .size(px(16.))
                    .flex_none()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(SUBTLE))
                    .hover(|style| style.bg(rgba(0x00000014)).text_color(rgb(INK)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |desktop, _, window, cx| {
                        cx.stop_propagation();
                        desktop.close_session_tab(tab_id, window, cx);
                    }))
                    .child(Icon::new(IconName::Close).size(px(10.))),
            )
            .into_any_element()
    }

    pub(super) fn workspace_header(&self, cx: &mut Context<Self>) -> Div {
        let t = i18n::tr();
        let account_active = self.account.is_active_workspace();
        let selected_address = self.address.read(cx).value().trim().to_string();
        let selected_host = (!account_active && self.role == Role::Connect)
            .then(|| {
                self.discovered
                    .iter()
                    .find(|host| host.address.to_string() == selected_address)
            })
            .flatten();

        let (online, badge_label, title, show_account_refresh) = if account_active {
            self.account_workspace_header()
        } else {
            let (online, badge_label, title) = match (self.role, self.sharing.as_ref()) {
                (Role::Share, Some(sharing)) => {
                    if sharing.peers.len() == 1 {
                        (
                            true,
                            i18n::fmt_connected_devices(1),
                            i18n::fmt_sharing_with(&sharing.peers[0].ip().to_string()),
                        )
                    } else if !sharing.peers.is_empty() {
                        (
                            true,
                            i18n::fmt_connected_devices(sharing.peers.len()),
                            sharing.display_summary.clone(),
                        )
                    } else {
                        (
                            true,
                            t.status_online.to_string(),
                            sharing.display_summary.clone(),
                        )
                    }
                }
                (Role::Share, None) => (
                    false,
                    t.status_ready.to_string(),
                    t.title_share_this_device.to_string(),
                ),
                (Role::Connect, _) => {
                    if let Some(host) = selected_host {
                        (true, t.status_online.to_string(), host.host_name.clone())
                    } else if !self.discovered.is_empty() {
                        (
                            true,
                            i18n::fmt_online_count(self.discovered.len()),
                            t.title_connect_to_device.to_string(),
                        )
                    } else {
                        (
                            false,
                            t.status_listening.to_string(),
                            t.title_connect_to_device.to_string(),
                        )
                    }
                }
            };
            (online, badge_label, title, false)
        };

        let is_share = self.role == Role::Share;
        let show_refresh_button = if account_active {
            show_account_refresh
        } else {
            self.sharing.is_none()
        };
        div()
            .h(px(50.))
            .flex_none()
            .px_6()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .border_b_1()
            .border_color(rgb(BORDER))
            .bg(rgb(CANVAS))
            .child(
                div()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(status_capsule(&badge_label, online))
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(INK))
                            .truncate()
                            .child(title),
                    ),
            )
            .when(
                !account_active && is_share && self.sharing.is_some(),
                |header| header.children(self.privacy_screen_status_label().map(subtle_chip)),
            )
            .when(show_refresh_button, |header| {
                header.child(
                    div()
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(BORDER))
                        .bg(rgb(SURFACE))
                        .px_1()
                        .py(px(2.))
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            Button::new("header-refresh")
                                .ghost()
                                .small()
                                .icon(refresh_icon())
                                .tooltip(if account_active {
                                    t.refresh_my_devices
                                } else if is_share {
                                    t.refresh_network_displays
                                } else {
                                    t.scan_nearby_devices
                                })
                                .label(if account_active || is_share {
                                    t.refresh
                                } else {
                                    t.scan
                                })
                                .on_click(cx.listener(move |desktop, _, window, cx| {
                                    if account_active {
                                        desktop.account.runtime.refresh_devices();
                                    } else if is_share {
                                        desktop.refresh_addresses(window, cx);
                                        desktop.refresh_displays(window, cx);
                                    } else {
                                        desktop.refresh_discovered(cx);
                                    }
                                })),
                        ),
                )
            })
    }

    pub(super) fn sidebar(&self, width: f32, cx: &mut Context<Self>) -> Div {
        let t = i18n::tr();
        let selected_address = self.address.read(cx).value().trim().to_string();
        let account_active = self.account.is_active_workspace();
        let navigation = [
            (
                Role::Connect,
                "nav-connect",
                t.nav_connect,
                IconName::ExternalLink,
                false,
            ),
            (
                Role::Share,
                "nav-share",
                t.nav_share,
                IconName::WindowRestore,
                self.sharing.is_some(),
            ),
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
            .px_3()
            .py_4()
            .gap_3()
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(self.account_sidebar_section(cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .text_xs()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(MUTED))
                                            .child(
                                                Icon::new(IconName::LayoutDashboard).size(px(14.)),
                                            )
                                            .child(t.nearby_devices),
                                    )
                                    .child(
                                        Button::new("sidebar-refresh")
                                            .ghost()
                                            .xsmall()
                                            .icon(refresh_icon())
                                            .tooltip(t.scan_nearby_devices)
                                            .on_click(cx.listener(|desktop, _, _, cx| {
                                                desktop.refresh_discovered(cx);
                                            })),
                                    ),
                            )
                            .child(if self.discovered.is_empty() {
                                div()
                                    .px_2()
                                    .py(px(6.))
                                    .text_xs()
                                    .text_color(rgb(SUBTLE))
                                    .child(t.listening_on_lan)
                                    .into_any_element()
                            } else {
                                let mut rows = Vec::with_capacity(self.discovered.len());
                                for (index, host) in self.discovered.iter().enumerate() {
                                    rows.push(self.sidebar_device_row(
                                        index,
                                        host,
                                        &selected_address,
                                        cx,
                                    ));
                                }
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .children(rows)
                                    .into_any_element()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(MUTED))
                                    .child(Icon::new(IconName::Globe).size(px(14.)))
                                    .child(t.remote_section),
                            )
                            .children(navigation.into_iter().map(
                                |(role, id, title, icon, active_dot)| {
                                    let selected = !account_active && self.role == role;
                                    let style = ButtonCustomVariant::new(cx)
                                        .color(
                                            rgb(if selected { SIDEBAR_ACTIVE } else { SIDEBAR })
                                                .into(),
                                        )
                                        .foreground(rgb(if selected { INK } else { MUTED }).into())
                                        .hover(rgb(SIDEBAR_HOVER).into())
                                        .active(rgb(SIDEBAR_ACTIVE).into());
                                    Button::new(id)
                                        .custom(style)
                                        .w_full()
                                        .justify_start()
                                        .disabled(!self.idle())
                                        .child(
                                            div()
                                                .w_full()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap_2()
                                                        .child(
                                                            Icon::new(icon)
                                                                .size(px(15.))
                                                                .text_color(rgb(if selected {
                                                                    ACCENT
                                                                } else {
                                                                    MUTED
                                                                })),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_size(px(13.))
                                                                .font_weight(if selected {
                                                                    FontWeight::SEMIBOLD
                                                                } else {
                                                                    FontWeight::MEDIUM
                                                                })
                                                                .child(title),
                                                        ),
                                                )
                                                .when(active_dot, |row| {
                                                    row.child(
                                                        div()
                                                            .size(px(6.))
                                                            .rounded_full()
                                                            .bg(rgb(ONLINE)),
                                                    )
                                                }),
                                        )
                                        .on_click(cx.listener(move |desktop, _, _, cx| {
                                            if desktop.idle()
                                                && (desktop.role != role
                                                    || desktop.account.is_active_workspace())
                                            {
                                                desktop.role = role;
                                                desktop.account.selection = AccountSelection::None;
                                                desktop.status = None;
                                                cx.notify();
                                            }
                                        }))
                                },
                            )),
                    ),
            )
            .child(self.account_sidebar_footer(cx))
    }

    fn sidebar_device_row(
        &self,
        index: usize,
        host: &DiscoveredHost,
        selected_address: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let host_address = host.address.to_string();
        let selected = !self.account.is_active_workspace()
            && self.role == Role::Connect
            && selected_address == host_address;
        let host_clone = host.clone();
        let platform_tag = platform_letter(&host.platform);
        div()
            .id(("sidebar-device", index))
            .w_full()
            .cursor_pointer()
            .rounded(px(8.))
            .px_2()
            .py(px(6.))
            .bg(rgb(if selected { SIDEBAR_ACTIVE } else { SIDEBAR }))
            .hover(move |style| {
                style.bg(rgb(if selected {
                    SIDEBAR_ACTIVE
                } else {
                    SIDEBAR_HOVER
                }))
            })
            .flex()
            .items_center()
            .gap_2()
            .on_click(cx.listener(move |desktop, _, window, cx| {
                if desktop.idle() {
                    desktop.role = Role::Connect;
                    desktop.account.selection = AccountSelection::None;
                    desktop.select_discovered(&host_clone, window, cx);
                }
            }))
            .child(
                div()
                    .size(px(6.))
                    .flex_none()
                    .rounded_full()
                    .bg(rgb(ONLINE)),
            )
            .child(
                div()
                    .size(px(19.))
                    .flex_none()
                    .rounded(px(5.))
                    .bg(rgb(ACCENT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(CANVAS))
                    .child(platform_tag),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .text_size(px(13.))
                    .font_weight(if selected {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .text_color(rgb(INK))
                    .truncate()
                    .child(host.host_name.clone()),
            )
            .into_any_element()
    }
}
