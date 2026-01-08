use {
    crate::api::{PoolStats, UserSummary},
    anyhow::Result,
    clap::Parser,
    iced::{
        Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Subscription, Task,
        Theme,
        alignment::{Horizontal, Vertical},
        mouse, time,
        widget::{
            Column, Scrollable, Space, button,
            canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text},
            column, container, row, svg, text, text_input,
        },
    },
    std::{collections::VecDeque, net::IpAddr, str::FromStr, time::Duration},
    tokio_util::sync::CancellationToken,
};

// Embed the logo at compile time
const LOGO_SVG: &[u8] = include_bytes!("../../static/parasite.svg");

/// Maximum number of data points to keep in history (5 minutes at 2 second intervals = 150 points)
const HISTORY_SIZE: usize = 150;
/// How often to poll for updates
const POLL_INTERVAL_MS: u64 = 2000;

// Theme colors
const BG_DARK: Color = Color::from_rgb(0.02, 0.02, 0.03);
const BG_CARD: Color = Color::from_rgb(0.06, 0.06, 0.08);
const BORDER_COLOR: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.08);
const TEXT_PRIMARY: Color = Color::from_rgb(0.9, 0.9, 0.9);
const TEXT_SECONDARY: Color = Color::from_rgb(0.5, 0.5, 0.5);
const TEXT_MUTED: Color = Color::from_rgb(0.35, 0.35, 0.35);
const ACCENT_CYAN: Color = Color::from_rgb(0.3, 0.75, 0.95);
const ACCENT_ORANGE: Color = Color::from_rgb(0.95, 0.6, 0.2);

/// How often to refresh miner scan (30 seconds)
const MINER_REFRESH_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ActiveTab {
    #[default]
    Dashboard,
    Miners,
}

#[derive(Debug, Clone)]
struct MinerInfo {
    ip: IpAddr,
    model: String,
    hashrate: String,
    temperature: String,
    status: String,
}

#[derive(Parser, Debug)]
pub(crate) struct Gui {
    #[arg(
        long,
        default_value = "http://localhost:8080",
        help = "Pool HTTP endpoint to connect to"
    )]
    endpoint: String,
}

impl Gui {
    pub(crate) fn run(self, cancel_token: CancellationToken) -> Result<()> {
        // Spawn a thread that polls the cancellation token and exits the process
        std::thread::spawn(move || {
            while !cancel_token.is_cancelled() {
                std::thread::sleep(Duration::from_millis(100));
            }
            std::process::exit(0);
        });

        let endpoint = self.endpoint;
        iced::application(
            move || ParaGui::new(endpoint.clone()),
            ParaGui::update,
            ParaGui::view,
        )
        .subscription(ParaGui::subscription)
        .theme(|_state: &ParaGui| Theme::Dark)
        .title("Parasite")
        .window_size((960.0, 840.0))
        .run()?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    StatsReceived(Result<PoolStats, String>),
    UsersReceived(Result<Vec<UserSummary>, String>),
    TabSelected(ActiveTab),
    ScanMiners,
    MinersDiscovered(Result<Vec<MinerInfo>, String>),
    ManualIpChanged(String),
    AddManualMiner,
    ManualMinerResult(Result<MinerInfo, String>),
}

struct ParaGui {
    endpoint: String,
    stats: Option<PoolStats>,
    users: Vec<UserSummary>,
    connected: bool,
    error: Option<String>,
    hash_rate_history: VecDeque<f64>,
    sps_history: VecDeque<f64>,
    hash_rate_cache: Cache,
    sps_cache: Cache,
    // Tab state
    active_tab: ActiveTab,
    // Miner scanner state
    miners: Vec<MinerInfo>,
    miners_loading: bool,
    miners_error: Option<String>,
    // Manual miner input
    manual_ip_input: String,
    manual_ip_loading: bool,
}

impl ParaGui {
    fn new(endpoint: String) -> (Self, Task<Message>) {
        let app = Self {
            endpoint: endpoint.clone(),
            stats: None,
            users: Vec::new(),
            connected: false,
            error: None,
            hash_rate_history: VecDeque::with_capacity(HISTORY_SIZE),
            sps_history: VecDeque::with_capacity(HISTORY_SIZE),
            hash_rate_cache: Cache::new(),
            sps_cache: Cache::new(),
            active_tab: ActiveTab::default(),
            miners: Vec::new(),
            miners_loading: false,
            miners_error: None,
            manual_ip_input: String::new(),
            manual_ip_loading: false,
        };

        let stats_task = fetch_stats(endpoint.clone());
        let users_task = fetch_users(endpoint);

        (app, Task::batch([stats_task, users_task]))
    }

