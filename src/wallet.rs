use {
    super::*,
    crate::router::error::{RouterError, RouterResult},
    bdk_bitcoind_rpc::{Emitter, MempoolEvent, bitcoincore_rpc, bitcoincore_rpc::RpcApi},
    bdk_wallet::{
        ChangeSet, KeychainKind,
        chain::{CanonicalizationParams, Merge, bdk_core::BlockId},
        keys::bip39::Mnemonic,
    },
    bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv},
    miniscript::{
        Descriptor,
        descriptor::{DescriptorSecretKey, DescriptorXKey, KeyMap, Wildcard},
    },
    rand::RngCore,
};

type RpcEmitter = Emitter<Arc<bitcoincore_rpc::Client>>;

pub(crate) struct SyncState {
    ready: bool,
    snapshot: Arc<ReceiptSnapshot>,
}

impl SyncState {
    pub(crate) fn snapshot(&self) -> Option<Arc<ReceiptSnapshot>> {
        self.ready.then(|| self.snapshot.clone())
    }
}

#[derive(Default)]
struct Receipt {
    total: Amount,
    confirmed: BTreeMap<u32, Amount>,
    txids: Vec<Txid>,
}

pub(crate) struct ReceiptSnapshot {
    checkpoint: BlockId,
    receipts: HashMap<u32, Receipt>,
}

impl ReceiptSnapshot {
    fn new(inner: &bdk_wallet::Wallet) -> Self {
        let checkpoint = inner.latest_checkpoint().block_id();
        let mut receipts = HashMap::<u32, Receipt>::new();

        for ((_, index), txout) in inner.tx_graph().filter_chain_txouts(
            inner.local_chain(),
            checkpoint,
            CanonicalizationParams::default(),
            inner
                .spk_index()
                .inner()
                .outputs_in_range((KeychainKind::External, 0)..=(KeychainKind::External, u32::MAX)),
        ) {
            let receipt = receipts.entry(*index).or_default();
            receipt.total += txout.txout.value;
            if let Some(height) = txout.chain_position.confirmation_height_upper_bound() {
                *receipt.confirmed.entry(height).or_default() += txout.txout.value;
            }
            receipt.txids.push(txout.outpoint.txid);
            if let Some((_, txid)) = txout.spent_by {
                receipt.txids.push(txid);
            }
        }

        for receipt in receipts.values_mut() {
            receipt.txids.sort_unstable();
            receipt.txids.dedup();
        }

        Self {
            checkpoint,
            receipts,
        }
    }

    pub(crate) fn tip(&self) -> u32 {
        self.checkpoint.height
    }

    pub(crate) fn received(&self, index: u32) -> Amount {
        self.receipts
            .get(&index)
            .map_or(Amount::ZERO, |receipt| receipt.total)
    }

    pub(crate) fn received_by_deadline(&self, index: u32, deadline: u32) -> Amount {
        self.receipts.get(&index).map_or(Amount::ZERO, |receipt| {
            receipt
                .confirmed
                .range(..=deadline)
                .map(|(_, amount)| *amount)
                .sum()
        })
    }

    pub(crate) fn txids_by_derivation_index(&self, index: u32) -> Vec<Txid> {
        self.receipts
            .get(&index)
            .map(|receipt| receipt.txids.clone())
            .unwrap_or_default()
    }

    pub(crate) fn confirmed_by_index(&self) -> impl Iterator<Item = (u32, Amount)> + '_ {
        self.receipts
            .iter()
            .map(|(index, receipt)| (*index, receipt.confirmed.values().copied().sum()))
    }
}

pub struct Wallet {
    inner: Mutex<bdk_wallet::Wallet>,
    settings: Arc<Settings>,
    store: Arc<Store>,
    rpc: Arc<bitcoincore_rpc::Client>,
    emitter: Mutex<RpcEmitter>,
    dust_limit: Amount,
    sync_tx: watch::Sender<SyncState>,
    stale_emitter: AtomicBool,
    reload_required: AtomicBool,
}

