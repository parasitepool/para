use {super::*, crate::settings::CommonOptions, bdk_wallet::KeychainKind};

pub(crate) struct TestWallet {
    pub(crate) wallet: Arc<Wallet>,
    pub(crate) store: Arc<Store>,
    pub(crate) _directory: tempfile::TempDir,
}

pub(crate) struct TestRouter {
    pub(crate) router: Arc<Router>,
    pub(crate) _wallet: Option<TestWallet>,
    pub(crate) _directory: Option<tempfile::TempDir>,
}

impl std::ops::Deref for TestWallet {
    type Target = Wallet;

    fn deref(&self) -> &Self::Target {
        self.wallet.as_ref()
    }
}

impl std::ops::Deref for TestRouter {
    type Target = Arc<Router>;

    fn deref(&self) -> &Self::Target {
        &self.router
    }
}

impl AsRef<Router> for TestRouter {
    fn as_ref(&self) -> &Router {
        self.router.as_ref()
    }
}

pub(crate) fn test_address() -> Address {
    "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
        .parse::<Address<NetworkUnchecked>>()
        .unwrap()
        .assume_checked()
}

pub(crate) fn test_wallet() -> TestWallet {
    let (descriptor, change_descriptor) = test_wallet_descriptors();
    let directory = tempfile::tempdir().unwrap();
    let settings = wallet_settings_with_descriptors_and_data_dir(
        descriptor,
        change_descriptor,
        directory.path(),
    );
    let store = Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
    let wallet = Arc::new(Wallet::open(settings, store.clone()).unwrap());

    TestWallet {
        wallet,
        store,
        _directory: directory,
    }
}

pub(crate) fn test_wallet_descriptors() -> (String, String) {
    let mnemonic: bdk_wallet::keys::bip39::Mnemonic =
            "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about"
                .parse()
                .unwrap();

    let (_, descriptor, change_descriptor) =
        Wallet::generate_from_mnemonic(mnemonic, bitcoin::Network::Regtest).unwrap();

    (descriptor, change_descriptor)
}

pub(crate) fn test_router() -> TestRouter {
    let wallet = test_wallet();
    let metatron = Arc::new(Metatron::test_with_store(wallet.store.clone()));
    let router = Arc::new(Router::new(
        Arc::new(Settings::default()),
        metatron,
        Some(wallet.wallet.clone()),
        TaskTracker::new(),
        CancellationToken::new(),
        HashValue::from_sats(1),
    ));

    TestRouter {
        router,
        _wallet: Some(wallet),
        _directory: None,
    }
}

pub(crate) fn test_router_with_wallet(wallet: Option<Arc<Wallet>>) -> TestRouter {
    let (router, directory) = router_with_wallet(wallet);
    TestRouter {
        router,
        _wallet: None,
        _directory: Some(directory),
    }
}

pub(crate) fn test_settings(data_dir: &Path) -> Arc<Settings> {
    Arc::new(
        Settings::from_proxy_options(ProxyOptions {
            common: CommonOptions {
                address: "127.0.0.1".into(),
                port: 0,
                http_port: None,
                bitcoin: BitcoinOptions {
                    chain: Some(Chain::Regtest),
                    bitcoin_data_dir: None,
                    bitcoin_rpc_port: Some(1),
                    bitcoin_rpc_cookie_file: None,
                    bitcoin_rpc_username: Some("user".into()),
                    bitcoin_rpc_password: Some("pass".into()),
                },
                start_diff: Difficulty::default(),
                min_diff: None,
                max_diff: None,
                vardiff_period: 3.33,
                vardiff_window: 300.0,
                acme_domain: Vec::new(),
                acme_contact: Vec::new(),
                acme_cache: PathBuf::from("acme-cache"),
                data_dir: Some(data_dir.to_path_buf()),
                store_path: None,
                http_api_token: None,
                http_admin_token: None,
            },
            upstream: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@127.0.0.1:1"
                .parse()
                .unwrap(),
            timeout: 30,
            enonce1_extension_size: ENONCE1_EXTENSION_SIZE,
            disable_bouncer: false,
        })
        .unwrap(),
    )
}

