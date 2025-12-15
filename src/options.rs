use super::*;

#[derive(Clone, Default, Debug, Parser)]
#[command(group(
    clap::ArgGroup::new("chains")
        .required(false)
        .args(&["chain", "signet", "regtest", "testnet", "testnet4"]),
))]
pub struct Options {
    #[arg(long, help = "Load configuration from <CONFIG>.")]
    pub config: Option<PathBuf>,

    #[arg(long, help = "Load configuration from <CONFIG_DIR>/para.toml.")]
    pub config_dir: Option<PathBuf>,

    #[arg(long, alias = "datadir", help = "Store data in <DATA_DIR>.")]
    pub data_dir: Option<PathBuf>,

    #[arg(long = "chain", value_enum, help = "Use <CHAIN>. [default: mainnet]")]
    pub chain: Option<Chain>,

    #[arg(
        long,
        short = 's',
        help = "Use signet. Equivalent to `--chain signet`."
    )]
    pub signet: bool,

    #[arg(
        long,
        short = 'r',
        help = "Use regtest. Equivalent to `--chain regtest`."
    )]
    pub regtest: bool,

    #[arg(
        long,
        short = 't',
        help = "Use testnet. Equivalent to `--chain testnet`."
    )]
    pub testnet: bool,

    #[arg(long, help = "Use testnet4. Equivalent to `--chain testnet4`.")]
    pub testnet4: bool,

    #[arg(long, help = "Load Bitcoin Core data dir from <BITCOIN_DATA_DIR>.")]
    pub bitcoin_data_dir: Option<PathBuf>,

    #[arg(long, help = "Connect to Bitcoin Core RPC at <BITCOIN_RPC_PORT>.")]
    pub bitcoin_rpc_port: Option<u16>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC as <BITCOIN_RPC_USERNAME>."
    )]
    pub bitcoin_rpc_username: Option<String>,

    #[arg(
        long,
        help = "Authenticate to Bitcoin Core RPC with <BITCOIN_RPC_PASSWORD>."
    )]
    pub bitcoin_rpc_password: Option<String>,

    #[arg(
        long,
        help = "Load Bitcoin Core RPC cookie file from <BITCOIN_RPC_COOKIE_FILE>."
    )]
    pub bitcoin_rpc_cookie_file: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options() {
        let opts = Options::default();
        assert!(opts.chain.is_none());
        assert!(!opts.signet);
        assert!(!opts.regtest);
        assert!(!opts.testnet);
        assert!(!opts.testnet4);
    }

    #[test]
    fn chain_flags_are_mutually_exclusive() {
        // This should fail to parse - mutually exclusive flags
        let result = Options::try_parse_from(["para", "--signet", "--regtest"]);
        assert!(result.is_err());
    }

    #[test]
    fn chain_argument_and_flag_are_mutually_exclusive() {
        let result = Options::try_parse_from(["para", "--chain", "signet", "--regtest"]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_signet_flag() {
        let opts = Options::try_parse_from(["para", "-s"]).unwrap();
        assert!(opts.signet);
    }

    #[test]
    fn parse_regtest_flag() {
        let opts = Options::try_parse_from(["para", "-r"]).unwrap();
        assert!(opts.regtest);
    }

    #[test]
    fn parse_chain_argument() {
        let opts = Options::try_parse_from(["para", "--chain", "testnet4"]).unwrap();
        assert_eq!(opts.chain, Some(Chain::Testnet4));
    }

    #[test]
    fn parse_bitcoin_rpc_options() {
        let opts = Options::try_parse_from([
            "para",
            "--bitcoin-rpc-port",
            "18443",
            "--bitcoin-rpc-username",
            "user",
            "--bitcoin-rpc-password",
            "pass",
        ])
        .unwrap();
        assert_eq!(opts.bitcoin_rpc_port, Some(18443));
        assert_eq!(opts.bitcoin_rpc_username, Some("user".into()));
        assert_eq!(opts.bitcoin_rpc_password, Some("pass".into()));
    }
}
