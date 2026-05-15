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
    #[arg(long, help = "Use <STORE_PATH> as database file.")]
    store_path: Option<PathBuf>,
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
            self.store_path,
            self.descriptor,
            self.change_descriptor,
            self.birthday,
        )?);
        let network = settings.chain().network();

        let subcommand = self.subcommand;

        task::spawn_blocking(move || {
            let store = Arc::new(Store::open(
                &settings.store_path("wallet.redb")?,
                settings.chain(),
            )?);
            let wallet = Wallet::open(settings, store.clone())?;

            match subcommand {
                Subcommand::Balance => {
                    let output = balance::run(&wallet)?;
                    wallet.persist_staged_with(|delta| store.persist_wallet_delta(delta))?;
                    print_json(output)
                }
                Subcommand::Receive => {
                    let output = receive::run(&wallet)?;
                    wallet.persist_staged_with(|delta| store.persist_wallet_delta(delta))?;
                    print_json(output)
                }
                Subcommand::Send(send) => {
                    let output = send.run(&wallet, network)?;
                    wallet.persist_staged_with(|delta| store.persist_wallet_delta(delta))?;
                    print_json(output)
                }
                Subcommand::Generate => unreachable!(),
            }
        })
        .await?
    }
}
