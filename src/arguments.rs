use {
    super::*,
    clap::builder::styling::{AnsiColor, Effects, Styles},
    options::Options,
    settings::Settings,
    subcommand::Subcommand,
};

#[derive(Debug, Parser)]
#[command(
  version,
  styles = Styles::styled()
    .error(AnsiColor::Red.on_default() | Effects::BOLD)
    .header(AnsiColor::Yellow.on_default() | Effects::BOLD)
    .invalid(AnsiColor::Red.on_default())
    .literal(AnsiColor::Blue.on_default())
    .placeholder(AnsiColor::Cyan.on_default())
    .usage(AnsiColor::Yellow.on_default() | Effects::BOLD)
    .valid(AnsiColor::Green.on_default()),
)]
pub(crate) struct Arguments {
    #[command(flatten)]
    pub(crate) options: Options,
    #[command(subcommand)]
    pub(crate) subcommand: Subcommand,
}

impl Arguments {
    pub(crate) async fn run(self, cancel_token: CancellationToken) -> Result {
        let settings = Settings::load(self.options)?;
        self.subcommand.run(settings, cancel_token).await
    }
}