    fn subscription(&self) -> Subscription<Message> {
        let tick = time::every(Duration::from_millis(POLL_INTERVAL_MS)).map(|_| Message::Tick);

        if self.active_tab == ActiveTab::Miners {
            let miner_refresh =
                time::every(Duration::from_secs(MINER_REFRESH_SECS)).map(|_| Message::ScanMiners);
            Subscription::batch([tick, miner_refresh])
        } else {
            tick
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                let stats_task = fetch_stats(self.endpoint.clone());
                let users_task = fetch_users(self.endpoint.clone());
                Task::batch([stats_task, users_task])
            }
            Message::StatsReceived(result) => {
                match result {
                    Ok(stats) => {
                        if self.hash_rate_history.len() >= HISTORY_SIZE {
                            self.hash_rate_history.pop_front();
                        }
                        self.hash_rate_history
                            .push_back(parse_hash_rate(&stats.hash_rate_1m.to_string()));

                        if self.sps_history.len() >= HISTORY_SIZE {
                            self.sps_history.pop_front();
                        }
                        self.sps_history.push_back(stats.sps_1m);

                        // Clear caches to trigger redraw
                        self.hash_rate_cache.clear();
                        self.sps_cache.clear();

                        self.stats = Some(stats);
                        self.connected = true;
                        self.error = None;
                    }
                    Err(e) => {
                        self.connected = false;
                        self.error = Some(e);
                    }
                }
                Task::none()
            }
            Message::UsersReceived(result) => {
                if let Ok(users) = result {
                    self.users = users;
                }
                Task::none()
            }
            Message::TabSelected(tab) => {
                self.active_tab = tab;
                // Auto-scan when switching to Miners tab if empty
                if tab == ActiveTab::Miners && self.miners.is_empty() && !self.miners_loading {
                    self.miners_loading = true;
                    return scan_miners();
                }
                Task::none()
            }
            Message::ScanMiners => {
                self.miners_loading = true;
                self.miners_error = None;
                scan_miners()
            }
            Message::MinersDiscovered(result) => {
                self.miners_loading = false;
                match result {
                    Ok(mut miners) => {
                        // Merge with existing miners (keep manually added ones)
                        let existing_ips: Vec<_> = miners.iter().map(|m| m.ip).collect();
                        for existing in &self.miners {
                            if !existing_ips.contains(&existing.ip) {
                                miners.push(existing.clone());
                            }
                        }
                        self.miners = miners;
                        self.miners_error = None;
                    }
                    Err(e) => {
                        self.miners_error = Some(e);
                    }
                }
                Task::none()
            }
            Message::ManualIpChanged(ip) => {
                self.manual_ip_input = ip;
                Task::none()
            }
            Message::AddManualMiner => {
                let ip_str = self.manual_ip_input.trim().to_string();
                if ip_str.is_empty() {
                    return Task::none();
                }
                // Check if already exists
                if let Ok(ip) = IpAddr::from_str(&ip_str) {
                    if self.miners.iter().any(|m| m.ip == ip) {
                        return Task::none();
                    }
                }
                self.manual_ip_loading = true;
                fetch_single_miner(ip_str)
            }
            Message::ManualMinerResult(result) => {
                self.manual_ip_loading = false;
                match result {
                    Ok(miner) => {
                        // Add if not already present
                        if !self.miners.iter().any(|m| m.ip == miner.ip) {
                            self.miners.push(miner);
                        }
                        self.manual_ip_input.clear();
                    }
                    Err(e) => {
                        self.miners_error = Some(e);
                    }
                }
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let header = self.view_header();

        let content = match self.active_tab {
            ActiveTab::Dashboard => {
                if let Some(stats) = &self.stats {
                    self.view_dashboard_content(stats)
                } else if let Some(error) = &self.error {
                    self.view_error(error.clone())
                } else {
                    self.view_loading()
                }
            }
            ActiveTab::Miners => self.view_miners(),
        };

        container(column![header, content].spacing(10))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(BG_DARK.into()),
                ..Default::default()
            })
            .into()
    }

