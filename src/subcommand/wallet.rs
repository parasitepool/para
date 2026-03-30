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
        let settings = Settings::from_bitcoin_options(self.bitcoin)?;
        let network = settings.chain().network();

        if let Subcommand::Generate = self.subcommand {
            return print_json(generate::run(network)?);
        }

        let rpc_url = format!("http://{}", settings.bitcoin_rpc_url());
        let rpc_auth = match settings.bitcoin_credentials()? {
            Auth::UserPass(user, pass) => {
                bdk_bitcoind_rpc::bitcoincore_rpc::Auth::UserPass(user, pass)
            }
            Auth::CookieFile(path) => bdk_bitcoind_rpc::bitcoincore_rpc::Auth::CookieFile(path),
        };

        let descriptor = self
            .descriptor
            .ok_or_else(|| anyhow!("--descriptor is required"))?;

        let change_descriptor = self.change_descriptor;
        let birthday = self.birthday;
        let subcommand = self.subcommand;

        task::spawn_blocking(move || {
            let mut wallet = Wallet::new(
                &descriptor,
                change_descriptor.as_deref(),
                network,
                &rpc_url,
                rpc_auth,
            )?;

            match subcommand {
                Subcommand::Balance => print_json(balance::run(&mut wallet, birthday)?),
                Subcommand::Receive => print_json(receive::run(&mut wallet)),
                Subcommand::Send(send) => print_json(send.run(wallet, network, birthday)?),
                Subcommand::Generate => unreachable!(),
            }
        })
        .await?
    }
}
