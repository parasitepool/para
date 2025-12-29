use {super::*, settings::Settings};

/// CLI arguments for server subcommand
#[derive(Clone, Debug, Parser)]
pub(crate) struct ServerConfig {
    #[arg(long, help = "Listen at <ADDRESS>.")]
    pub address: Option<String>,
    #[arg(long, help = "Listen on <PORT>.")]
    pub port: Option<u16>,
    #[arg(long, alias = "datadir", help = "Store data in <DATA_DIR>.")]
    pub data_dir: Option<PathBuf>,
    #[arg(long, help = "Require <ADMIN_TOKEN> for HTTP authentication.")]
    pub admin_token: Option<String>,
    #[arg(long, help = "Require <API_TOKEN> for HTTP authentication.")]
    pub api_token: Option<String>,
    #[arg(long, help = "Request ACME TLS certificate for <ACME_DOMAIN>.")]
    pub acme_domain: Vec<String>,
    #[arg(long, help = "Provide ACME contact <ACME_CONTACT>.")]
    pub acme_contact: Vec<String>,
    #[arg(
        long,
        help = "The <CHANNEL> at ntfy.sh to use for block found notifications."
    )]
    pub alerts_ntfy_channel: Option<String>,
    #[arg(long, help = "Connect to Postgres running at <DATABASE_URL>.")]
    pub database_url: Option<String>,
    #[arg(long, help = "CKpool <LOG_DIR>.")]
    pub log_dir: Option<PathBuf>,
    #[arg(long, help = "Collect statistics from <NODES>.")]
    pub nodes: Vec<Url>,
    #[arg(long, help = "Send shares to HTTP <SYNC_ENDPOINT>.")]
    pub sync_endpoint: Option<String>,
    #[arg(long, help = "Cache <TTL> in seconds.")]
    pub ttl: Option<u64>,
    #[arg(long, help = "Run account migration before processing sync batches.")]
    pub migrate_accounts: bool,
}

impl ServerConfig {
    pub(crate) fn address(&self, settings: &Settings) -> String {
        self.address
            .clone()
            .or(settings.server_address.clone())
            .unwrap_or_else(|| "0.0.0.0".into())
    }

    pub(crate) fn port(&self, settings: &Settings) -> Option<u16> {
        self.port.or(settings.server_port)
    }

    pub(crate) fn data_dir(&self, settings: &Settings) -> PathBuf {
        self.data_dir
            .clone()
            .or(settings.data_dir.clone())
            .unwrap_or_default()
    }

    pub(crate) fn acme_cache(&self, settings: &Settings) -> PathBuf {
        self.data_dir(settings).join("acme-cache")
    }

    pub(crate) fn admin_token(&self, settings: &Settings) -> Option<String> {
        self.admin_token
            .clone()
            .or(settings.server_admin_token.clone())
    }

    pub(crate) fn api_token(&self, settings: &Settings) -> Option<String> {
        self.api_token.clone().or(settings.server_api_token.clone())
    }

    pub(crate) fn acme_contacts(&self, settings: &Settings) -> Vec<String> {
        if !self.acme_contact.is_empty() {
            self.acme_contact.clone()
        } else {
            settings.server_acme_contact.clone().unwrap_or_default()
        }
    }

    pub(crate) fn alerts_ntfy_channel(&self, settings: &Settings) -> Option<String> {
        self.alerts_ntfy_channel
            .clone()
            .or(settings.server_alerts_ntfy_channel.clone())
    }

    pub(crate) fn domain(&self, settings: &Settings) -> String {
        self.domains(settings)
            .expect("should have domain")
            .first()
            .expect("should have domain")
            .clone()
    }