    fn view_header(&self) -> Element<'_, Message> {
        let logo_handle = svg::Handle::from_memory(LOGO_SVG);
        let logo = svg(logo_handle)
            .width(Length::Fixed(50.0))
            .height(Length::Fixed(53.0));

        let dashboard_color = if self.active_tab == ActiveTab::Dashboard {
            TEXT_PRIMARY
        } else {
            TEXT_SECONDARY
        };
        let miners_color = if self.active_tab == ActiveTab::Miners {
            TEXT_PRIMARY
        } else {
            TEXT_SECONDARY
        };

        // Tabs centered with logo in the middle
        let tabs = row![
            button(text("Dashboard").size(16).color(dashboard_color))
                .on_press(Message::TabSelected(ActiveTab::Dashboard))
                .style(button::text),
            logo,
            button(text("Miners").size(16).color(miners_color))
                .on_press(Message::TabSelected(ActiveTab::Miners))
                .style(button::text),
        ]
        .spacing(15)
        .align_y(Alignment::Center);

        container(tabs)
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding(10)
            .into()
    }

    fn view_dashboard_content(&self, stats: &PoolStats) -> Element<'_, Message> {
        // Stats rows (all at top)
        let stats_row_1 = row![
            stat_card("Hash Rate", stats.hash_rate_1m.to_string(), ACCENT_CYAN),
            stat_card("Shares/sec", format!("{:.2}", stats.sps_1m), ACCENT_ORANGE),
            stat_card("Users", stats.users.to_string(), TEXT_PRIMARY),
            stat_card("Workers", stats.workers.to_string(), TEXT_PRIMARY),
            stat_card("Blocks Found", stats.blocks.to_string(), TEXT_PRIMARY),
        ]
        .spacing(10)
        .width(Length::Fill);

        let stats_row_2 = row![
            stat_card("Accepted", format_number(stats.accepted), TEXT_PRIMARY),
            stat_card("Rejected", format_number(stats.rejected), TEXT_PRIMARY),
            stat_card("Best Ever", format!("{:.4}", stats.best_ever), TEXT_PRIMARY),
            stat_card(
                "Last Share",
                stats
                    .last_share
                    .map(format_time_ago)
                    .unwrap_or_else(|| "N/A".to_string()),
                TEXT_PRIMARY
            ),
            stat_card("Uptime", format_duration(stats.uptime_secs), TEXT_PRIMARY),
        ]
        .spacing(10)
        .width(Length::Fill);

        // Charts
        let hash_rate_chart = self.view_chart(
            "Hashrate",
            &self.hash_rate_history,
            &self.hash_rate_cache,
            ACCENT_CYAN,
        );
        let sps_chart = self.view_chart(
            "Shares Per Second",
            &self.sps_history,
            &self.sps_cache,
            ACCENT_ORANGE,
        );

        let content = column![stats_row_1, stats_row_2, hash_rate_chart, sps_chart]
            .spacing(15)
            .padding(20)
            .width(Length::Fill);

        Scrollable::new(content).height(Length::Fill).into()
    }

    fn view_chart<'a>(
        &'a self,
        title: &'static str,
        history: &'a VecDeque<f64>,
        cache: &'a Cache,
        color: Color,
    ) -> Element<'a, Message> {
        let graph: Element<'_, Message> = Canvas::new(LineChart {
            history,
            cache,
            color,
        })
        .width(Length::Fill)
        .height(Length::Fixed(180.0))
        .into();

        container(
            column![text(title).size(14).color(TEXT_SECONDARY), graph,]
                .spacing(10)
                .width(Length::Fill),
        )
        .padding(15)
        .style(|_| container::Style {
            background: Some(BG_CARD.into()),
            border: iced::Border {
                color: BORDER_COLOR,
                width: 1.0,
                radius: 4.0.into(),
            },
            ..Default::default()
        })
        .into()
    }

    fn view_error(&self, error: String) -> Element<'_, Message> {
        container(
            column![
                text("Connection Error")
                    .size(20)
                    .color(Color::from_rgb(0.8, 0.3, 0.3)),
                text(error).size(14).color(TEXT_SECONDARY),
                text(format!("Endpoint: {}", self.endpoint))
                    .size(12)
                    .color(TEXT_MUTED),
            ]
            .spacing(10)
            .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
    }

    fn view_loading(&self) -> Element<'_, Message> {
        container(text("Connecting...").size(16).color(TEXT_SECONDARY))
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn view_miners(&self) -> Element<'_, Message> {
        let header_row = row![
            text("Network Miners").size(18).color(TEXT_PRIMARY),
            Space::new().width(Length::Fill),
            button(
                text(if self.miners_loading {
                    "Scanning..."
                } else {
                    "Refresh"
                })
                .size(14)
                .color(TEXT_PRIMARY)
            )
            .on_press_maybe(if self.miners_loading {
                None
            } else {
                Some(Message::ScanMiners)
            })
            .style(|theme, status| {
                let mut style = button::text(theme, status);
                style.background = Some(BG_CARD.into());
                style.border = iced::Border {
                    color: BORDER_COLOR,
                    width: 1.0,
                    radius: 4.0.into(),
                };
                style
            })
            .padding([8, 16]),
        ]
        .align_y(Alignment::Center)
        .padding([0, 20]);

        // Manual IP input section
        let manual_input = self.view_manual_ip_input();

        let content: Element<'_, Message> = if self.miners_loading && self.miners.is_empty() {
            container(
                text("Scanning network for miners...")
                    .size(14)
                    .color(TEXT_SECONDARY),
            )
            .width(Length::Fill)
            .height(Length::Fixed(200.0))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        } else if self.miners.is_empty() {
            container(
                column![
                    text("No miners found on local network")
                        .size(14)
                        .color(TEXT_MUTED),
                    Space::new().height(Length::Fixed(20.0)),
                    text("Add a miner manually by IP address:")
                        .size(12)
                        .color(TEXT_SECONDARY),
                ]
                .align_x(Alignment::Center)
                .spacing(8),
            )
            .width(Length::Fill)
            .height(Length::Fixed(150.0))
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
        } else {
            self.view_miner_list()
        };

        // Show error if any
        let error_display: Element<'_, Message> = if let Some(err) = &self.miners_error {
            container(text(err).size(12).color(Color::from_rgb(0.8, 0.3, 0.3)))
                .padding([0, 20])
                .into()
        } else {
            Space::new().height(Length::Fixed(0.0)).into()
        };

        column![header_row, manual_input, error_display, content]
            .spacing(10)
            .padding([10, 0])
            .into()
    }

    fn view_manual_ip_input(&self) -> Element<'_, Message> {
        let input = text_input(
            "Enter miner IP address (e.g., 192.168.1.100)",
            &self.manual_ip_input,
        )
        .on_input(Message::ManualIpChanged)
        .on_submit(Message::AddManualMiner)
        .padding([10, 15])
        .size(14)
        .width(Length::Fixed(350.0))
        .style(|theme, status| {
            let mut style = text_input::default(theme, status);
            style.background = BG_CARD.into();
            style.border = iced::Border {
                color: BORDER_COLOR,
                width: 1.0,
                radius: 4.0.into(),
            };
            style
        });

        let add_button = button(
            text(if self.manual_ip_loading {
                "Adding..."
            } else {
                "Add"
            })
            .size(14)
            .color(TEXT_PRIMARY),
        )
        .on_press_maybe(
            if self.manual_ip_loading || self.manual_ip_input.trim().is_empty() {
                None
            } else {
                Some(Message::AddManualMiner)
            },
        )
        .style(|theme, status| {
            let mut style = button::text(theme, status);
            style.background = Some(ACCENT_CYAN.into());
            style.border = iced::Border {
                color: ACCENT_CYAN,
                width: 1.0,
                radius: 4.0.into(),
            };
            style
        })
        .padding([10, 20]);

        container(
            row![input, add_button]
                .spacing(10)
                .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .center_x(Length::Fill)
        .padding([5, 20])
        .into()
    }

    fn view_miner_list(&self) -> Element<'_, Message> {
        // Header row
        let header = container(
            row![
                text("IP Address")
                    .size(12)
                    .color(TEXT_MUTED)
                    .width(Length::FillPortion(2)),
                text("Model")
                    .size(12)
                    .color(TEXT_MUTED)
                    .width(Length::FillPortion(2)),
                text("Hashrate")
                    .size(12)
                    .color(TEXT_MUTED)
                    .width(Length::FillPortion(2)),
                text("Temp")
                    .size(12)
                    .color(TEXT_MUTED)
                    .width(Length::FillPortion(1)),
                text("Status")
                    .size(12)
                    .color(TEXT_MUTED)
                    .width(Length::FillPortion(1)),
            ]
            .spacing(10)
            .padding([0, 20]),
        )
        .style(|_| container::Style {
            border: iced::Border {
                color: BORDER_COLOR,
                width: 0.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        });

        // Miner rows
        let rows: Vec<Element<'_, Message>> = self
            .miners
            .iter()
            .map(|m| {
                let status_color = if m.status == "Mining" {
                    Color::from_rgb(0.3, 0.8, 0.3)
                } else {
                    Color::from_rgb(0.8, 0.5, 0.2)
                };

                container(
                    row![
                        text(m.ip.to_string())
                            .size(13)
                            .color(TEXT_PRIMARY)
                            .width(Length::FillPortion(2)),
                        text(&m.model)
                            .size(13)
                            .color(TEXT_SECONDARY)
                            .width(Length::FillPortion(2)),
                        text(&m.hashrate)
                            .size(13)
                            .color(ACCENT_CYAN)
                            .width(Length::FillPortion(2)),
                        text(&m.temperature)
                            .size(13)
                            .color(TEXT_SECONDARY)
                            .width(Length::FillPortion(1)),
                        text(&m.status)
                            .size(13)
                            .color(status_color)
                            .width(Length::FillPortion(1)),
                    ]
                    .spacing(10)
                    .padding([12, 20]),
                )
                .style(|_| container::Style {
                    background: Some(BG_CARD.into()),
                    border: iced::Border {
                        color: BORDER_COLOR,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
                .into()
            })
            .collect();

        let list = Column::with_children(rows).spacing(8).padding([0, 20]);

        Scrollable::new(column![header, list].spacing(10))
            .height(Length::Fill)
            .into()
    }
}

fn stat_card(label: &'static str, value: String, value_color: Color) -> Element<'static, Message> {
    container(
        column![
            text(label).size(11).color(TEXT_MUTED),
            text(value).size(18).color(value_color),
        ]
        .spacing(4),
    )
    .padding([12, 16])
    .width(Length::Fill)
    .style(|_| container::Style {
        background: Some(BG_CARD.into()),
        border: iced::Border {
            color: BORDER_COLOR,
            width: 1.0,
            radius: 4.0.into(),
        },
        ..Default::default()
    })
    .into()
}

struct LineChart<'a> {
    history: &'a VecDeque<f64>,
    cache: &'a Cache,
    color: Color,
}

impl<'a> canvas::Program<Message> for LineChart<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            self.draw_chart(frame, bounds.size());
        });
        vec![geometry]
    }
}