pub(crate) fn test_router_settings() -> Arc<Settings> {
    let (descriptor, change_descriptor) = test_wallet_descriptors();

    Arc::new(
        Settings::from_router_options(RouterOptions {
            common: CommonOptions {
                address: "127.0.0.1".into(),
                port: 0,
                http_port: None,
                bitcoin: BitcoinOptions {
                    chain: Some(Chain::Regtest),
                    bitcoin_data_dir: None,
                    bitcoin_rpc_port: Some(1),
                    bitcoin_rpc_cookie_file: None,
                    bitcoin_rpc_username: Some("user".into()),
                    bitcoin_rpc_password: Some("pass".into()),
                },
                start_diff: Difficulty::default(),
                min_diff: None,
                max_diff: None,
                vardiff_period: 3.33,
                vardiff_window: 300.0,
                acme_domain: Vec::new(),
                acme_contact: Vec::new(),
                acme_cache: PathBuf::from("acme-cache"),
                data_dir: None,
                store_path: None,
                http_api_token: None,
                http_admin_token: None,
            },
            descriptor,
            change_descriptor: Some(change_descriptor),
            wallet_birthday: 0,
            timeout: 30,
            enonce1_extension_size: ENONCE1_EXTENSION_SIZE,
            tick_interval: 60,
            sink_order: Vec::new(),
            halt: false,
            boost: false,
            capacity_work: 1e18,
            premium_percent: 5.0,
        })
        .unwrap(),
    )
}

pub(crate) fn router_with_wallet(wallet: Option<Arc<Wallet>>) -> (Arc<Router>, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
    let router = Arc::new(Router::new(
        Arc::new(Settings::default()),
        Arc::new(Metatron::test_with_store(store)),
        wallet,
        TaskTracker::new(),
        CancellationToken::new(),
        HashValue::from_sats(1),
    ));
    (router, directory)
}

pub(crate) fn wallet_settings_without_descriptors() -> Arc<Settings> {
    Arc::new(
        Settings::from_bitcoin_options(BitcoinOptions {
            chain: Some(Chain::Regtest),
            bitcoin_data_dir: None,
            bitcoin_rpc_port: Some(1),
            bitcoin_rpc_cookie_file: None,
            bitcoin_rpc_username: Some("user".into()),
            bitcoin_rpc_password: Some("pass".into()),
        })
        .unwrap(),
    )
}

pub(crate) fn wallet_settings_with_descriptors(data_dir: &Path) -> Arc<Settings> {
    let (descriptor, change_descriptor) = test_wallet_descriptors();

    wallet_settings_with_descriptors_and_data_dir(descriptor, change_descriptor, data_dir)
}

pub(crate) fn wallet_settings_with_descriptors_and_data_dir(
    descriptor: String,
    change_descriptor: String,
    data_dir: &Path,
) -> Arc<Settings> {
    Arc::new(
        Settings::from_wallet_options(
            BitcoinOptions {
                chain: Some(Chain::Regtest),
                bitcoin_data_dir: None,
                bitcoin_rpc_port: Some(1),
                bitcoin_rpc_cookie_file: None,
                bitcoin_rpc_username: Some("user".into()),
                bitcoin_rpc_password: Some("pass".into()),
            },
            Some(data_dir.to_path_buf()),
            None,
            Some(descriptor),
            Some(change_descriptor),
            0,
        )
        .unwrap(),
    )
}

pub(crate) fn persisted_next_external_index(store: &Store) -> u32 {
    let changeset = store.read_wallet_changeset().unwrap();
    let mut wallet = bdk_wallet::Wallet::load()
        .check_network(Network::Regtest)
        .load_wallet_no_persist(changeset)
        .unwrap()
        .expect("wallet state persisted");

    wallet.reveal_next_address(KeychainKind::External).index
}

pub(crate) fn test_upstream_target() -> UpstreamTarget {
    "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo@bar:3333"
        .parse()
        .unwrap()
}

