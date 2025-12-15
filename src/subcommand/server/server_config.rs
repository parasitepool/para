use {super::*, settings::Settings};

/// CLI arguments for server subcommand
#[derive(Clone, Debug, Parser)]
pub(crate) struct ServerConfig {
    #[arg(long, help = "Listen at <ADDRESS>.")]
    address: Option<String>,
    #[arg(long, help = "Listen on <PORT>.")]
    port: Option<u16>,
    #[arg(long, alias = "datadir", help = "Store data in <DATA_DIR>.")]
    data_dir: Option<PathBuf>,
    #[arg(long, help = "Require <ADMIN_TOKEN> for HTTP authentication.")]
    admin_token: Option<String>,
    #[arg(long, help = "Require <API_TOKEN> for HTTP authentication.")]
    api_token: Option<String>,
    #[arg(long, help = "Request ACME TLS certificate for <ACME_DOMAIN>.")]
    acme_domain: Vec<String>,
    #[arg(long, help = "Provide ACME contact <ACME_CONTACT>.")]
    acme_contact: Vec<String>,
    #[arg(
        long,
        help = "The <CHANNEL> at ntfy.sh to use for block found notifications."
    )]
    alerts_ntfy_channel: Option<String>,
    #[arg(long, help = "Connect to Postgres running at <DATABASE_URL>.")]
    database_url: Option<String>,
    #[arg(long, help = "CKpool <LOG_DIR>.")]
    log_dir: Option<PathBuf>,
    #[arg(long, help = "Collect statistics from <NODES>.")]
    nodes: Vec<Url>,
    #[arg(long, help = "Send shares to HTTP <SYNC_ENDPOINT>.")]
    sync_endpoint: Option<String>,
    #[arg(long, help = "Cache <TTL> in seconds.")]
    ttl: Option<u64>,
    #[arg(long, help = "Run account migration before processing sync batches.")]
    migrate_accounts: bool,
}

/// Resolved server configuration (merged from all sources)
#[derive(Clone, Debug)]
pub struct ResolvedServerConfig {
    settings: Settings,
    // CLI overrides
    address: Option<String>,
    port: Option<u16>,
    data_dir: Option<PathBuf>,
    admin_token: Option<String>,
    api_token: Option<String>,
    acme_domain: Vec<String>,
    acme_contact: Vec<String>,
    alerts_ntfy_channel: Option<String>,
    database_url: Option<String>,
    log_dir: Option<PathBuf>,
    nodes: Vec<Url>,
    sync_endpoint: Option<String>,
    ttl: Option<u64>,
    migrate_accounts: bool,
}

impl ServerConfig {
    /// Merge CLI args with Settings to produce resolved config
    pub fn resolve(self, settings: Settings) -> ResolvedServerConfig {
        ResolvedServerConfig {
            settings,
            address: self.address,
            port: self.port,
            data_dir: self.data_dir,
            admin_token: self.admin_token,
            api_token: self.api_token,
            acme_domain: self.acme_domain,
            acme_contact: self.acme_contact,
            alerts_ntfy_channel: self.alerts_ntfy_channel,
            database_url: self.database_url,
            log_dir: self.log_dir,
            nodes: self.nodes,
            sync_endpoint: self.sync_endpoint,
            ttl: self.ttl,
            migrate_accounts: self.migrate_accounts,
        }
    }
}

impl ResolvedServerConfig {
    pub(crate) fn address(&self) -> String {
        self.address
            .clone()
            .or(self.settings.server_address.clone())
            .unwrap_or_else(|| "0.0.0.0".into())
    }

    pub(crate) fn port(&self) -> Option<u16> {
        self.port.or(self.settings.server_port)
    }

    pub(crate) fn data_dir(&self) -> PathBuf {
        self.data_dir
            .clone()
            .or(self.settings.data_dir.clone())
            .unwrap_or_default()
    }

    pub(crate) fn acme_cache(&self) -> PathBuf {
        self.data_dir().join("acme-cache")
    }