impl<'a> LineChart<'a> {
    fn draw_chart(&self, frame: &mut Frame, size: Size) {
        let left_margin = 65.0;
        let right_margin = 10.0;
        let top_margin = 5.0;
        let bottom_margin = 20.0;

        let graph_width = size.width - left_margin - right_margin;
        let graph_height = size.height - top_margin - bottom_margin;

        // Pure black background
        let background = Path::rectangle(Point::ORIGIN, size);
        frame.fill(&background, Color::BLACK);

        if self.history.is_empty() || graph_width <= 0.0 || graph_height <= 0.0 {
            return;
        }

        // Calculate range with padding
        let max_value = self.history.iter().cloned().fold(f64::MIN, f64::max);
        let min_value = self.history.iter().cloned().fold(f64::MAX, f64::min);
        let range = (max_value - min_value).max(0.001);
        let padded_min = (min_value - range * 0.1).max(0.0);
        let padded_max = max_value + range * 0.1;
        let value_range = padded_max - padded_min;

        // Draw grid lines and Y-axis labels
        let grid_color = Color::from_rgba(1.0, 1.0, 1.0, 0.06);

        for i in 0..=4 {
            let ratio = i as f32 / 4.0;
            let y = top_margin + ratio * graph_height;
            let value = padded_max - (ratio as f64 * value_range);

            // Horizontal grid line
            let line = Path::line(
                Point::new(left_margin, y),
                Point::new(size.width - right_margin, y),
            );
            frame.stroke(
                &line,
                Stroke::default().with_color(grid_color).with_width(1.0),
            );

            // Y-axis label
            let label = format_chart_value(value);
            let text = Text {
                content: label,
                position: Point::new(left_margin - 8.0, y),
                color: TEXT_MUTED,
                size: 10.0.into(),
                align_x: Horizontal::Right.into(),
                align_y: Vertical::Center.into(),
                ..Default::default()
            };
            frame.fill_text(text);
        }

        // Draw the line
        let num_points = self.history.len();
        if num_points >= 2 {
            let point_spacing = graph_width / (num_points - 1).max(1) as f32;

            let mut builder = canvas::path::Builder::new();
            for (i, &value) in self.history.iter().enumerate() {
                let x = left_margin + (i as f32 * point_spacing);
                let normalized = ((value - padded_min) / value_range) as f32;
                let y = top_margin + graph_height - (normalized * graph_height);

                if i == 0 {
                    builder.move_to(Point::new(x, y));
                } else {
                    builder.line_to(Point::new(x, y));
                }
            }

            let path = builder.build();
            frame.stroke(
                &path,
                Stroke::default().with_color(self.color).with_width(1.5),
            );
        }

        // Time labels
        let seconds = (self.history.len() as f64 * POLL_INTERVAL_MS as f64 / 1000.0) as u64;

        frame.fill_text(Text {
            content: format!("-{}s", seconds),
            position: Point::new(left_margin, size.height - 3.0),
            color: TEXT_MUTED,
            size: 9.0.into(),
            align_x: Horizontal::Left.into(),
            align_y: Vertical::Bottom.into(),
            ..Default::default()
        });

        frame.fill_text(Text {
            content: "now".to_string(),
            position: Point::new(size.width - right_margin, size.height - 3.0),
            color: TEXT_MUTED,
            size: 9.0.into(),
            align_x: Horizontal::Right.into(),
            align_y: Vertical::Bottom.into(),
            ..Default::default()
        });

        // Current value
        if let Some(&current) = self.history.back() {
            frame.fill_text(Text {
                content: format_chart_value(current),
                position: Point::new(size.width - right_margin - 5.0, top_margin + 3.0),
                color: self.color,
                size: 12.0.into(),
                align_x: Horizontal::Right.into(),
                align_y: Vertical::Top.into(),
                ..Default::default()
            });
        }
    }
}

