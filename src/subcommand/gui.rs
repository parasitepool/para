use {
  clap::Parser,
  iced::{
    Alignment, Color, Element, Length, Task, Theme,
    widget::{Column, Scrollable, Space, button, column, container, row, text, text_input},
  },
  std::{net::IpAddr, str::FromStr, time::Duration},
  tokio_util::sync::CancellationToken,
};

const BG_DARK: Color = Color::from_rgb(0.02, 0.02, 0.03);
const BG_CARD: Color = Color::from_rgb(0.06, 0.06, 0.08);
const BORDER_COLOR: Color = Color::from_rgba(1.0, 1.0, 1.0, 0.08);
const TEXT_PRIMARY: Color = Color::from_rgb(0.9, 0.9, 0.9);
const TEXT_SECONDARY: Color = Color::from_rgb(0.5, 0.5, 0.5);
const TEXT_MUTED: Color = Color::from_rgb(0.35, 0.35, 0.35);
const ACCENT_CYAN: Color = Color::from_rgb(0.3, 0.75, 0.95);

#[derive(Debug, Clone)]
pub struct MinerInfo {
  ip: IpAddr,
  model: String,
  hashrate: String,
  temperature: String,
  status: String,
}

#[derive(Parser, Debug)]
pub(crate) struct Gui {}

impl Gui {
  pub(crate) fn run(self, cancel_token: CancellationToken) -> anyhow::Result<()> {
    std::thread::spawn(move || {
      while !cancel_token.is_cancelled() {
        std::thread::sleep(Duration::from_millis(100));
      }
      std::process::exit(0);
    });

    iced::application(App::new, App::update, App::view)
      .theme(App::theme)
      .title("Noid - Miner Scanner")
      .window_size((800.0, 600.0))
      .run()?;

    Ok(())
  }
}

#[derive(Debug, Clone)]
enum Message {
  ScanMiners,
  ScanResult(ScanResult),
  ManualIpChanged(String),
  AddManualMiner,
  ManualMinerResult(Result<MinerInfo, String>),
}

#[derive(Debug, Clone)]
enum ScanResult {
  Progress { scanned: u32, total: u32 },
  Found(MinerInfo),
  Done,
}

struct App {
  miners: Vec<MinerInfo>,
  scan_in_progress: bool,
  scan_progress: Option<(u32, u32)>,
  error: Option<String>,
  manual_ip_input: String,
  manual_ip_loading: bool,
}

impl App {
  fn new() -> (Self, Task<Message>) {
    let app = Self {
      miners: Vec::new(),
      scan_in_progress: false,
      scan_progress: None,
      error: None,
      manual_ip_input: String::new(),
      manual_ip_loading: false,
    };

    (app, Task::none())
  }

  fn theme(&self) -> Theme {
    Theme::Dark
  }

  fn update(&mut self, message: Message) -> Task<Message> {
    match message {
      Message::ScanMiners => {
        self.scan_in_progress = true;
        self.scan_progress = Some((0, 254));
        self.error = None;
        scan_subnet_stream()
      }
      Message::ScanResult(result) => {
        match result {
          ScanResult::Progress { scanned, total } => {
            self.scan_progress = Some((scanned, total));
          }
          ScanResult::Found(miner) => {
            if !self.miners.iter().any(|m| m.ip == miner.ip) {
              self.miners.push(miner);
            }
          }
          ScanResult::Done => {
            self.scan_in_progress = false;
            self.scan_progress = None;
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
            if !self.miners.iter().any(|m| m.ip == miner.ip) {
              self.miners.push(miner);
            }
            self.manual_ip_input.clear();
          }
          Err(e) => {
            self.error = Some(e);
          }
        }
        Task::none()
      }
    }
  }

  fn view(&self) -> Element<'_, Message> {
    let header = self.view_header();
    let manual_input = self.view_manual_input();
    let error_display = self.view_error();
    let content = self.view_content();

