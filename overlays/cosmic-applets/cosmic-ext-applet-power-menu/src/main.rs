// SPDX-License-Identifier: GPL-3.0-only

//! Power menu for the right side of the top bar: user name, Settings,
//! Sleep, Lock Screen, Restart, Shut Down and Log Out.

mod cosmic_session;
mod power;
mod session_manager;

use cosmic::{
    Element, Task, app,
    iced::{Padding, Subscription, widget::column, window},
    widget::text,
};
use cosmic_ext_mac_applet_style::{
    PillVariant, mac_app_popup, mac_bar_item, mac_menu_divider, mac_menu_item,
    mac_pill_button, mac_popover_container_sized, panel_icon_size,
};

use crate::power::PowerAction;

const APP_ID: &str = "dev.asw.CosmicExtAppletPowerMenu";
const POPOVER_WIDTH: f32 = 230.0;

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    OpenSettings,
    Power(PowerAction),
    PowerResult(Result<(), String>),
    Closed(window::Id),
}

struct PowerMenu {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    username: String,
}

impl PowerMenu {
    fn close_popup_task(&mut self) -> app::Task<Message> {
        match self.popup.take() {
            Some(id) => cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(id)),
            None => Task::none(),
        }
    }

    fn view_popover(&self) -> Element<'_, Message> {
        let items = column![
            mac_menu_item(text::body(self.username.clone())).on_press(Message::OpenSettings),
            mac_menu_divider(),
            mac_menu_item(text::body("System Settings…")).on_press(Message::OpenSettings),
            mac_menu_divider(),
            mac_menu_item(text::body("Sleep")).on_press(Message::Power(PowerAction::Sleep)),
            mac_menu_item(text::body("Lock Screen"))
                .on_press(Message::Power(PowerAction::LockScreen)),
            mac_menu_divider(),
            mac_menu_item(text::body("Restart…")).on_press(Message::Power(PowerAction::Restart)),
            mac_menu_item(text::body("Shut Down…"))
                .on_press(Message::Power(PowerAction::ShutDown)),
            mac_menu_item(text::body(format!("Log Out {}…", self.username)))
                .on_press(Message::Power(PowerAction::LogOut)),
        ]
        .padding([6, 0]);

        mac_popover_container_sized(&self.core.applet, items, 200.0, POPOVER_WIDTH).into()
    }
}

impl cosmic::Application for PowerMenu {
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = APP_ID;

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn init(core: cosmic::app::Core, _flags: ()) -> (Self, app::Task<Self::Message>) {
        (
            Self {
                core,
                popup: None,
                username: power::current_username(),
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if self.popup.is_some() {
                    return self.close_popup_task();
                }
                return cosmic::surface::surface_task(mac_app_popup(
                    |app: &mut PowerMenu, id| {
                        app.popup.replace(id);
                    },
                    Some(POPOVER_WIDTH),
                    None,
                ));
            }
            Message::OpenSettings => {
                if let Err(err) = std::process::Command::new("cosmic-settings").spawn() {
                    tracing::error!("could not start cosmic-settings: {err}");
                }
                return self.close_popup_task();
            }
            Message::Power(action) => {
                let close = self.close_popup_task();
                let run = Task::perform(
                    async move { action.execute().await.map_err(|e| e.to_string()) },
                    |res| cosmic::action::app(Message::PowerResult(res)),
                );
                return Task::batch([close, run]);
            }
            Message::PowerResult(Err(err)) => tracing::error!("power action failed: {err}"),
            Message::PowerResult(Ok(())) => {}
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let icon = cosmic::widget::icon::from_name("system-shutdown-symbolic")
            .symbolic(true)
            .size(panel_icon_size())
            .icon();
        let pill = mac_pill_button(icon, self.popup.is_some(), PillVariant::IconOnly)
            .on_press_down(Message::TogglePopup);
        let item = mac_bar_item(
            pill,
            Padding {
                top: 0.0,
                right: 6.0,
                bottom: 0.0,
                left: 0.0,
            },
        );
        self.core.applet.autosize_window(item).into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if self.popup == Some(id) {
            self.view_popover()
        } else {
            text("").into()
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<PowerMenu>(())
}