fn format_chart_value(value: f64) -> String {
    if value >= 1e15 {
        format!("{:.1} PH/s", value / 1e15)
    } else if value >= 1e12 {
        format!("{:.1} TH/s", value / 1e12)
    } else if value >= 1e9 {
        format!("{:.1} GH/s", value / 1e9)
    } else if value >= 1e6 {
        format!("{:.2} MH/s", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.2} KH/s", value / 1e3)
    } else if value >= 1.0 {
        format!("{:.1}", value)
    } else {
        format!("{:.2}", value)
    }
}

fn fetch_stats(endpoint: String) -> Task<Message> {
    Task::perform(
        async move {
            let url = format!("{}/api/stats", endpoint);
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response
                .json::<PoolStats>()
                .await
                .map_err(|e| e.to_string())
        },
        Message::StatsReceived,
    )
}

fn scan_miners() -> Task<Message> {
    Task::perform(
        async {
            use asic_rs::MinerFactory;

            // Scan common local network subnets
            let subnets = [
                "192.168.0.0/24",
                "192.168.1.0/24",
                "192.168.4.0/24",
                "10.0.0.0/24",
            ];

            let mut all_miners = Vec::new();

            for subnet in subnets {
                if let Ok(factory) = MinerFactory::from_subnet(subnet) {
                    if let Ok(miners) = factory.scan().await {
                        for miner in miners {
                            let data = miner.get_data().await;

                            let hashrate_str = data
                                .hashrate
                                .map(|h| h.to_string())
                                .unwrap_or_else(|| "N/A".to_string());

                            let temp_str = data
                                .average_temperature
                                .map(|t| format!("{:.1}°C", t.as_celsius()))
                                .unwrap_or_else(|| "N/A".to_string());

                            let status = if data.is_mining {
                                "Mining".to_string()
                            } else {
                                "Idle".to_string()
                            };

                            all_miners.push(MinerInfo {
                                ip: data.ip,
                                model: format!(
                                    "{} {}",
                                    data.device_info.make, data.device_info.model
                                ),
                                hashrate: hashrate_str,
                                temperature: temp_str,
                                status,
                            });
                        }
                    }
                }
            }

            Ok(all_miners)
        },
        Message::MinersDiscovered,
    )
}

