use {
    crate::api::{PoolStats, UserSummary},
    anyhow::Result,
    clap::Parser,
    iced::{
        alignment::{Horizontal, Vertical},
        mouse,
        time,
        widget::{
            canvas::{self, Cache, Canvas, Frame, Geometry, Path, Stroke, Text},
            column, container, row, svg, text, Scrollable,
        },
        Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Subscription, Task,
        Theme,
    },
    std::{collections::VecDeque, time::Duration},
    tokio_util::sync::CancellationToken,
};

// Embed the logo at compile time
const LOGO_SVG: &[u8] = include_bytes!("../../static/parasite.svg");

/// Maximum number of data points to keep in history (5 minutes at 1 second intervals)
const HISTORY_SIZE: usize = 300;
/// How often to poll for updates
const POLL_INTERVAL_MS: u64 = 1000;

// Theme colors
const BG_DARK: Color = Color::from_rgb(0.02, 0.02, 0.03);
const BG_CARD: Color = Color::from_rgb(0.06, 0.06, 0.08);
const BORDER_COLOR: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.08);
const TEXT_PRIMARY: Color = Color::from_rgb(0.9, 0.9, 0.9);
const TEXT_SECONDARY: Color = Color::from_rgb(0.5, 0.5, 0.5);
const TEXT_MUTED: Color = Color::from_rgb(0.35, 0.35, 0.35);
const ACCENT_CYAN: Color = Color::from_rgb(0.3, 0.75, 0.95);
const ACCENT_ORANGE: Color = Color::from_rgb(0.95, 0.6, 0.2);

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
        // Spawn a thread that listens for Ctrl-C and exits the process
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(cancel_token.cancelled());
            std::process::exit(0);
        });

        iced::application("Parasite", ParaGui::update, ParaGui::view)
            .subscription(ParaGui::subscription)
            .theme(|_| Theme::Dark)
            .window_size((960.0, 840.0))
            .run_with(|| ParaGui::new(self.endpoint))?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
enum Message {
    Tick,
    StatsReceived(Result<PoolStats, String>),
    UsersReceived(Result<Vec<UserSummary>, String>),
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
        };

        let stats_task = fetch_stats(endpoint.clone());
        let users_task = fetch_users(endpoint);

        (app, Task::batch([stats_task, users_task]))
    }

    fn subscription(&self) -> Subscription<Message> {
        time::every(Duration::from_millis(POLL_INTERVAL_MS)).map(|_| Message::Tick)
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
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let content = if let Some(stats) = &self.stats {
            self.view_dashboard(stats)
        } else if let Some(error) = &self.error {
            self.view_error(error.clone())
        } else {
            self.view_loading()
        };

        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(BG_DARK.into()),
                ..Default::default()
            })
            .into()
    }

    fn view_dashboard(&self, stats: &PoolStats) -> Element<'_, Message> {
        // Logo header
        let logo_handle = svg::Handle::from_memory(LOGO_SVG);
        let logo = svg(logo_handle)
            .width(Length::Fixed(50.0))
            .height(Length::Fixed(53.0));

        let header = container(logo)
            .width(Length::Fill)
            .center_x(Length::Fill)
            .padding(10);

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
                stats.last_share.map(format_time_ago).unwrap_or_else(|| "N/A".to_string()),
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

        let content = column![header, stats_row_1, stats_row_2, hash_rate_chart, sps_chart,]
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
        .height(Length::Fixed(160.0))
        .into();

        container(
            column![
                text(title).size(14).color(TEXT_SECONDARY),
                graph,
            ]
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
                text("Connection Error").size(20).color(Color::from_rgb(0.8, 0.3, 0.3)),
                text(error).size(14).color(TEXT_SECONDARY),
                text(format!("Endpoint: {}", self.endpoint)).size(12).color(TEXT_MUTED),
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
            frame.stroke(&line, Stroke::default().with_color(grid_color).with_width(1.0));

            // Y-axis label
            let label = format_chart_value(value);
            let text = Text {
                content: label,
                position: Point::new(left_margin - 8.0, y),
                color: TEXT_MUTED,
                size: 10.0.into(),
                horizontal_alignment: Horizontal::Right,
                vertical_alignment: Vertical::Center,
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
            frame.stroke(&path, Stroke::default().with_color(self.color).with_width(1.5));
        }

        // Time labels
        let seconds = (self.history.len() as f64 * POLL_INTERVAL_MS as f64 / 1000.0) as u64;

        frame.fill_text(Text {
            content: format!("-{}s", seconds),
            position: Point::new(left_margin, size.height - 3.0),
            color: TEXT_MUTED,
            size: 9.0.into(),
            horizontal_alignment: Horizontal::Left,
            vertical_alignment: Vertical::Bottom,
            ..Default::default()
        });

        frame.fill_text(Text {
            content: "now".to_string(),
            position: Point::new(size.width - right_margin, size.height - 3.0),
            color: TEXT_MUTED,
            size: 9.0.into(),
            horizontal_alignment: Horizontal::Right,
            vertical_alignment: Vertical::Bottom,
            ..Default::default()
        });

        // Current value
        if let Some(&current) = self.history.back() {
            frame.fill_text(Text {
                content: format_chart_value(current),
                position: Point::new(size.width - right_margin - 5.0, top_margin + 3.0),
                color: self.color,
                size: 12.0.into(),
                horizontal_alignment: Horizontal::Right,
                vertical_alignment: Vertical::Top,
                ..Default::default()
            });
        }
    }
}

fn fetch_stats(endpoint: String) -> Task<Message> {
    Task::perform(
        async move {
            let url = format!("{}/api/stats", endpoint);
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response.json::<PoolStats>().await.map_err(|e| e.to_string())
        },
        Message::StatsReceived,
    )
}

fn fetch_users(endpoint: String) -> Task<Message> {
    Task::perform(
        async move {
            let url = format!("{}/api/users", endpoint);
            let response = reqwest::get(&url).await.map_err(|e| e.to_string())?;
            response.json::<Vec<UserSummary>>().await.map_err(|e| e.to_string())
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
