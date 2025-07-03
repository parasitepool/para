use super::*;

#[derive(Clone, Default, Debug, Parser)]
pub struct Options {
    #[arg(long = "chain", help = "Run on <CHAIN>")]
    pub(crate) chain: Option<Chain>,
    #[arg(long, alias = "datadir", help = "Store acme cache in <DATA_DIR>")]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(long, help = "Connect to Postgres running at <DATABASE_URL>")]
    pub(crate) database_url: Option<String>,
    #[arg(long, help = "CKpool <LOG_DIR>")]
    pub(crate) log_dir: Option<PathBuf>,
}

impl Options {
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_default()
    }

    pub(crate) fn database_url(&self) -> String {
        self.database_url
            .clone()
            .unwrap_or_else(|| "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool".to_string())
    }

    pub(crate) fn log_dir(&self) -> PathBuf {
        self.log_dir.clone().unwrap_or_else(|| {
            std::env::current_dir().expect("Failed to get current working directory")
        })
    }
}
