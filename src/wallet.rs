use {
    super::*,
    bdk_bitcoind_rpc::{Emitter, bitcoincore_rpc, bitcoincore_rpc::RpcApi},
    bdk_wallet::{KeychainKind, chain::Merge, keys::bip39::Mnemonic},
    bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv},
    miniscript::{
        Descriptor,
        descriptor::{DescriptorSecretKey, DescriptorXKey, KeyMap, Wildcard},
    },
    rand::RngCore,
};

pub struct Wallet {
    inner: Mutex<bdk_wallet::Wallet>,
    store: Arc<Store>,
    rpc: bitcoincore_rpc::Client,
    birthday: u32,
    dust_limit: Amount,
    synced: AtomicBool,
}

impl Wallet {
    pub(crate) fn open(settings: Arc<Settings>, store: Arc<Store>) -> Result<Self> {
        let descriptor = settings.descriptor();
        let change_descriptor = settings.change_descriptor();
        let network = settings.chain().network();
        let changeset = store.read_wallet_changeset()?;

        let inner = if changeset.is_empty() {
            let descriptor = descriptor.context("descriptor required for fresh wallet")?;

            if let Some(change_descriptor) = change_descriptor {
                bdk_wallet::Wallet::create(descriptor.to_owned(), change_descriptor.to_owned())
            } else {
                bdk_wallet::Wallet::create_single(descriptor.to_owned())
            }
            .network(network)
            .create_wallet_no_persist()
            .context("failed to create wallet")?
        } else {
            let mut params = bdk_wallet::Wallet::load().check_network(network);

            if let Some(descriptor) = descriptor {
                params = params.descriptor(KeychainKind::External, Some(descriptor.to_owned()));
            }

            if let Some(change_descriptor) = change_descriptor {
                params =
                    params.descriptor(KeychainKind::Internal, Some(change_descriptor.to_owned()));
            }

            if descriptor.is_some() || change_descriptor.is_some() {
                params = params.extract_keys();
            }

            params
                .load_wallet_no_persist(changeset)
                .context("failed to load stored wallet")?
                .context("stored wallet changeset is empty")?
        };

        let rpc = bitcoincore_rpc::Client::new(
            &format!("http://{}", settings.bitcoin_rpc_url()),
            settings.wallet_rpc_auth()?,
        )
        .context("failed to create rpc client")?;

        let dust_limit = inner
            .peek_address(KeychainKind::External, 0)
            .address
            .script_pubkey()
            .minimal_non_dust();

        let wallet = Self {
            inner: Mutex::new(inner),
            store,
            rpc,
            birthday: settings.wallet_birthday(),
            dust_limit,
            synced: AtomicBool::new(false),
        };

        wallet.persist_staged()?;

        Ok(wallet)
    }

    pub fn is_synced(&self) -> bool {
        self.synced.load(Ordering::Relaxed)
    }