fn fetch_single_miner(ip_str: String) -> Task<Message> {
    Task::perform(
        async move {
            use asic_rs::MinerFactory;

            let ip = IpAddr::from_str(&ip_str).map_err(|e| format!("Invalid IP: {}", e))?;

            // First try asic-rs
            let factory = MinerFactory::new();
            if let Ok(Some(miner)) = factory.get_miner(ip).await {
                let data = miner.get_data().await;

                let hashrate_str = data
                    .hashrate
                    .map(|h| h.to_string())
                    .unwrap_or_else(|| "N/A".to_string());

                let temp_str = data
                    .average_temperature
                    .map(|t| format!("{:.1}°C", t.as_celsius()))
                    .unwrap_or_else(|| "N/A".to_string());

                let status = if data.is_mining {
                    "Mining".to_string()
                } else {
                    "Idle".to_string()
                };

                return Ok(MinerInfo {
                    ip: data.ip,
                    model: format!("{} {}", data.device_info.make, data.device_info.model),
                    hashrate: hashrate_str,
                    temperature: temp_str,
                    status,
                });
            }

            // Fallback: Try NerdAxe/AxeOS API directly
            fetch_axeos_miner(ip).await
        },
        Message::ManualMinerResult,
    )
}

/// Fallback for NerdAxe/AxeOS devices that asic-rs doesn't recognize
async fn fetch_axeos_miner(ip: IpAddr) -> Result<MinerInfo, String> {
    let url = format!("http://{}/api/system/info", ip);

    let response = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Invalid JSON: {}", e))?;

    // Parse NerdAxe/AxeOS response
    let model = json["deviceModel"]
        .as_str()
        .or_else(|| json["ASICModel"].as_str())
        .unwrap_or("Unknown")
        .to_string();

    let hashrate = json["hashRate"]
        .as_f64()
        .or_else(|| json["hashRate_1m"].as_f64())
        .map(|h| format_hashrate_gh(h))
        .unwrap_or_else(|| "N/A".to_string());

    let temp = json["temp"]
        .as_f64()
        .map(|t| format!("{:.1}°C", t))
        .unwrap_or_else(|| "N/A".to_string());

    // Check if mining based on hashrate > 0 or shares accepted
    let is_mining = json["hashRate"].as_f64().unwrap_or(0.0) > 0.0
        || json["sharesAccepted"].as_u64().unwrap_or(0) > 0;

    let status = if is_mining {
        "Mining".to_string()
    } else {
        "Idle".to_string()
    };

    Ok(MinerInfo {
        ip,
        model,
        hashrate,
        temperature: temp,
        status,
    })
}

