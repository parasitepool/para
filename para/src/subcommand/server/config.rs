use super::*;

#[derive(Clone, Debug, Parser)]
pub(crate) struct Config {
    #[clap(long, help = "Listen at <ADDRESS>")]
    address: Option<String>,
    #[arg(long, help = "Request ACME TLS certificate for <ACME_DOMAIN>")]
    acme_domain: Vec<String>,
    #[arg(long, help = "Provide ACME contact <ACME_CONTACT>")]
    acme_contact: Vec<String>,
    #[arg(long, alias = "datadir", help = "Store acme cache in <DATA_DIR>")]
    data_dir: Option<PathBuf>,
    #[arg(long, help = "Connect to Postgres running at <DATABASE_URL>")]
    database_url: Option<String>,
    #[arg(long, help = "CKpool <LOG_DIR>")]
    log_dir: Option<PathBuf>,
    #[clap(long, help = "Listen on <PORT>")]
    port: Option<u16>,
    #[arg(
        long,
        help = "Require basic HTTP authentication with <USERNAME>.",
        requires = "password"
    )]
    username: Option<String>,
    #[arg(
        long,
        help = "Require basic HTTP authentication with <PASSWORD>.",
        requires = "username"
    )]
    password: Option<String>,
}

impl Config {
    pub(crate) fn address(&self) -> String {
        self.address.clone().unwrap_or_else(|| "0.0.0.0".into())
    }

    pub(crate) fn acme_cache(&self) -> PathBuf {
        self.data_dir().join("acme-cache")
    }

    pub(crate) fn acme_domains(&self) -> Vec<String> {
        self.acme_domain.clone()
    }

    pub(crate) fn acme_contacts(&self) -> Vec<String> {
        self.acme_contact.clone()
    }

    pub(crate) fn credentials(&self) -> Option<(&str, &str)> {
        self.username.as_deref().zip(self.password.as_deref())
    }

    pub(crate) fn domain(&self) -> String {
        self.domains()
            .expect("should have domain")
            .first()
            .expect("should have domain")
            .clone()
    }

    pub(crate) fn domains(&self) -> Result<Vec<String>> {
        if !self.acme_domain.is_empty() {
            Ok(self.acme_domain.clone())
        } else {
            Ok(vec![
                System::host_name().ok_or(anyhow!("no hostname found"))?,
            ])
        }
    }

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

    pub(crate) fn port(&self) -> Option<u16> {
        self.port
    }
}
