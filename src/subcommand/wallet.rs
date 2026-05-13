use super::*;

pub mod balance;
pub mod generate;
pub mod receive;
pub mod send;

fn print_json(output: impl Serialize) -> Result {
    serde_json::to_writer_pretty(io::stdout(), &output)?;
    println!();
    Ok(())
}

#[derive(Debug, Parser)]
pub(crate) struct WalletCommand {
    #[arg(long, help = "External <DESCRIPTOR>.")]
    descriptor: Option<String>,
    #[arg(long, help = "Internal (change) <DESCRIPTOR>.")]
    change_descriptor: Option<String>,
    #[arg(long, default_value_t = 0, help = "Start sync from block <BIRTHDAY>.")]
    birthday: u32,
    #[arg(long, alias = "datadir", help = "Store wallet data in <DATA_DIR>.")]
    data_dir: Option<PathBuf>,
    #[command(flatten)]
    bitcoin: BitcoinOptions,
    #[command(subcommand)]
    subcommand: Subcommand,
}

#[derive(Debug, clap::Subcommand)]
enum Subcommand {
    #[command(about = "Show wallet balance")]
    Balance,
    #[command(about = "Generate new taproot descriptors")]
    Generate,
    #[command(about = "Show receiving address")]
    Receive,
    #[command(about = "Send bitcoin")]
    Send(send::Send),
}

impl WalletCommand {
    pub(crate) async fn run(self) -> Result {
        if matches!(&self.subcommand, Subcommand::Generate) {
            let settings = Settings::from_bitcoin_options(self.bitcoin)?;
            return print_json(generate::run(settings.chain().network())?);
        }

        let settings = Arc::new(Settings::from_wallet_options(
            self.bitcoin,
            self.data_dir,
            self.descriptor,
            self.change_descriptor,
            self.birthday,
        )?);
        let network = settings.chain().network();

        let subcommand = self.subcommand;

        task::spawn_blocking(move || {
            let store = Arc::new(Store::open(settings.clone())?);
            let wallet = Wallet::open(settings, store)?;

            match subcommand {
                Subcommand::Balance => print_json(balance::run(&wallet)?),
                Subcommand::Receive => print_json(receive::run(&wallet)?),
                Subcommand::Send(send) => print_json(send.run(&wallet, network)?),
                Subcommand::Generate => unreachable!(),
            }
        })
        .await?
    }
}
