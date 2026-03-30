use super::*;

#[derive(Clone, Debug, Args)]
pub(crate) struct BitcoinOptions {
    #[arg(long, help = "Run on <CHAIN>.")]
    pub(crate) chain: Option<Chain>,

    #[arg(long, help = "Load Bitcoin Core data dir from <BITCOIN_DATA_DIR>.")]
    pub(crate) bitcoin_data_dir: Option<PathBuf>,

    #[arg(long, help = "Connect to Bitcoin Core RPC at <BITCOIN_RPC_PORT>.")]
    pub(crate) bitcoin_rpc_port: Option<u16>,

    #[arg(long, help = "Load Bitcoin Core RPC cookie file from <COOKIE_FILE>.")]
    pub(crate) bitcoin_rpc_cookie_file: Option<PathBuf>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC as <BITCOIN_RPC_USERNAME>."
    )]
    pub(crate) bitcoin_rpc_username: Option<String>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC with <BITCOIN_RPC_PASSWORD>."
    )]
    pub(crate) bitcoin_rpc_password: Option<String>,
}
