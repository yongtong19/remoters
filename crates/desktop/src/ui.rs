mod connect;
mod session;
mod session_menu;
mod session_overlays;
mod share;
mod shell;
mod theme;
mod widgets;

use gpui::{Context, Window, div, prelude::*, px, rgb};

use crate::{Desktop, Role};

#[allow(unused_imports)]
pub(crate) use session::cursor_style_for;
pub(crate) use theme::{
    ACCENT, ACCENT_LIGHT, BORDER, CANVAS, DANGER, INK, MUTED, ONLINE, SIDEBAR, SIDEBAR_ACTIVE,
    SIDEBAR_HOVER, SUBTLE, SURFACE,
};
pub use theme::{REFRESH_CW_SVG, REFRESH_ICON_PATH, configure_theme, refresh_icon};
pub(crate) use widgets::{card, field, muted, platform_letter, status_capsule, subtle_chip};
#[allow(unused_imports)]
pub(crate) use widgets::{format_bitrate_choice, quality_mode_label, quality_summary_label};

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(target_os = "linux")]
        session::sync_linux_video_atlas(self.active_viewer(), window);
        let fullscreen = window.is_fullscreen();
        let active_session = self.active_session();
        let root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(CANVAS))
            .text_color(rgb(INK))
            .when(active_session.is_none() || !fullscreen, |root| {
                root.child(self.title_bar(cx))
            });

        if let Some(tab) = active_session {
            return root.child(self.session_panel(tab, window, fullscreen, cx));
        }

        let narrow = f32::from(window.viewport_size().width) < 915.0;
        let sidebar_width = if narrow { 200. } else { 236. };
        let padding = if narrow { 20. } else { 28. };
        let content = if self.account.is_active_workspace() {
            self.render_account_workspace(cx)
        } else {
            match (self.role, self.sharing.is_some()) {
                (Role::Connect, _) => self.connect_panel(narrow, cx),
                (Role::Share, true) => self.invitation(narrow, cx),
                (Role::Share, false) => self.share_panel(narrow, cx),
            }
        };

        root.child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(self.sidebar(sidebar_width, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .bg(rgb(CANVAS))
                        .child(self.workspace_header(cx))
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
                ),
        )
    }
}
