// SPDX-License-Identifier: GPL-3.0-only

//! System stats for the top bar: CPU and memory in the bar, and a popover
//! with CPU, memory, swap, disk, GPU and battery meters.

mod gpu;

use std::time::Duration;

use cosmic::{
    Element, Task, app,
    iced::{
        Length, Padding, Subscription,
        widget::{column, progress_bar, row},
        window,
    },
    widget::{button, container, text},
};
use cosmic_ext_mac_applet_style::{
    mac_app_popup, mac_bar_item, mac_menu_divider, mac_popover_container_sized,
    mac_text_pill_button,
};
use mac_widgets::stats::{CpuSample, Stats, format_bytes};

const APP_ID: &str = "dev.asw.CosmicExtAppletSysmon";
const POPOVER_WIDTH: f32 = 300.0;
/// Bar refresh; the popover refreshes faster while open.
const BAR_INTERVAL: Duration = Duration::from_secs(3);
const POPUP_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
enum Message {
    TogglePopup,
    Tick,
    Gpu(Option<gpu::Gpu>),
    OpenMonitor,
    Closed(window::Id),
}

struct Sysmon {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    cpu_prev: Option<CpuSample>,
    stats: Stats,
    gpu: Option<gpu::Gpu>,
    gpu_pending: bool,
}

impl Sysmon {
    fn close_popup(&mut self) -> app::Task<Message> {
        match self.popup.take() {
            Some(id) => cosmic::surface::surface_task(cosmic::surface::action::destroy_popup(id)),
            None => Task::none(),
        }
    }

    /// Query the GPU off the UI thread (`nvidia-smi` is a process).
    fn refresh_gpu(&mut self) -> app::Task<Message> {
        if self.gpu_pending {
            return Task::none();
        }
        self.gpu_pending = true;
        Task::perform(
            async { tokio::task::spawn_blocking(gpu::read).await.ok().flatten() },
            |g| cosmic::action::app(Message::Gpu(g)),
        )
    }

    fn view_popover(&self) -> Element<'_, Message> {
        let s = &self.stats;
        let used_of = |used: u64, total: u64| {
            format!("{} / {}", format_bytes(used), format_bytes(total))
        };
        let mut list = column![
            text::heading("System"),
            meter("CPU", s.cpu_percent / 100.0, format!("{:.0}%", s.cpu_percent)),
            meter("Memory", s.ram.fraction(), used_of(s.ram.used, s.ram.total)),
        ]
        .spacing(10);
        if s.swap.total > 0 {
            list = list.push(meter("Swap", s.swap.fraction(), used_of(s.swap.used, s.swap.total)));
        }
        list = list.push(meter("Disk", s.disk.fraction(), used_of(s.disk.used, s.disk.total)));
        if let Some(g) = &self.gpu {
            let mut detail = format!("{:.0}%", g.busy_percent);
            if let Some(t) = g.temp_c {
                detail.push_str(&format!(" · {t:.0}°C"));
            }
            list = list.push(meter("GPU", g.busy_percent / 100.0, detail));
            if let Some((used, total)) = g.vram {
                let fraction = if total > 0 { used as f32 / total as f32 } else { 0.0 };
                list = list.push(meter("Video memory", fraction, used_of(used, total)));
            }
        }
        if let Some(b) = s.battery {
            list = list.push(meter(
                "Battery",
                f32::from(b.percent) / 100.0,
                format!("{}%{}", b.percent, if b.charging { " · charging" } else { "" }),
            ));
        }
        let content = column![
            list.padding([12, 14]),
            mac_menu_divider(),
            container(
                button::text("Open System Monitor…")
                    .on_press(Message::OpenMonitor)
                    .width(Length::Fill),
            )
            .padding([4, 6]),
        ];
        mac_popover_container_sized(&self.core.applet, content, 260.0, POPOVER_WIDTH).into()
    }
}

fn meter<'a>(label: &'static str, fraction: f32, detail: String) -> Element<'a, Message> {
    column![
        row![text::caption(label).width(Length::Fill), text::caption(detail)],
        progress_bar(0.0..=1.0, fraction.clamp(0.0, 1.0)).girth(6),
    ]
    .spacing(3)
    .into()
}

impl cosmic::Application for Sysmon {
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
        let mut cpu_prev = None;
        let stats = Stats::read(&mut cpu_prev);
        let app = Self {
            core,
            popup: None,
            cpu_prev,
            stats,
            gpu: None,
            gpu_pending: false,
        };
        (app, Task::none())
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::Closed(id))
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if self.popup.is_some() {
                    return self.close_popup();
                }
                self.stats = Stats::read(&mut self.cpu_prev);
                let open = cosmic::surface::surface_task(mac_app_popup(
                    |app: &mut Sysmon, id| {
                        app.popup.replace(id);
                    },
                    Some(POPOVER_WIDTH),
                    None,
                ));
                return Task::batch([open, self.refresh_gpu()]);
            }
            Message::Tick => {
                self.stats = Stats::read(&mut self.cpu_prev);
                if self.popup.is_some() {
                    return self.refresh_gpu();
                }
            }
            Message::Gpu(gpu) => {
                self.gpu_pending = false;
                self.gpu = gpu;
            }
            Message::OpenMonitor => {
                if let Err(err) = std::process::Command::new("cosmic-monitor").spawn() {
                    tracing::error!("could not start cosmic-monitor: {err}");
                }
                return self.close_popup();
            }
            Message::Closed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let label = format!(
            "CPU {:.0}%  MEM {:.0}%",
            self.stats.cpu_percent,
            self.stats.ram.fraction() * 100.0
        );
        let pill = mac_text_pill_button(label, false, self.popup.is_some())
            .on_press_down(Message::TogglePopup);
        let item = mac_bar_item(pill, Padding::ZERO);
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
        let every = if self.popup.is_some() { POPUP_INTERVAL } else { BAR_INTERVAL };
        cosmic::iced::time::every(every).map(|_| Message::Tick)
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

fn main() -> cosmic::iced::Result {
    tracing_subscriber::fmt::init();
    let _ = tracing_log::LogTracer::init();
    cosmic::applet::run::<Sysmon>(())
}