pub(crate) fn add_test_bucket_order(router: &Arc<Router>) -> Arc<Order> {
    let wallet = router.wallet.as_ref().unwrap();
    wallet.mark_synced();

    router
        .add_bucket_order(test_upstream_target(), hash_days(1e18), router.hash_price())
        .unwrap()
}

pub(crate) fn test_order(
    id: u32,
    target: Option<HashDays>,
    status: OrderStatus,
    metatron: &Arc<Metatron>,
) -> Arc<Order> {
    let bucket = target.map(|target| Bucket {
        target,
        payment: Payment::new(test_address(), 0, Amount::from_sat(1000), 0),
    });
    let order = Order::new(
        id,
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
            .parse()
            .unwrap(),
        bucket,
        CancellationToken::new(),
        metatron.clone(),
    );

    order.force_status(status);

    if status == OrderStatus::Active {
        *order.upstream.lock() = Some(Upstream::test(id, metatron.clone()));
        let _ = order.allocator.set(Arc::new(EnonceAllocator::new(
            Extranonces::Pool(PoolExtranonces::new(4, 4).unwrap()),
            id,
        )));
    }

    order
}

pub(crate) fn test_order_with_payment(
    id: u32,
    payment: Payment,
    status: OrderStatus,
    metatron: &Arc<Metatron>,
) -> Arc<Order> {
    let order = Order::new(
        id,
        test_upstream_target(),
        Some(Bucket {
            target: hash_days(100.0),
            payment,
        }),
        CancellationToken::new(),
        metatron.clone(),
    );

    order.force_status(status);
    order
}

pub(crate) fn payment(order: &Order) -> &Payment {
    &order.bucket.as_ref().unwrap().payment
}

pub(crate) fn hash_days(value: f64) -> HashDays {
    HashDays::new(value).unwrap()
}

pub(crate) fn set_delivered_work(metatron: &Metatron, order: &Order, value: f64) {
    metatron.set_order_delivered_work(order.id, hash_days(value).to_hash_work());
}

pub(crate) fn add_orders(router: &Router, orders: impl IntoIterator<Item = Arc<Order>>) {
    let mut stored = router.book.orders().write();

    for order in orders {
        stored.add(order);
    }
}

pub(crate) fn addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

pub(crate) fn blank() -> Prelude {
    Prelude::default()
}

pub(crate) fn ids(orders: Vec<Arc<Order>>) -> Vec<u32> {
    orders.into_iter().map(|order| order.id).collect()
}

pub(crate) fn funded_bucket_order(
    id: u32,
    funded: &bdk_wallet::AddressInfo,
    amount: u64,
    metatron: &Arc<Metatron>,
) -> Arc<Order> {
    let order = Order::new(
        id,
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.foo@bar:3333"
            .parse()
            .unwrap(),
        Some(Bucket {
            target: hash_days(100.0),
            payment: Payment::new(
                funded.address.clone(),
                funded.index,
                Amount::from_sat(amount),
                0,
            ),
        }),
        CancellationToken::new(),
        metatron.clone(),
    );

    order.force_status(OrderStatus::Expired);
    order
}

pub(crate) fn regtest_wallet_router(directory: &tempfile::TempDir) -> (Arc<Router>, Arc<Wallet>) {
    let store = Arc::new(Store::open(&directory.path().join("test.redb"), Chain::Regtest).unwrap());
    let wallet = Arc::new(
        Wallet::open(
            wallet_settings_with_descriptors(directory.path()),
            store.clone(),
        )
        .unwrap(),
    );
    let router = Arc::new(Router::new(
        test_settings(directory.path()),
        Arc::new(Metatron::test_with_store(store)),
        Some(wallet.clone()),
        TaskTracker::new(),
        CancellationToken::new(),
        HashValue::from_sats(1),
    ));
    (router, wallet)
}

pub(crate) fn test_authorization(
    enonce1: &str,
    workername: &str,
) -> Arc<crate::stratifier::state::Authorization> {
    Arc::new(crate::stratifier::state::Authorization {
        enonce1: enonce1.parse().unwrap(),
        address: test_address(),
        workername: workername.into(),
        username: format!("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.{workername}")
            .parse()
            .unwrap(),
        version_mask: None,
    })
}