    pub(crate) fn domains(&self, settings: &Settings) -> Result<Vec<String>> {
        if !self.acme_domain.is_empty() {
            Ok(self.acme_domain.clone())
        } else if let Some(domains) = &settings.server_acme_domain {
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

    pub(crate) fn database_url(&self, settings: &Settings) -> String {
        self.database_url
            .clone()
            .or(settings.server_database_url.clone())
            .unwrap_or_else(|| "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool".to_string())
    }

    pub(crate) fn log_dir(&self, settings: &Settings) -> PathBuf {
        let dir = self
            .log_dir
            .clone()
            .or(settings.server_log_dir.clone())
            .unwrap_or_else(|| {
                std::env::current_dir().expect("Failed to get current working directory")
            });

        if !dir.exists() {
            warn!("Log dir {} does not exist", dir.display());
        }

        dir
    }

    pub(crate) fn nodes(&self, settings: &Settings) -> Vec<Url> {
        if !self.nodes.is_empty() {
            self.nodes.clone()
        } else if let Some(nodes) = &settings.server_nodes {
            nodes.iter().filter_map(|s| s.parse::<Url>().ok()).collect()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn sync_endpoint(&self, settings: &Settings) -> Option<String> {
        self.sync_endpoint
            .clone()
            .or(settings.server_sync_endpoint.clone())
    }

    pub(crate) fn ttl(&self, settings: &Settings) -> Duration {
        Duration::from_secs(self.ttl.or(settings.server_ttl).unwrap_or(30))
    }

    pub(crate) fn migrate_accounts(&self, settings: &Settings) -> bool {
        self.migrate_accounts || settings.server_migrate_accounts
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
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert_eq!(config.address(&settings), "0.0.0.0");
    }

    #[test]
    fn override_address() {
        let config = parse_server_config("para server --address 127.0.0.1");
        let settings = default_settings();
        assert_eq!(config.address(&settings), "127.0.0.1");
    }

    #[test]
    fn default_acme_cache() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        // acme_cache is data_dir/acme-cache, where data_dir comes from Settings defaults
        assert!(config.acme_cache(&settings).ends_with("acme-cache"));
    }

    #[test]
    fn override_acme_cache_via_data_dir() {
        let config = parse_server_config("para server --data-dir /custom/path");
        let settings = default_settings();
        assert_eq!(
            config.acme_cache(&settings),
            PathBuf::from("/custom/path/acme-cache")
        );
    }

    #[test]
    fn override_acme_domains() {
        let config =
            parse_server_config("para server --acme-domain example.com --acme-domain foo.bar");
        let settings = default_settings();
        assert_eq!(
            config.domains(&settings).unwrap(),
            vec!["example.com".to_string(), "foo.bar".to_string()]
        );
    }

    #[test]
    fn default_acme_contacts() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert!(config.acme_contacts(&settings).is_empty());
    }

    #[test]
    fn override_acme_contacts() {
        let config = parse_server_config("para server --acme-contact admin@example.com");
        let settings = default_settings();
        assert_eq!(
            config.acme_contacts(&settings),
            vec!["admin@example.com".to_string()]
        );
    }

    #[test]
    fn default_no_admin_token() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert_eq!(config.admin_token(&settings), None);
    }

    #[test]
    fn admin_token() {
        let config = parse_server_config("para server --admin-token verysecrettoken");
        let settings = default_settings();
        assert_eq!(
            config.admin_token(&settings),
            Some("verysecrettoken".into())
        );
    }

    #[test]
    fn default_domain() {
        let config = parse_server_config("para server --acme-domain example.com");
        let settings = default_settings();
        assert_eq!(config.domain(&settings), "example.com");
    }

    #[test]
    fn default_domains_fallback() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        let domains = config.domains(&settings).unwrap();
        assert!(!domains.is_empty(), "Expected hostname fallback");
    }

    #[test]
    fn override_domains_no_fallback() {
        let config = parse_server_config("para server --acme-domain custom.domain");
        let settings = default_settings();
        let domains = config.domains(&settings).unwrap();
        assert_eq!(domains, vec!["custom.domain".to_string()]);
    }

    #[test]
    fn default_data_dir() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        // data_dir will be from settings defaults
        assert!(
            config
                .data_dir(&settings)
                .to_string_lossy()
                .contains("para")
        );
    }

    #[test]
    fn override_data_dir() {
        let config = parse_server_config("para server --data-dir /var/pool");
        let settings = default_settings();
        assert_eq!(config.data_dir(&settings), PathBuf::from("/var/pool"));
    }

    #[test]
    fn default_database_url() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert_eq!(
            config.database_url(&settings),
            "postgres://satoshi:nakamoto@127.0.0.1:5432/ckpool"
        );
    }

    #[test]
    fn override_database_url() {
        let config = parse_server_config("para server --database-url postgres://user:pass@host/db");
        let settings = default_settings();
        assert_eq!(
            config.database_url(&settings),
            "postgres://user:pass@host/db"
        );
    }

    #[test]
    fn default_log_dir() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert_eq!(config.log_dir(&settings), std::env::current_dir().unwrap());
    }

    #[test]
    fn override_log_dir() {
        let config = parse_server_config("para server --log-dir /logs");
        let settings = default_settings();
        assert_eq!(config.log_dir(&settings), PathBuf::from("/logs"));
    }

    #[test]
    fn default_port() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert_eq!(config.port(&settings), None);
    }

    #[test]
    fn override_port() {
        let config = parse_server_config("para server --port 8080");
        let settings = default_settings();
        assert_eq!(config.port(&settings), Some(8080));
    }

    #[test]
    fn default_nodes() {
        let config = parse_server_config("para server");
        let settings = default_settings();
        assert!(config.nodes(&settings).is_empty());
    }

    #[test]
    fn override_nodes_single_http() {
        let config = parse_server_config("para server --nodes http://localhost:80");
        let settings = default_settings();
        let expected = vec![Url::parse("http://localhost:80").unwrap()];
        assert_eq!(config.nodes(&settings), expected);
    }

    #[test]
    fn override_nodes_single_https() {
        let config = parse_server_config("para server --nodes https://parasite.wtf");
        let settings = default_settings();
        let expected = vec![Url::parse("https://parasite.wtf").unwrap()];
        assert_eq!(config.nodes(&settings), expected);
    }

    #[test]
    fn multiple_nodes() {
        let config = parse_server_config(
            "para server --nodes http://localhost:80 --nodes https://parasite.wtf",
        );
        let settings = default_settings();
        let expected = vec![
            Url::parse("http://localhost:80").unwrap(),
            Url::parse("https://parasite.wtf").unwrap(),
        ];
        assert_eq!(config.nodes(&settings), expected);
    }

    #[test]
    #[should_panic(expected = "error parsing arguments")]
    fn invalid_node_url() {
        parse_server_config("para server --nodes invalid_url");
    }
}
