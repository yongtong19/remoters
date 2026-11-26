use gpui::{Context, Div, FontWeight, div, prelude::*, px, rgb};
use gpui_component::{
    Disableable, IconName, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants},
    input::Input,
};

use crate::{
    Desktop, i18n,
    ui::{
        BORDER, CANVAS, DANGER, INK, MUTED, SIDEBAR, SIDEBAR_ACTIVE, SURFACE, card, field, muted,
    },
};

use super::state::{AccountSelection, AuthMode};

impl Desktop {
    pub(super) fn account_auth_panel(&self, cx: &mut Context<Self>) -> Div {
        let s = i18n::tr();
        let is_register = self.account.auth_mode == AuthMode::Register;
        let busy = self.account.auth_busy;

        let sign_in_tab = ButtonCustomVariant::new(cx)
            .color(rgb(if !is_register { CANVAS } else { SURFACE }).into())
            .foreground(rgb(if !is_register { INK } else { MUTED }).into())
            .hover(rgb(CANVAS).into())
            .active(rgb(SIDEBAR_ACTIVE).into());
        let register_tab = ButtonCustomVariant::new(cx)
            .color(rgb(if is_register { CANVAS } else { SURFACE }).into())
            .foreground(rgb(if is_register { INK } else { MUTED }).into())
            .hover(rgb(CANVAS).into())
            .active(rgb(SIDEBAR_ACTIVE).into());

        let form_card = card()
            .w_full()
            .max_w(px(640.))
            .p_0()
            .overflow_hidden()
            .child(
                div()
                    .px_5()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(BORDER))
                    .bg(rgb(SURFACE))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(INK))
                                    .child(if is_register {
                                        s.create_account
                                    } else {
                                        s.sign_in
                                    }),
                            )
                            .child(muted(s.auth_subtitle)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .p(px(3.))
                            .rounded(px(9.))
                            .bg(rgb(SIDEBAR))
                            .border_1()
                            .border_color(rgb(BORDER))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                Button::new("auth-mode-signin")
                                    .custom(sign_in_tab)
                                    .small()
                                    .label(s.sign_in)
                                    .disabled(busy)
                                    .on_click(cx.listener(|desktop, _, _, cx| {
                                        desktop.account.auth_mode = AuthMode::SignIn;
                                        desktop.account.auth_error = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("auth-mode-register")
                                    .custom(register_tab)
                                    .small()
                                    .label(s.create_account)
                                    .disabled(busy)
                                    .on_click(cx.listener(|desktop, _, _, cx| {
                                        desktop.account.auth_mode = AuthMode::Register;
                                        desktop.account.auth_error = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(field(
                        s.server_url,
                        Input::new(&self.account.server_url).large().w_full(),
                    ))
                    .child(field(
                        s.username,
                        Input::new(&self.account.username).large().w_full(),
                    ))
                    .when(is_register, |form| {
                        form.child(field(
                            s.display_name,
                            Input::new(&self.account.display_name).large().w_full(),
                        ))
                    })
                    .child(field(
                        s.password,
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&self.account.password).large().w_full()),
                            )
                            .child(
                                Button::new("auth-toggle-password")
                                    .ghost()
                                    .icon(if self.account.show_password {
                                        IconName::EyeOff
                                    } else {
                                        IconName::Eye
                                    })
                                    .on_click(cx.listener(|desktop, _, window, cx| {
                                        desktop.account.show_password =
                                            !desktop.account.show_password;
                                        let show = desktop.account.show_password;
                                        desktop.account.password.update(cx, |input, cx| {
                                            input.set_masked(!show, window, cx);
                                        });
                                        cx.notify();
                                    })),
                            ),
                    ))
                    .when_some(self.account.auth_error.as_ref(), |form, error| {
                        form.child(
                            div()
                                .w_full()
                                .rounded(px(9.))
                                .px_3()
                                .py_2()
                                .text_size(px(13.))
                                .bg(rgb(0xFFECEA))
                                .text_color(rgb(DANGER))
                                .child(error.clone()),
                        )
                    })
                    .child(
                        div()
                            .pt_1()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new("auth-cancel")
                                    .label(s.cancel)
                                    .disabled(busy)
                                    .on_click(cx.listener(|desktop, _, _, cx| {
                                        desktop.account.selection = AccountSelection::None;
                                        desktop.account.auth_error = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("auth-submit")
                                    .primary()
                                    .px_5()
                                    .disabled(busy)
                                    .icon(if busy {
                                        IconName::LoaderCircle
                                    } else {
                                        IconName::ArrowRight
                                    })
                                    .label(if busy {
                                        if is_register {
                                            s.creating_account
                                        } else {
                                            s.signing_in
                                        }
                                    } else if is_register {
                                        s.create_account
                                    } else {
                                        s.sign_in
                                    })
                                    .on_click(cx.listener(|desktop, _, _, cx| {
                                        desktop.account.submit_auth(cx);
                                    })),
                            ),
                    ),
            );

        div().w_full().flex().flex_col().gap_4().child(form_card)
    }
}
