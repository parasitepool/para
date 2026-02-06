use {
    super::*,
    axum::{
        Extension,
        extract::{
            Path,
            ws::{Message, WebSocketUpgrade},
        },
    },
    error::{OptionExt, ServerError, ServerResult},
    sysinfo::DiskRefreshKind,
};

pub(crate) mod accept_json;
pub(crate) mod error;
pub(crate) mod templates;

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

pub(crate) async fn ws_logs(
    Extension(logs): Extension<Arc<logs::Logs>>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(|socket| async move {
        let (mut sender, mut receiver) = socket.split();

        for msg in logs.backlog() {
            if sender
                .send(Message::Text(msg.as_ref().into()))
                .await
                .is_err()
            {
                return;
            }
        }

        let level = logs.get_level();
        let _ = sender
            .send(Message::Text(format!("level\t{level}").into()))
            .await;

        let mut rx = logs.subscribe();

        let send_task = async {
            while let Ok(msg) = rx.recv().await {
                if sender
                    .send(Message::Text(msg.as_ref().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        };

        let recv_task = async {
            while let Some(Ok(msg)) = receiver.next().await {
                if let Message::Text(text) = msg
                    && let Some(level) = text.strip_prefix("set-level:")
                {
                    logs.set_level(level);
                    logs.broadcast_level(level);
                }
            }
        };

        tokio::select! {
            _ = send_task => {}
            _ = recv_task => {}
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
pub struct SystemStatus {
    pub cpu_usage_percent: f64,
    pub memory_usage_percent: f64,
    pub disk_usage_percent: f64,
    pub uptime: u64,
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

pub(crate) async fn system_status() -> Json<SystemStatus> {
    Json(task::block_in_place(|| {
        let system = System::new_all();

        let path = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));

        let mut disk_usage_percent = 0.0;

        let disks =
            Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage());

        for disk in &disks {
            if path.starts_with(disk.mount_point()) {
                let total = disk.total_space();
                if total > 0 {
                    disk_usage_percent =
                        100.0 * (total - disk.available_space()) as f64 / total as f64;
                }
                break;
            }
        }

        let total_memory = system.total_memory();
        let memory_usage_percent = if total_memory > 0 {
            100.0 * system.used_memory() as f64 / total_memory as f64
        } else {
            0.0
        };

        let cpu_usage_percent: f64 = system.global_cpu_usage().into();

        SystemStatus {
            cpu_usage_percent: round2(cpu_usage_percent),
            memory_usage_percent: round2(memory_usage_percent),
            disk_usage_percent: round2(disk_usage_percent),
            uptime: System::uptime(),
        }
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitcoinStatus {
    pub height: u32,
    pub network_difficulty: Difficulty,
    pub network_hashrate: HashRate,
    pub mempool_txs: u32,
}

pub(crate) async fn bitcoin_status(
    Extension(client): Extension<Arc<Client>>,
) -> ServerResult<Json<BitcoinStatus>> {
    #[derive(Debug, Deserialize)]
    struct GetMiningInfoResponse {
        blocks: u32,
        difficulty: f64,
        networkhashps: f64,
        pooledtx: u32,
    }

    let info: GetMiningInfoResponse = client
        .call_raw("getmininginfo", &[])
        .await
        .map_err(|e| ServerError::Internal(e.into()))?;

    Ok(Json(BitcoinStatus {
        height: info.blocks,
        network_difficulty: Difficulty::from(info.difficulty),
        network_hashrate: HashRate(info.networkhashps),
        mempool_txs: info.pooledtx,
    }))
}