    container(column![header, manual_input, error_display, content].spacing(10))
      .width(Length::Fill)
      .height(Length::Fill)
      .style(|_| container::Style {
        background: Some(BG_DARK.into()),
        ..Default::default()
      })
      .into()
  }

  fn view_header(&self) -> Element<'_, Message> {
    let title = text("Network Miners").size(20).color(TEXT_PRIMARY);

    let btn_text = if self.scan_in_progress {
      if let Some((scanned, total)) = self.scan_progress {
        format!("Scanning... {}/{}", scanned, total)
      } else {
        "Scanning...".to_string()
      }
    } else {
      "Scan Network".to_string()
    };

    let scan_btn = button(text(btn_text).size(14).color(TEXT_PRIMARY))
      .on_press_maybe(if self.scan_in_progress {
        None
      } else {
        Some(Message::ScanMiners)
      })
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
      .padding([8, 16]);

    container(
      row![title, Space::new().width(Length::Fill), scan_btn]
        .align_y(Alignment::Center)
        .padding([0, 20]),
    )
    .padding(10)
    .into()
  }

  fn view_manual_input(&self) -> Element<'_, Message> {
    let input = text_input("Enter miner IP (e.g., 192.168.4.100)", &self.manual_ip_input)
      .on_input(Message::ManualIpChanged)
      .on_submit(Message::AddManualMiner)
      .padding([10, 15])
      .size(14)
      .width(Length::Fixed(300.0))
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

    let add_btn = button(
      text(if self.manual_ip_loading { "Adding..." } else { "Add" })
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
      style.background = Some(BG_CARD.into());
      style.border = iced::Border {
        color: BORDER_COLOR,
        width: 1.0,
        radius: 4.0.into(),
      };
      style
    })
    .padding([10, 20]);

    container(row![input, add_btn].spacing(10).align_y(Alignment::Center))
      .width(Length::Fill)
      .center_x(Length::Fill)
      .padding([5, 20])
      .into()
  }

  fn view_error(&self) -> Element<'_, Message> {
    if let Some(err) = &self.error {
      container(text(err).size(12).color(Color::from_rgb(0.8, 0.3, 0.3)))
        .padding([0, 20])
        .into()
    } else {
      Space::new().height(Length::Fixed(0.0)).into()
    }
  }

  fn view_content(&self) -> Element<'_, Message> {
    if self.miners.is_empty() && !self.scan_in_progress {
      return container(
        column![
          text("No miners found").size(14).color(TEXT_MUTED),
          Space::new().height(Length::Fixed(10.0)),
          text("Click 'Scan Network' to search 192.168.4.0/24")
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
      .into();
    }

    self.view_miner_list()
  }

  fn view_miner_list(&self) -> Element<'_, Message> {
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
    );

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

const SCAN_CONCURRENCY: usize = 64;
const SCAN_TIMEOUT_SECS: u64 = 5;

fn scan_subnet_stream() -> Task<Message> {
  Task::stream(async_stream::stream! {
    use std::net::Ipv4Addr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::sync::mpsc;

    let ips: Vec<IpAddr> = (1..=254)
      .map(|i| IpAddr::V4(Ipv4Addr::new(192, 168, 4, i)))
      .collect();

    let total = ips.len() as u32;
    let scanned = Arc::new(AtomicU32::new(0));
    let (tx, mut rx) = mpsc::channel::<ScanResult>(256);

    let scan_handle = tokio::spawn({
      let scanned = scanned.clone();
      let tx = tx.clone();
      async move {
        let semaphore = Arc::new(tokio::sync::Semaphore::new(SCAN_CONCURRENCY));
        let mut handles = Vec::new();

        for ip in ips {
          let permit = semaphore.clone().acquire_owned().await.unwrap();
          let tx = tx.clone();
          let scanned = scanned.clone();

          let handle = tokio::spawn(async move {
            let _permit = permit;

            if let Some(info) = try_miner(ip).await {
              let _ = tx.send(ScanResult::Found(info)).await;
            }

            let count = scanned.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = tx.send(ScanResult::Progress { scanned: count, total }).await;
          });

          handles.push(handle);
        }

        for handle in handles {
          let _ = handle.await;
        }

        let _ = tx.send(ScanResult::Done).await;
      }
    });

    drop(tx);

    while let Some(result) = rx.recv().await {
      yield result;
    }

    let _ = scan_handle.await;
  })
  .map(Message::ScanResult)
}

async fn try_miner(ip: IpAddr) -> Option<MinerInfo> {
  use asic_rs::MinerFactory;

  let factory = MinerFactory::new()
    .with_identification_timeout_secs(SCAN_TIMEOUT_SECS)
    .with_connectivity_timeout_secs(1);

  let miner = factory.get_miner(ip).await.ok()??;
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

  Some(MinerInfo {
    ip: data.ip,
    model: format!("{} {}", data.device_info.make, data.device_info.model),
    hashrate: hashrate_str,
    temperature: temp_str,
    status,
  })
}

fn fetch_single_miner(ip_str: String) -> Task<Message> {
  Task::perform(
    async move {
      use asic_rs::MinerFactory;

      let ip = IpAddr::from_str(&ip_str).map_err(|e| format!("Invalid IP: {}", e))?;

      let factory = MinerFactory::new()
        .with_port_check(false)
        .with_identification_timeout_secs(15);

      let miner = factory
        .get_miner(ip)
        .await
        .map_err(|e| format!("Connection error: {}", e))?
        .ok_or_else(|| format!("No miner found at {}", ip))?;

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

      Ok(MinerInfo {
        ip: data.ip,
        model: format!("{} {}", data.device_info.make, data.device_info.model),
        hashrate: hashrate_str,
        temperature: temp_str,
        status,
      })
    },
    Message::ManualMinerResult,
  )
}