fn format_hashrate_gh(gh: f64) -> String {
    if gh >= 1000.0 {
        format!("{:.2} TH/s", gh / 1000.0)
    } else if gh >= 1.0 {
        format!("{:.2} GH/s", gh)
    } else {
        format!("{:.2} MH/s", gh * 1000.0)
    }
}

fn fetch_users(endpoint: String) -> Task<Message> {
    Task::perform(
        async move {
            let url = format!("{}/api/users", endpoint);
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response
                .json::<Vec<UserSummary>>()
                .await
                .map_err(|e| e.to_string())
        },
        Message::UsersReceived,
    )
}

fn parse_hash_rate(s: &str) -> f64 {
    let parts: Vec<&str> = s.trim().split_whitespace().collect();
    if parts.is_empty() {
        return 0.0;
    }
    let num: f64 = parts[0].parse().unwrap_or(0.0);
    if parts.len() < 2 {
        return num;
    }
    let mult = match parts[1].chars().next() {
        Some('K') => 1e3,
        Some('M') => 1e6,
        Some('G') => 1e9,
        Some('T') => 1e12,
        Some('P') => 1e15,
        Some('E') => 1e18,
        _ => 1.0,
    };
    num * mult
}

fn format_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn format_duration(secs: u64) -> String {
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    if d > 0 {
        format!("{}d {}h", d, h)
    } else if h > 0 {
        format!("{}h {}m", h, m)
    } else {
        format!("{}m", m)
    }
}

fn format_time_ago(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if ts > now {
        return "now".to_string();
    }
    let diff = now - ts;
    if diff < 60 {
        format!("{}s ago", diff)
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}
