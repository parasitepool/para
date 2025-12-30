use {
    crate::Result,
    axum::Router,
    axum_server::Handle,
    futures::StreamExt,
    rustls_acme::{
        AcmeConfig,
        acme::{LETS_ENCRYPT_PRODUCTION_DIRECTORY, LETS_ENCRYPT_STAGING_DIRECTORY},
        axum::AxumAcceptor,
        caches::DirCache,
    },
    std::{
        io,
        net::ToSocketAddrs,
        path::PathBuf,
        sync::{Arc, LazyLock},
    },
    tokio::task::JoinHandle,
    tokio_util::sync::CancellationToken,
    tracing::{error, info},
};

#[derive(Clone, Debug)]
pub struct HttpConfig {
    pub address: String,
    pub port: u16,
    pub acme_domains: Vec<String>,
    pub acme_contacts: Vec<String>,
    pub acme_cache: PathBuf,
}

pub fn spawn(
    config: HttpConfig,
    router: Router,
    cancel_token: CancellationToken,
) -> Result<JoinHandle<io::Result<()>>> {
    let handle = Handle::new();

    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        cancel_token.cancelled().await;
        info!("Received shutdown signal, stopping HTTP server...");
        shutdown_handle.shutdown();
    });

    spawn_with_handle(config, router, handle)
}

pub fn spawn_with_handle(
    config: HttpConfig,
    router: Router,
    handle: Handle,
) -> Result<JoinHandle<io::Result<()>>> {
    let address = config.address.clone();
    let port = config.port;
    let acme_domains = config.acme_domains.clone();
    let acme_contacts = config.acme_contacts.clone();
    let acme_cache = config.acme_cache.clone();

    Ok(tokio::spawn(async move {
        if !acme_domains.is_empty() && !acme_contacts.is_empty() {
            info!(
                "Getting certificate for {} using contact email {}",
                acme_domains[0], acme_contacts[0]
            );

            let addr = (address.as_str(), port).to_socket_addrs()?.next().unwrap();

            info!("HTTP server listening on https://{addr}");

            axum_server::Server::bind(addr)
                .handle(handle)
                .acceptor(acceptor(acme_domains, acme_contacts, acme_cache).unwrap())
                .serve(router.into_make_service())
                .await
        } else {
            let addr = (address.as_str(), port).to_socket_addrs()?.next().unwrap();

            info!("HTTP server listening on http://{addr}");

            axum_server::Server::bind(addr)
                .handle(handle)
                .serve(router.into_make_service())
                .await
        }
    }))
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

    anyhow::ensure! {
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