impl Wallet {
    pub(crate) fn open(settings: Arc<Settings>, store: Arc<Store>) -> Result<Self> {
        let inner = Self::load_inner(&settings, &store)?;

        let rpc = Arc::new(
            bitcoincore_rpc::Client::new(
                &format!("http://{}", settings.bitcoin_rpc_url()),
                settings.wallet_rpc_auth()?,
            )
            .context("failed to create rpc client")?,
        );

        let dust_limit = inner
            .peek_address(KeychainKind::External, 0)
            .address
            .script_pubkey()
            .minimal_non_dust();

        let emitter = Mutex::new(Self::emitter_from(&rpc, &inner, settings.wallet_birthday()));
        let sync_tx = watch::channel(SyncState {
            ready: false,
            snapshot: Arc::new(ReceiptSnapshot::new(&inner)),
        })
        .0;

        let wallet = Self {
            inner: Mutex::new(inner),
            settings: settings.clone(),
            store,
            rpc,
            emitter,
            dust_limit,
            sync_tx,
            stale_emitter: AtomicBool::new(false),
            reload_required: AtomicBool::new(false),
        };

        Ok(wallet)
    }

    fn load_inner(settings: &Settings, store: &Store) -> Result<bdk_wallet::Wallet> {
        let descriptor = settings.descriptor();
        let change_descriptor = settings.change_descriptor();
        let network = settings.chain().network();
        let changeset = store.read_wallet_changeset()?;

        Ok(if changeset.is_empty() {
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
        })
    }

    fn reload_inner(&self, inner: &mut bdk_wallet::Wallet) -> Result {
        self.reload_required.store(true, Ordering::Release);
        self.mark_unsynced();
        *inner = Self::load_inner(&self.settings, &self.store)?;
        self.reload_required.store(false, Ordering::Release);
        Ok(())
    }

    pub fn is_synced(&self) -> bool {
        self.sync_tx.borrow().ready
    }

    pub(crate) fn snapshot(&self) -> Option<Arc<ReceiptSnapshot>> {
        self.sync_tx.borrow().snapshot()
    }

    pub(crate) fn last_snapshot(&self) -> Arc<ReceiptSnapshot> {
        self.sync_tx.borrow().snapshot.clone()
    }

    fn require_synced(&self) -> RouterResult<()> {
        if self.is_synced() {
            Ok(())
        } else {
            Err(RouterError::WalletSyncing)
        }
    }

    pub async fn synced(&self) -> bool {
        let mut rx = self.sync_tx.subscribe();
        while !rx.borrow_and_update().ready {
            if rx.changed().await.is_err() {
                return false;
            }
        }
        true
    }

    pub(crate) fn subscribe_sync(&self) -> watch::Receiver<SyncState> {
        self.sync_tx.subscribe()
    }

