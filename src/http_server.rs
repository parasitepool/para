use {
    super::*,
    axum::extract::{
        Path,
        ws::{Message, WebSocketUpgrade},
    },
    error::{OptionExt, ServerResult},
    sysinfo::DiskRefreshKind,
};

pub(crate) mod accept_json;
pub(crate) mod error;

#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub address: String,
    pub port: u16,
    pub acme_domains: Vec<String>,
    pub acme_contacts: Vec<String>,
    pub acme_cache: PathBuf,
}

pub fn spawn(
    settings: &Settings,
    router: Router,
    cancel_token: CancellationToken,
    tasks: &mut JoinSet<()>,
) -> Result<()> {
    let Some(port) = settings.http_port() else {
        return Ok(());
    };

    info!("Spawning http server task");

    let config = HttpConfig {
        address: settings.address().to_string(),
        port,
        acme_domains: settings.acme_domains().to_vec(),
        acme_contacts: settings.acme_contacts().to_vec(),
        acme_cache: settings.acme_cache_path(),
    };

    let handle = Handle::new();

    let shutdown_handle = handle.clone();
    tasks.spawn(async move {
        cancel_token.cancelled().await;
        info!("Shutting down http server");
        shutdown_handle.shutdown();
    });

    let (listener, tls_enabled) = bind_listener(&config)?;

    tasks.spawn(async move {
        if let Err(e) = serve(listener, router, handle, tls_enabled, config).await {
            error!("HTTP server error: {e}");
        }
    });

    Ok(())
}

pub fn spawn_with_handle(
    config: HttpConfig,
    router: Router,
    handle: Handle,
) -> Result<JoinHandle<io::Result<()>>> {
    let (listener, tls_enabled) = bind_listener(&config)?;

    Ok(tokio::spawn(async move {
        serve(listener, router, handle, tls_enabled, config).await
    }))
}

fn bind_listener(config: &HttpConfig) -> Result<(std::net::TcpListener, bool)> {
    let addr = (config.address.as_str(), config.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| {
            anyhow!(
                "failed to resolve address {}:{}",
                config.address,
                config.port
            )
        })?;

    let listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("failed to bind HTTP server to {addr}"))?;

    listener.set_nonblocking(true)?;

    let tls_enabled = !config.acme_domains.is_empty() && !config.acme_contacts.is_empty();

    if tls_enabled {
        info!("HTTPS server listening on https://{addr}");
    } else {
        info!("HTTP server listening on http://{addr}");
    }

    Ok((listener, tls_enabled))
}

async fn serve(
    listener: std::net::TcpListener,
    router: Router,
    handle: Handle,
    tls_enabled: bool,
    config: HttpConfig,
) -> io::Result<()> {
    if tls_enabled {
        info!(
            "Getting certificate for {} using contact email {}",
            config.acme_domains[0], config.acme_contacts[0]
        );

        axum_server::from_tcp(listener)
            .handle(handle)
            .acceptor(
                acceptor(config.acme_domains, config.acme_contacts, config.acme_cache).unwrap(),
            )
            .serve(router.into_make_service())
            .await
    } else {
        axum_server::from_tcp(listener)
            .handle(handle)
            .serve(router.into_make_service())
            .await
    }
}

fn acceptor(
    acme_domains: Vec<String>,
    acme_contacts: Vec<String>,
    acme_cache: PathBuf,
) -> Result<AxumAcceptor> {
    static RUSTLS_PROVIDER_INSTALLED: LazyLock<bool> = LazyLock::new(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .is_ok()
    });

    let config = AcmeConfig::new(acme_domains)
        .contact(acme_contacts)
        .cache_option(Some(DirCache::new(acme_cache)))
        .directory(if cfg!(test) {
            LETS_ENCRYPT_STAGING_DIRECTORY
        } else {
            LETS_ENCRYPT_PRODUCTION_DIRECTORY
        });

    let mut state = config.state();

    ensure! {
        *RUSTLS_PROVIDER_INSTALLED,
        "failed to install rustls ring crypto provider",
    }

    let mut server_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(state.resolver());

    server_config.alpn_protocols = vec!["h2".into(), "http/1.1".into()];

    let acceptor = state.axum_acceptor(Arc::new(server_config));

    tokio::spawn(async move {
        loop {
            match state.next().await {
                Some(Ok(ok)) => info!("ACME event: {:?}", ok),
                Some(Err(err)) => error!("ACME error: {:?}", err),
                None => break,
            }
        }
    });

    Ok(acceptor)
}

#[derive(RustEmbed)]
#[folder = "static"]
pub(crate) struct StaticAssets;

pub(crate) async fn ws_logs(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket| async move {
        for msg in logstream::backlog() {
            if socket
                .send(Message::Text(msg.as_ref().into()))
                .await
                .is_err()
            {
                return;
            }
        }

        let mut rx = logstream::subscribe();

        while let Ok(msg) = rx.recv().await {
            if socket
                .send(Message::Text(msg.as_ref().into()))
                .await
                .is_err()
            {
                break;
            }
        }
    })
}

pub(crate) async fn static_assets(Path(path): Path<String>) -> ServerResult<Response> {
    let content = StaticAssets::get(if let Some(stripped) = path.strip_prefix('/') {
        stripped
    } else {
        &path
    })
    .ok_or_not_found(|| format!("asset {path}"))?;

    let mime = mime_guess::from_path(path).first_or_octet_stream();

    Ok(Response::builder()
        .header(CONTENT_TYPE, mime.as_ref())
        .body(content.data.into())
        .unwrap())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SystemStatus {
    #[serde(with = "percentum::serde_points")]
    pub cpu_usage: Percentage<f64>,
    #[serde(with = "percentum::serde_points")]
    pub memory_usage: Percentage<f64>,
    #[serde(with = "percentum::serde_points")]
    pub disk_usage: Percentage<f64>,
    pub uptime: u64,
}

pub(crate) async fn system_status() -> Json<SystemStatus> {
    Json(task::block_in_place(|| {
        let mut system = System::new_all();
        system.refresh_all();

        let path = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

        let mut disk_usage = Percentage::from_points(0.0);
        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());
        for disk in &disks {
            if path.starts_with(disk.mount_point()) {
                let total = disk.total_space();
                if total > 0 {
                    disk_usage = Percentage::from_fraction(
                        (total - disk.available_space()) as f64 / total as f64,
                    );
                }
                break;
            }
        }

        let total_memory = system.total_memory();
        let memory_usage = if total_memory > 0 {
            Percentage::from_fraction(system.used_memory() as f64 / total_memory as f64)
        } else {
            Percentage::from_points(0.0)
        };

        system.refresh_cpu_all();
        let cpu_usage = Percentage::from_points(system.global_cpu_usage().into());

        SystemStatus {
            cpu_usage,
            memory_usage,
            disk_usage,
            uptime: System::uptime(),
        }
    }))
}