    pub(crate) fn spawn(
        self: &Arc<Self>,
        interval: Duration,
        cancel: CancellationToken,
        tasks: &TaskTracker,
    ) {
        info!("Syncing wallet in background...");

        let wallet = self.clone();

        tasks.spawn(async move {
            let mut ticker = ticker(interval);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let wallet_clone = wallet.clone();
                        let cancel_clone = cancel.clone();
                        match task::spawn_blocking(move || {
                            wallet_clone.sync(&cancel_clone)?;
                            Ok::<(), Error>(())
                        })
                            .await
                            .unwrap_or_else(|err| Err(err.into()))
                        {
                            Ok(()) => {
                                if !wallet.synced.swap(true, Ordering::Relaxed) {
                                    info!("Wallet synced");
                                }
                            }
                            Err(e) => {
                                wallet.synced.store(false, Ordering::Relaxed);
                                warn!("Wallet sync error: {e}");
                            }
                        }
                    }
                }
            }
        });
    }

    pub fn sync(&self, cancel: &CancellationToken) -> Result {
        let (checkpoint, expected_mempool_txs) = {
            let inner = self.inner.lock();
            let txs = inner
                .transactions()
                .filter(|tx| !tx.chain_position.is_confirmed())
                .map(|tx| tx.tx_node.tx.clone())
                .collect::<Vec<_>>();

            (inner.latest_checkpoint(), txs)
        };

        let mut emitter = Emitter::new(&self.rpc, checkpoint, self.birthday, expected_mempool_txs);

        let mut blocks = Vec::new();
        while let Some(event) = emitter.next_block()? {
            if cancel.is_cancelled() {
                bail!("wallet sync cancelled");
            }
            blocks.push(event);
        }
        let mempool = emitter.mempool()?;

        {
            let mut inner = self.inner.lock();
            for event in blocks {
                inner.apply_block_connected_to(
                    &event.block,
                    event.block_height(),
                    event.connected_to(),
                )?;
            }
            inner.apply_unconfirmed_txs(mempool.update);
            inner.apply_evicted_txs(mempool.evicted);
            self.persist_staged_locked(&mut inner)?;
        }

        Ok(())
    }

    pub fn address(&self) -> Result<bdk_wallet::AddressInfo> {
        task::block_in_place(|| {
            let mut inner = self.inner.lock();
            let address = inner.reveal_next_address(KeychainKind::External);

            self.persist_staged_locked(&mut inner)?;

            Ok(address)
        })
    }

    pub(crate) fn persist_staged(&self) -> Result<bool> {
        let mut inner = self.inner.lock();
        self.persist_staged_locked(&mut inner)
    }

    fn persist_staged_locked(&self, inner: &mut bdk_wallet::Wallet) -> Result<bool> {
        let Some(staged) = inner.staged_mut() else {
            return Ok(false);
        };

        self.store.persist_wallet_changeset(staged)?;
        let _ = staged.take();
        Ok(true)
    }

    pub fn balance(&self) -> bdk_wallet::Balance {
        self.inner.lock().balance()
    }

    pub fn dust_limit(&self) -> Amount {
        self.dust_limit
    }

    pub fn tip(&self) -> u32 {
        self.inner.lock().latest_checkpoint().height()
    }

    pub fn received(&self, derivation_index: u32) -> (Amount, Amount) {
        let inner = self.inner.lock();
        let mut total = Amount::ZERO;
        let mut confirmed = Amount::ZERO;

        for output in inner.list_output() {
            if output.keychain == KeychainKind::External
                && output.derivation_index == derivation_index
            {
                total += output.txout.value;
                if output.chain_position.is_confirmed() {
                    confirmed += output.txout.value;
                }
            }
        }

        (total, confirmed)
    }

    pub fn send(&self, to: Address, amount: Amount, fee_rate: FeeRate) -> Result<Txid> {
        let tx = {
            let mut inner = self.inner.lock();
            let mut builder = inner.build_tx();
            builder.add_recipient(to.script_pubkey(), amount);
            builder.fee_rate(fee_rate);
            let mut psbt = builder.finish()?;

            #[allow(deprecated)]
            let finalized = inner.sign(&mut psbt, bdk_wallet::SignOptions::default())?;

            ensure!(finalized, "failed to finalize transaction");

            psbt.extract_tx_unchecked_fee_rate()
        };

        let txid = self.rpc.send_raw_transaction(&tx)?;

        self.persist_staged()?;

        Ok(txid)
    }

    pub fn generate(network: Network) -> Result<(String, String, String)> {
        let mut entropy = [0u8; 16];
        rand::rng().fill_bytes(&mut entropy);

        let mnemonic = Mnemonic::from_entropy(&entropy)?;

        Self::generate_from_mnemonic(mnemonic, network)
    }

    pub fn generate_from_mnemonic(
        mnemonic: Mnemonic,
        network: Network,
    ) -> Result<(String, String, String)> {
        let seed = mnemonic.to_seed("");

        let secp = Secp256k1::new();

        let master = Xpriv::new_master(network, &seed)?;
        let fingerprint = master.fingerprint(&secp);

        let coin_type = if network == Network::Bitcoin { 0 } else { 1 };

        let derivation_path = DerivationPath::master()
            .child(ChildNumber::Hardened { index: 86 })
            .child(ChildNumber::Hardened { index: coin_type })
            .child(ChildNumber::Hardened { index: 0 });

        let derived = master.derive_priv(&secp, &derivation_path)?;

        let mut descriptors = Vec::new();

        for change in [false, true] {
            let secret_key = DescriptorSecretKey::XPrv(DescriptorXKey {
                origin: Some((fingerprint, derivation_path.clone())),
                xkey: derived,
                derivation_path: DerivationPath::master().child(ChildNumber::Normal {
                    index: change.into(),
                }),
                wildcard: Wildcard::Unhardened,
            });

            let mut key_map = KeyMap::new();
            let public_key = key_map
                .insert(&secp, secret_key)
                .map_err(|e| anyhow!("{e}"))?;

            let descriptor = Descriptor::new_tr(public_key, None)?;
            descriptors.push(descriptor.to_string_with_secret(&key_map));
        }

        Ok((
            mnemonic.to_string(),
            descriptors.remove(0),
            descriptors.remove(0),
        ))
    }

    #[cfg(test)]
    pub(crate) fn mark_synced(&self) {
        self.synced.store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn test_reveal_address(&self) -> bdk_wallet::AddressInfo {
        let mut inner = self.inner.lock();
        let address = inner.reveal_next_address(KeychainKind::External);
        self.persist_staged_locked(&mut inner).unwrap();
        address
    }

    #[cfg(test)]
    pub(crate) fn test_receive_unconfirmed(&self, address: &Address, amount: Amount) {
        let mut inner = self.inner.lock();
        let tx = Self::test_payment_tx(address, amount);
        inner.apply_unconfirmed_txs([(tx, 1)]);
        self.persist_staged_locked(&mut inner).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn test_advance_tip_to(&self, height: u32) {
        let mut inner = self.inner.lock();

        while inner.latest_checkpoint().height() < height {
            let previous = inner.latest_checkpoint().block_id();
            let block = Self::test_block(previous.hash, Vec::new());

            inner
                .apply_block_connected_to(&block, previous.height + 1, previous)
                .unwrap();
        }

        self.persist_staged_locked(&mut inner).unwrap();
    }

    #[cfg(test)]
    fn test_payment_tx(address: &Address, amount: Amount) -> Transaction {
        Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: Txid::all_zeros(),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
            }],
        }
    }

    #[cfg(test)]
    fn test_block(prev_blockhash: BlockHash, txdata: Vec<Transaction>) -> Block {
        let mut block = Block {
            header: Header {
                version: bitcoin::block::Version::TWO,
                prev_blockhash,
                merkle_root: bitcoin::TxMerkleNode::from_raw_hash(
                    BlockHash::all_zeros().to_raw_hash(),
                ),
                time: 0,
                bits: Target::MAX.to_compact_lossy(),
                nonce: 0,
            },
            txdata,
        };

        if let Some(merkle_root) = block.compute_merkle_root() {
            block.header.merkle_root = merkle_root;
        }

        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestWallet {
        wallet: Arc<Wallet>,
        store: Arc<Store>,
        _directory: tempfile::TempDir,
    }

    impl std::ops::Deref for TestWallet {
        type Target = Wallet;

        fn deref(&self) -> &Self::Target {
            self.wallet.as_ref()
        }
    }

    fn test_wallet() -> TestWallet {
        let mnemonic: Mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".parse().unwrap();

        let (_, descriptor, change_descriptor) =
            Wallet::generate_from_mnemonic(mnemonic, Network::Regtest).unwrap();

        let directory = tempfile::tempdir().unwrap();
        let settings = test_settings(directory.path(), Some(descriptor), Some(change_descriptor));
        let store = Arc::new(Store::open(settings.clone()).unwrap());
        let wallet = Arc::new(Wallet::open(settings, store.clone()).unwrap());

        TestWallet {
            wallet,
            store,
            _directory: directory,
        }
    }

    fn test_settings(
        data_dir: &Path,
        descriptor: Option<String>,
        change_descriptor: Option<String>,
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
                descriptor,
                change_descriptor,
                0,
            )
            .unwrap(),
        )
    }

    fn persisted_next_external_index(store: &Store) -> u32 {
        let changeset = store.read_wallet_changeset().unwrap();
        let mut wallet = bdk_wallet::Wallet::load()
            .check_network(Network::Regtest)
            .load_wallet_no_persist(changeset)
            .unwrap()
            .expect("wallet state persisted");

        wallet.reveal_next_address(KeychainKind::External).index
    }

    #[test]
    fn generate_from_mnemonic_is_deterministic() {
        let mnemonic: Mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".parse().unwrap();

        let (_, descriptor_a, change_descriptor_a) =
            Wallet::generate_from_mnemonic(mnemonic.clone(), bitcoin::Network::Bitcoin).unwrap();
        let (_, descriptor_b, change_descriptor_b) =
            Wallet::generate_from_mnemonic(mnemonic, bitcoin::Network::Bitcoin).unwrap();

        assert_eq!(descriptor_a, descriptor_b);
        assert_eq!(change_descriptor_a, change_descriptor_b);
    }

    #[test]
    fn consecutive_addresses_have_increasing_indexes() {
        let wallet = test_wallet();

        assert_eq!(wallet.address().unwrap().index, 0);
        assert_eq!(wallet.address().unwrap().index, 1);
        assert_eq!(wallet.address().unwrap().index, 2);
    }

    #[test]
    fn address_returns_distinct_reveals() {
        let wallet = test_wallet();

        let a = wallet.address().unwrap();
        let b = wallet.address().unwrap();
        assert_ne!(a.index, b.index);
        assert_ne!(a.address, b.address);
    }

    #[test]
    fn concurrent_addresses_are_persisted() {
        let wallet = test_wallet();
        let count = 16;
        let barrier = Arc::new(std::sync::Barrier::new(count));

        let mut indexes = (0..count)
            .map(|_| {
                let wallet = wallet.wallet.clone();
                let barrier = barrier.clone();

                thread::spawn(move || {
                    barrier.wait();
                    wallet.address().unwrap().index
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();

        indexes.sort();

        assert_eq!(indexes, (0..count as u32).collect::<Vec<_>>());
        assert_eq!(persisted_next_external_index(&wallet.store), count as u32);
    }
}