    pub(crate) fn spawn(
        self: &Arc<Self>,
        interval: Duration,
        cancel: CancellationToken,
        tasks: &TaskTracker,
    ) {
        info!("Syncing wallet in background...");

        let wallet = self.clone();
        let blocking_tasks = tasks.clone();

        tasks.spawn(async move {
            let mut ticker = ticker(interval);
            loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let wallet_clone = wallet.clone();
                        let cancel_clone = cancel.clone();
                        match blocking_tasks.spawn_blocking(move || wallet_clone.sync(&cancel_clone))
                            .await
                            .unwrap_or_else(|err| Err(err.into()))
                        {
                            Ok(()) => {}
                            Err(e) => {
                                if !cancel.is_cancelled() {
                                    wallet.mark_unsynced();
                                    warn!("Wallet sync error: {e}");
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    fn emitter_from(
        rpc: &Arc<bitcoincore_rpc::Client>,
        inner: &bdk_wallet::Wallet,
        birthday: u32,
    ) -> RpcEmitter {
        let txs = inner
            .transactions()
            .filter(|tx| !tx.chain_position.is_confirmed())
            .map(|tx| tx.tx_node.tx.clone())
            .collect::<Vec<_>>();

        Emitter::new(rpc.clone(), inner.latest_checkpoint(), birthday, txs)
    }

    pub fn sync(&self, cancel: &CancellationToken) -> Result {
        let start = Instant::now();
        let mut emitter = self.emitter.lock();

        let result = (|| -> Result<(u32, MempoolEvent)> {
            let mut inner = self.inner.lock();
            if self.reload_required.load(Ordering::Acquire) {
                self.reload_inner(&mut inner)?;
            }
            if self.stale_emitter.swap(false, Ordering::AcqRel) {
                *emitter = Self::emitter_from(&self.rpc, &inner, self.settings.wallet_birthday());
            }
            drop(inner);

            let mut blocks = 0u32;
            while let Some(event) = emitter.next_block()? {
                let mut inner = self.inner.lock();
                ensure!(
                    !self.stale_emitter.load(Ordering::Acquire),
                    "wallet reloaded during sync"
                );
                inner.apply_block_connected_to(
                    &event.block,
                    event.block_height(),
                    event.connected_to(),
                )?;

                blocks += 1;

                if cancel.is_cancelled() {
                    bail!("wallet sync cancelled");
                }
            }

            if cancel.is_cancelled() {
                bail!("wallet sync cancelled");
            }

            let mempool = emitter.mempool()?;
            Ok((blocks, mempool))
        })();

        let (blocks, mempool) = match result {
            Ok(update) => update,
            Err(err) => {
                self.mark_unsynced();
                return Err(err);
            }
        };
        let (mempool_txs, evicted) = self.apply_mempool(mempool, cancel)?;

        let elapsed = start.elapsed();
        debug!(
            "Wallet sync: {blocks} blocks, {mempool_txs} mempool txs, {evicted} evicted, {elapsed:?}"
        );
        if elapsed > Duration::from_secs(1) {
            info!(
                "Wallet sync took {elapsed:?}: {blocks} blocks, {mempool_txs} mempool txs, {evicted} evicted"
            );
        }

        Ok(())
    }

    fn apply_mempool(
        &self,
        mut mempool: MempoolEvent,
        cancel: &CancellationToken,
    ) -> Result<(usize, usize)> {
        let mut inner = self.inner.lock();
        if cancel.is_cancelled()
            || self.stale_emitter.load(Ordering::Acquire)
            || self.reload_required.load(Ordering::Acquire)
        {
            self.mark_unsynced();
            bail!("wallet sync cancelled or wallet reloaded");
        }

        let mempool_txs = mempool.update.len();
        mempool
            .evicted
            .retain(|(txid, _)| inner.tx_graph().get_tx(*txid).is_some());
        let evicted = mempool.evicted.len();
        if !mempool.evicted.is_empty() {
            inner.apply_evicted_txs(mempool.evicted);
        }
        inner.apply_unconfirmed_txs(mempool.update);

        self.publish(&inner)?;
        Ok((mempool_txs, evicted))
    }

    fn mark_unsynced(&self) {
        self.stale_emitter.store(true, Ordering::Release);
        self.sync_tx.send_modify(|state| state.ready = false);
    }

    fn publish(&self, inner: &bdk_wallet::Wallet) -> Result {
        ensure!(
            !self.stale_emitter.load(Ordering::Acquire)
                && !self.reload_required.load(Ordering::Acquire),
            "wallet reloaded during sync"
        );
        let snapshot = Arc::new(ReceiptSnapshot::new(inner));
        if !self
            .sync_tx
            .send_replace(SyncState {
                ready: true,
                snapshot,
            })
            .ready
        {
            info!("Wallet synced");
        }
        Ok(())
    }

    pub fn address(&self) -> Result<bdk_wallet::AddressInfo> {
        task::block_in_place(|| {
            let mut inner = self.inner.lock();
            Ok(inner.reveal_next_address(KeychainKind::External))
        })
    }

    pub(crate) fn reveal_address_with<T>(
        &self,
        persist: impl FnOnce(bdk_wallet::AddressInfo, u32, &ChangeSet) -> Result<T>,
    ) -> RouterResult<T> {
        let mut inner = self.inner.lock();
        self.require_synced()?;
        let created_at_height = inner.latest_checkpoint().height();
        let address = inner.reveal_next_address(KeychainKind::External);
        let err = {
            let Some(staged) = inner.staged_mut() else {
                return persist(address, created_at_height, &ChangeSet::default())
                    .map_err(|error| RouterError::WalletPersistence { error });
            };

            match persist(address, created_at_height, staged) {
                Ok(value) => {
                    let _ = staged.take();
                    return Ok(value);
                }
                Err(err) => err,
            }
        };

        if let Err(reload_err) = self.reload_inner(&mut inner) {
            error!("wallet rollback failed after persist error: {reload_err}");
        }

        Err(RouterError::WalletPersistence { error: err })
    }

    #[cfg(test)]
    pub(crate) fn take_staged(&self) -> ChangeSet {
        let mut inner = self.inner.lock();
        let Some(staged) = inner.staged_mut() else {
            return ChangeSet::default();
        };

        staged.take().unwrap_or_default()
    }

    pub(crate) fn persist_staged_with(&self, persist: impl FnOnce(&ChangeSet) -> Result) -> Result {
        let mut inner = self.inner.lock();
        ensure!(
            !self.reload_required.load(Ordering::Acquire),
            "wallet recovery is required before persistence"
        );
        let Some(staged) = inner.staged_mut() else {
            return persist(&ChangeSet::default());
        };

        persist(staged)?;
        let _ = staged.take();

        Ok(())
    }

    pub fn balance(&self) -> bdk_wallet::Balance {
        self.inner.lock().balance()
    }

    pub fn dust_limit(&self) -> Amount {
        self.dust_limit
    }

    #[cfg(test)]
    pub(crate) fn tip(&self) -> u32 {
        self.inner.lock().latest_checkpoint().height()
    }

    pub(crate) fn build_refund_psbt(
        &self,
        id: u32,
        derivation_index: u32,
        destination: Address,
        fee_rate: FeeRate,
    ) -> RouterResult<Psbt> {
        let mut inner = self.inner.lock();
        let tip = inner.latest_checkpoint().height();
        let outpoints = inner
            .list_unspent()
            .filter(|txout| {
                txout.keychain == KeychainKind::External
                    && txout.derivation_index == derivation_index
                    && txout
                        .chain_position
                        .confirmation_height_upper_bound()
                        .is_some_and(|height| {
                            !inner
                                .tx_graph()
                                .get_tx(txout.outpoint.txid)
                                .is_some_and(|tx| tx.is_coinbase())
                                || tip.saturating_add(1) >= height.saturating_add(COINBASE_MATURITY)
                        })
            })
            .map(|txout| txout.outpoint)
            .collect::<Vec<_>>();

        if outpoints.is_empty() {
            return Err(RouterError::NoUnspentFunds { id });
        }

        (|| -> Result<Psbt> {
            let mut builder = inner.build_tx();
            builder
                .add_utxos(&outpoints)
                .context("failed to add refund inputs")?;
            builder.manually_selected_only();
            builder.drain_to(destination.script_pubkey());
            builder.fee_rate(fee_rate);
            Ok(builder.finish()?)
        })()
        .map_err(|error| RouterError::RefundConstruction { error })
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
        let inner = self.inner.lock();
        self.stale_emitter.store(false, Ordering::Release);
        self.publish(&inner).unwrap();
    }

    #[cfg(test)]
    pub(crate) fn test_reveal_address(&self) -> bdk_wallet::AddressInfo {
        let mut inner = self.inner.lock();
        inner.reveal_next_address(KeychainKind::External)
    }

    #[cfg(test)]
    pub(crate) fn test_receive_unconfirmed(
        &self,
        address: &Address,
        amount: Amount,
    ) -> Transaction {
        let mut inner = self.inner.lock();
        let tx = Self::test_payment_tx(address, amount);
        inner.apply_unconfirmed_txs([(tx.clone(), 1)]);
        tx
    }

    #[cfg(test)]
    pub(crate) fn test_confirm_tx(&self, tx: Transaction) {
        let mut inner = self.inner.lock();
        let previous = inner.latest_checkpoint().block_id();
        let block = Self::test_block(previous.hash, vec![tx]);
        inner
            .apply_block_connected_to(&block, previous.height + 1, previous)
            .unwrap();
    }

    #[cfg(test)]
    pub(crate) fn test_evict_tx(&self, tx: &Transaction) {
        let mut inner = self.inner.lock();
        inner.apply_evicted_txs([(tx.compute_txid(), 2)]);
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
    }

    #[cfg(test)]
    fn test_payment_tx(address: &Address, amount: Amount) -> Transaction {
        let txid = Txid::from_raw_hash(bitcoin::hashes::sha256d::Hash::hash(
            address.script_pubkey().as_bytes(),
        ));

        Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint { txid, vout: 0 },
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
    use {super::*, crate::router::testkit::*};

    fn persist_staged(wallet: &TestWallet) {
        wallet
            .store
            .persist_wallet_delta(&wallet.take_staged())
            .unwrap();
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
    fn concurrent_addresses_are_staged_until_persisted() {
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
        assert_eq!(
            wallet.store.read_wallet_changeset().unwrap(),
            ChangeSet::default()
        );

        persist_staged(&wallet);

        assert_eq!(persisted_next_external_index(&wallet.store), count as u32);
    }

    #[test]
    fn reveal_address_with_rolls_back_on_persist_failure() {
        let wallet = test_wallet();
        wallet.mark_synced();

        let persisted = wallet
            .reveal_address_with(|address, _, wallet_delta| {
                wallet.store.persist_wallet_delta(wallet_delta)?;
                Ok(address.index)
            })
            .unwrap();
        assert_eq!(persisted, 0);
        assert_eq!(persisted_next_external_index(&wallet.store), 1);

        let result: RouterResult<()> = wallet.reveal_address_with(|address, _, _| {
            assert_eq!(address.index, 1);
            Err(anyhow!("store write failed"))
        });
        assert!(result.is_err());
        assert!(!wallet.is_synced());

        assert_eq!(wallet.address().unwrap().index, 1);
    }

    #[test]
    fn failed_persist_rolls_back_staged_changes() {
        let wallet = test_wallet();
        wallet.mark_synced();

        let result: RouterResult<()> = wallet.reveal_address_with(|address, _, _| {
            assert_eq!(address.index, 0);
            Err(anyhow!("store write failed"))
        });
        assert!(result.is_err());
        assert_eq!(
            wallet.store.read_wallet_changeset().unwrap(),
            ChangeSet::default()
        );
        assert_eq!(wallet.address().unwrap().index, 0);
    }

    #[test]
    fn txids_by_derivation_index() {
        #[track_caller]
        fn case(confirmed: bool) {
            let wallet = test_wallet();
            let address = wallet.test_reveal_address();
            let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));

            if confirmed {
                wallet.test_confirm_tx(tx.clone());
            }

            wallet.mark_synced();
            let snapshot = wallet.snapshot().unwrap();
            let txids = snapshot.txids_by_derivation_index(address.index);
            assert_eq!(txids, vec![tx.compute_txid()]);

            assert!(
                snapshot
                    .txids_by_derivation_index(address.index + 1)
                    .is_empty()
            );
        }

        case(false);
        case(true);
    }

    #[test]
    fn build_refund_psbt_spends_only_selected_outpoints() {
        let wallet = test_wallet();

        let included = wallet.test_reveal_address();
        let excluded = wallet.test_reveal_address();

        let tx = wallet.test_receive_unconfirmed(&included.address, Amount::from_sat(10_000));
        let outpoint = OutPoint {
            txid: tx.compute_txid(),
            vout: 0,
        };
        wallet.test_confirm_tx(tx);

        let tx = wallet.test_receive_unconfirmed(&excluded.address, Amount::from_sat(20_000));
        wallet.test_confirm_tx(tx);

        wallet.mark_synced();

        let destination = wallet.test_reveal_address().address;

        let psbt = wallet
            .build_refund_psbt(
                0,
                included.index,
                destination.clone(),
                FeeRate::from_sat_per_vb(1).unwrap(),
            )
            .unwrap();

        assert_eq!(psbt.unsigned_tx.input.len(), 1);
        assert_eq!(psbt.unsigned_tx.input[0].previous_output, outpoint);
        assert_eq!(psbt.unsigned_tx.output.len(), 1);
        assert_eq!(
            psbt.unsigned_tx.output[0].script_pubkey,
            destination.script_pubkey(),
        );
        assert!(psbt.unsigned_tx.output[0].value < Amount::from_sat(10_000));
        assert!(psbt.inputs[0].final_script_witness.is_none());
    }

    #[test]
    fn snapshot_retains_spent_receipts_and_deadline_heights() {
        let wallet = test_wallet();
        let address = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(10_000));
        let mut spend = Wallet::test_payment_tx(
            &wallet.test_reveal_address().address,
            Amount::from_sat(9000),
        );
        spend.input[0].previous_output = OutPoint {
            txid: tx.compute_txid(),
            vout: 0,
        };
        wallet.test_confirm_tx(tx.clone());
        wallet.test_confirm_tx(spend.clone());
        wallet.mark_synced();
        let snapshot = wallet.snapshot().unwrap();
        assert_eq!(snapshot.received(address.index), Amount::from_sat(10_000));
        assert_eq!(
            snapshot.received_by_deadline(address.index, 0),
            Amount::ZERO
        );
        assert_eq!(
            snapshot.received_by_deadline(address.index, 1),
            Amount::from_sat(10_000)
        );
        let mut txids = vec![tx.compute_txid(), spend.compute_txid()];
        txids.sort_unstable();
        assert_eq!(snapshot.txids_by_derivation_index(address.index), txids);
        let _inner = wallet.inner.lock();
        for _ in 0..100 {
            assert_eq!(
                wallet.snapshot().unwrap().received(address.index),
                Amount::from_sat(10_000)
            );
        }
    }

    #[test]
    fn failed_periodic_persistence_keeps_staged_changes() {
        let wallet = test_wallet();
        wallet.test_reveal_address();
        let staged = wallet.inner.lock().staged().unwrap().clone();
        assert!(wallet.persist_staged_with(|_| bail!("foo")).is_err());
        assert_eq!(wallet.inner.lock().staged(), Some(&staged));
        wallet
            .persist_staged_with(|delta| wallet.store.persist_wallet_delta(delta))
            .unwrap();
        assert!(wallet.inner.lock().staged().is_none());
    }

    #[test]
    fn unavailable_wallet_retains_receipt_history() {
        let wallet = test_wallet();
        let address = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&address.address, Amount::from_sat(1000));
        let txid = tx.compute_txid();
        wallet.test_confirm_tx(tx);
        wallet.mark_synced();
        wallet
            .persist_staged_with(|delta| wallet.store.persist_wallet_delta(delta))
            .unwrap();
        let snapshot = wallet.last_snapshot();

        assert!(wallet.sync(&CancellationToken::new()).is_err());
        assert!(wallet.snapshot().is_none());
        assert!(Arc::ptr_eq(&snapshot, &wallet.last_snapshot()));

        wallet.reload_inner(&mut wallet.inner.lock()).unwrap();
        assert!(wallet.snapshot().is_none());
        assert_eq!(
            wallet
                .last_snapshot()
                .txids_by_derivation_index(address.index),
            vec![txid]
        );

        let reopened = Wallet::open(wallet.settings.clone(), wallet.store.clone()).unwrap();
        assert!(reopened.snapshot().is_none());
        assert_eq!(
            reopened
                .last_snapshot()
                .txids_by_derivation_index(address.index),
            vec![txid]
        );
    }

    #[test]
    fn tip_change_keeps_mempool_progress() {
        let wallet = test_wallet();
        let foo = wallet.test_reveal_address();
        let bar = wallet.test_reveal_address();
        let tx = wallet.test_receive_unconfirmed(&foo.address, Amount::from_sat(1000));
        wallet.mark_synced();
        let event = MempoolEvent {
            update: vec![(
                Arc::new(Wallet::test_payment_tx(
                    &bar.address,
                    Amount::from_sat(2000),
                )),
                1,
            )],
            evicted: vec![(tx.compute_txid(), 2)],
        };

        assert_eq!(
            wallet
                .apply_mempool(event, &CancellationToken::new())
                .unwrap(),
            (1, 1)
        );
        let snapshot = wallet.snapshot().unwrap();
        assert_eq!(snapshot.tip(), 0);
        assert_eq!(snapshot.received(foo.index), Amount::ZERO);
        assert_eq!(snapshot.received(bar.index), Amount::from_sat(2000));

        wallet.test_advance_tip_to(1);
        wallet
            .apply_mempool(MempoolEvent::default(), &CancellationToken::new())
            .unwrap();
        let snapshot = wallet.snapshot().unwrap();
        assert_eq!(snapshot.tip(), 1);
        assert_eq!(snapshot.received(foo.index), Amount::ZERO);
        assert_eq!(snapshot.received(bar.index), Amount::from_sat(2000));
    }

    #[test]
    fn unrelated_evictions_are_ignored() {
        let wallet = test_wallet();
        let event = MempoolEvent {
            evicted: vec![(Txid::all_zeros(), 1)],
            ..Default::default()
        };
        assert_eq!(
            wallet
                .apply_mempool(event, &CancellationToken::new())
                .unwrap(),
            (0, 0)
        );
        assert!(wallet.is_synced());
    }

    #[test]
    fn failed_reload_requires_recovery_before_publication() {
        let wallet = test_wallet();
        wallet.test_reveal_address();
        wallet.mark_synced();
        wallet
            .store
            .persist_wallet_delta(&ChangeSet {
                network: Some(Network::Regtest),
                ..Default::default()
            })
            .unwrap();

        assert!(wallet.reload_inner(&mut wallet.inner.lock()).is_err());
        assert!(!wallet.is_synced());
        wallet.stale_emitter.store(false, Ordering::Release);
        assert!(wallet.publish(&wallet.inner.lock()).is_err());
        assert!(wallet.sync(&CancellationToken::new()).is_err());
        assert!(wallet.reload_required.load(Ordering::Acquire));
        assert!(
            wallet
                .persist_staged_with(|_| panic!("unexpected persistence"))
                .is_err()
        );

        let delta = wallet.inner.lock().staged().unwrap().clone();
        wallet.store.persist_wallet_delta(&delta).unwrap();
        assert!(wallet.sync(&CancellationToken::new()).is_err());
        assert!(!wallet.reload_required.load(Ordering::Acquire));
        assert!(!wallet.is_synced());
    }
}