    pub(crate) fn admin_token(&self) -> Option<&str> {
        self.admin_token
            .as_deref()
            .or(self.settings.server_admin_token.as_deref())
    }

    pub(crate) fn api_token(&self) -> Option<&str> {
        self.api_token
            .as_deref()
            .or(self.settings.server_api_token.as_deref())
    }

    pub(crate) fn acme_contacts(&self) -> Vec<String> {
        if !self.acme_contact.is_empty() {
            self.acme_contact.clone()
        } else {
            self.settings
                .server_acme_contact
                .clone()
                .unwrap_or_default()
        }
    }

    pub(crate) fn alerts_ntfy_channel(&self) -> Option<String> {
        self.alerts_ntfy_channel
            .clone()
            .or(self.settings.server_alerts_ntfy_channel.clone())
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
        } else if let Some(domains) = &self.settings.server_acme_domain {
            if !domains.is_empty() {
                return Ok(domains.clone());
            }
            Ok(vec![
                System::host_name().ok_or(anyhow!("no hostname found"))?,
            ])
        } else {
            Ok(vec![
                System::host_name().ok_or(anyhow!("no hostname found"))?,
            ])
        }
    }

    pub(crate) fn database_url(&self) -> String {
        self.database_url
            .clone()
            .or(self.settings.server_database_url.clone())
            .unwrap_or_else(|| "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool".to_string())
    }

    pub(crate) fn log_dir(&self) -> PathBuf {
        let dir = self
            .log_dir
            .clone()
            .or(self.settings.server_log_dir.clone())
            .unwrap_or_else(|| {
                std::env::current_dir().expect("Failed to get current working directory")
            });

        if !dir.exists() {
            warn!("Log dir {} does not exist", dir.display());
        }

        dir
    }

    pub(crate) fn nodes(&self) -> Vec<Url> {
        if !self.nodes.is_empty() {
            self.nodes.clone()
        } else if let Some(nodes) = &self.settings.server_nodes {
            nodes.iter().filter_map(|s| s.parse::<Url>().ok()).collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn sync_endpoint(&self) -> Option<String> {
        self.sync_endpoint
            .clone()
            .or(self.settings.server_sync_endpoint.clone())
    }

    pub(crate) fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl.or(self.settings.server_ttl).unwrap_or(30))
    }

    pub(crate) fn migrate_accounts(&self) -> bool {
        self.migrate_accounts || self.settings.server_migrate_accounts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_server_config(args: &str) -> ServerConfig {
        match crate::arguments::Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                crate::subcommand::Subcommand::Server(server) => server.config,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    fn default_settings() -> Settings {
        Settings::merge(crate::options::Options::default(), Default::default()).unwrap()
    }

    #[test]
    fn default_address() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert_eq!(config.address(), "0.0.0.0");
    }

    #[test]
    fn override_address() {
        let config =
            parse_server_config("para server --address 127.0.0.1").resolve(default_settings());
        assert_eq!(config.address(), "127.0.0.1");
    }

    #[test]
    fn default_acme_cache() {
        let config = parse_server_config("para server").resolve(default_settings());
        // acme_cache is data_dir/acme-cache, where data_dir comes from Settings defaults
        assert!(config.acme_cache().ends_with("acme-cache"));
    }

    #[test]
    fn override_acme_cache_via_data_dir() {
        let config =
            parse_server_config("para server --data-dir /custom/path").resolve(default_settings());
        assert_eq!(
            config.acme_cache(),
            PathBuf::from("/custom/path/acme-cache")
        );
    }

    #[test]
    fn override_acme_domains() {
        let config =
            parse_server_config("para server --acme-domain example.com --acme-domain foo.bar")
                .resolve(default_settings());
        assert_eq!(
            config.domains().unwrap(),
            vec!["example.com".to_string(), "foo.bar".to_string()]
        );
    }

    #[test]
    fn default_acme_contacts() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert!(config.acme_contacts().is_empty());
    }

    #[test]
    fn override_acme_contacts() {
        let config = parse_server_config("para server --acme-contact admin@example.com")
            .resolve(default_settings());
        assert_eq!(
            config.acme_contacts(),
            vec!["admin@example.com".to_string()]
        );
    }

    #[test]
    fn default_no_admin_token() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert_eq!(config.admin_token(), None);
    }

    #[test]
    fn admin_token() {
        let config = parse_server_config("para server --admin-token verysecrettoken")
            .resolve(default_settings());
        assert_eq!(config.admin_token(), Some("verysecrettoken"));
    }

    #[test]
    fn default_domain() {
        let config = parse_server_config("para server --acme-domain example.com")
            .resolve(default_settings());
        assert_eq!(config.domain(), "example.com");
    }

    #[test]
    fn default_domains_fallback() {
        let config = parse_server_config("para server").resolve(default_settings());
        let domains = config.domains().unwrap();
        assert!(!domains.is_empty(), "Expected hostname fallback");
    }

    #[test]
    fn override_domains_no_fallback() {
        let config = parse_server_config("para server --acme-domain custom.domain")
            .resolve(default_settings());
        let domains = config.domains().unwrap();
        assert_eq!(domains, vec!["custom.domain".to_string()]);
    }

    #[test]
    fn default_data_dir() {
        let config = parse_server_config("para server").resolve(default_settings());
        // data_dir will be from settings defaults
        assert!(config.data_dir().to_string_lossy().contains("para"));
    }

    #[test]
    fn override_data_dir() {
        let config =
            parse_server_config("para server --data-dir /var/pool").resolve(default_settings());
        assert_eq!(config.data_dir(), PathBuf::from("/var/pool"));
    }

    #[test]
    fn default_database_url() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert_eq!(
            config.database_url(),
            "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
        );
    }

    #[test]
    fn override_database_url() {
        let config = parse_server_config("para server --database-url postgres://user:pass@host/db")
            .resolve(default_settings());
        assert_eq!(config.database_url(), "postgres://user:pass@host/db");
    }

    #[test]
    fn default_log_dir() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert_eq!(config.log_dir(), std::env::current_dir().unwrap());
    }

    #[test]
    fn override_log_dir() {
        let config = parse_server_config("para server --log-dir /logs").resolve(default_settings());
        assert_eq!(config.log_dir(), PathBuf::from("/logs"));
    }

    #[test]
    fn default_port() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert_eq!(config.port(), None);
    }

    #[test]
    fn override_port() {
        let config = parse_server_config("para server --port 8080").resolve(default_settings());
        assert_eq!(config.port(), Some(8080));
    }

    #[test]
    fn default_nodes() {
        let config = parse_server_config("para server").resolve(default_settings());
        assert!(config.nodes().is_empty());
    }

    #[test]
    fn override_nodes_single_http() {
        let config = parse_server_config("para server --nodes http://localhost:80")
            .resolve(default_settings());
        let expected = vec![Url::parse("http://localhost:80").unwrap()];
        assert_eq!(config.nodes(), expected);
    }

    #[test]
    fn override_nodes_single_https() {
        let config = parse_server_config("para server --nodes https://parasite.wtf")
            .resolve(default_settings());
        let expected = vec![Url::parse("https://parasite.wtf").unwrap()];
        assert_eq!(config.nodes(), expected);
    }

    #[test]
    fn multiple_nodes() {
        let config = parse_server_config(
            "para server --nodes http://localhost:80 --nodes https://parasite.wtf",
        )
        .resolve(default_settings());
        let expected = vec![
            Url::parse("http://localhost:80").unwrap(),
            Url::parse("https://parasite.wtf").unwrap(),
        ];
        assert_eq!(config.nodes(), expected);
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn invalid_node_url() {
        parse_server_config("para server --nodes invalid_url");
    }
}
