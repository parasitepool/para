use {
    super::*,
    bdk_bitcoind_rpc::{Emitter, bitcoincore_rpc, bitcoincore_rpc::RpcApi},
    bdk_wallet::{KeychainKind, keys::bip39::Mnemonic},
    bitcoin::bip32::{ChildNumber, DerivationPath, Xpriv},
    miniscript::{
        Descriptor,
        descriptor::{DescriptorSecretKey, DescriptorXKey, KeyMap, Wildcard},
    },
    rand::RngCore,
};

pub struct Wallet {
    inner: Mutex<bdk_wallet::Wallet>,
    rpc: bitcoincore_rpc::Client,
    birthday: u32,
    dust_limit: Amount,
    synced: AtomicBool,
}

impl Wallet {
    pub fn new(
        descriptor: &str,
        change_descriptor: Option<&str>,
        network: Network,
        rpc_url: &str,
        rpc_auth: bitcoincore_rpc::Auth,
        birthday: u32,
    ) -> Result<Self> {
        let inner = if let Some(change_descriptor) = change_descriptor {
            bdk_wallet::Wallet::create(descriptor.to_owned(), change_descriptor.to_owned())
        } else {
            bdk_wallet::Wallet::create_single(descriptor.to_owned())
        }
        .network(network)
        .create_wallet_no_persist()
        .context("failed to create wallet")?;

        let rpc = bitcoincore_rpc::Client::new(rpc_url, rpc_auth)
            .context("failed to create rpc client")?;

        let dust_limit = inner
            .peek_address(KeychainKind::External, 0)
            .address
            .script_pubkey()
            .minimal_non_dust();

        Ok(Self {
            inner: Mutex::new(inner),
            rpc,
            birthday,
            dust_limit,
            synced: AtomicBool::new(false),
        })
    }

    pub fn is_synced(&self) -> bool {
        self.synced.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn mark_synced(&self) {
        self.synced.store(true, Ordering::Relaxed);
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
                        match task::spawn_blocking(move || wallet_clone.sync(&cancel_clone))
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

        Ok(())
    }

    pub fn address(&self) -> bdk_wallet::AddressInfo {
        self.inner
            .lock()
            .reveal_next_address(KeychainKind::External)
    }

    pub fn balance(&self) -> bdk_wallet::Balance {
        self.inner.lock().balance()
    }

    pub fn reserve_address(&self) -> bdk_wallet::AddressInfo {
        let mut inner = self.inner.lock();
        let info = inner.next_unused_address(KeychainKind::External);
        inner.mark_used(KeychainKind::External, info.index);
        info
    }

    pub fn release_address(&self, index: u32) {
        self.inner.lock().unmark_used(KeychainKind::External, index);
    }

    pub fn dust_limit(&self) -> Amount {
        self.dust_limit
    }

    pub fn tip(&self) -> u32 {
        self.inner.lock().latest_checkpoint().height()
    }

    pub fn confirmed_received(&self, derivation_index: u32) -> (Amount, Vec<OutPoint>) {
        let (amount, outpoints, _) = self.check_payment(derivation_index, true);
        (amount, outpoints)
    }

    pub fn check_payment(
        &self,
        derivation_index: u32,
        confirmed_only: bool,
    ) -> (Amount, Vec<OutPoint>, u32) {
        let inner = self.inner.lock();
        let tip = inner.latest_checkpoint().height();
        let mut amount = Amount::ZERO;
        let mut outpoints = Vec::new();

        for utxo in inner.list_unspent() {
            if utxo.keychain == KeychainKind::External
                && utxo.derivation_index == derivation_index
                && (!confirmed_only || utxo.chain_position.is_confirmed())
            {
                amount += utxo.txout.value;
                outpoints.push(utxo.outpoint);
            }
        }

        (amount, outpoints, tip)
    }

    pub fn send(&self, to: Address, amount: Amount, fee_rate: FeeRate) -> Result<Txid> {
        let mut inner = self.inner.lock();
        let mut builder = inner.build_tx();
        builder.add_recipient(to.script_pubkey(), amount);
        builder.fee_rate(fee_rate);
        let mut psbt = builder.finish()?;

        #[allow(deprecated)]
        let finalized = inner.sign(&mut psbt, bdk_wallet::SignOptions::default())?;

        ensure!(finalized, "failed to finalize transaction");

        let tx = psbt.extract_tx_unchecked_fee_rate();
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_wallet() -> Wallet {
        let mnemonic: Mnemonic = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about".parse().unwrap();

        let (_, descriptor, change_descriptor) =
            Wallet::generate_from_mnemonic(mnemonic, bitcoin::Network::Regtest).unwrap();

        Wallet::new(
            &descriptor,
            Some(&change_descriptor),
            bitcoin::Network::Regtest,
            "http://127.0.0.1:1",
            bitcoincore_rpc::Auth::None,
            0,
        )
        .unwrap()
    }

    fn confirm_payment(wallet: &Wallet, address: &Address, amount: Amount) {
        let mut inner = wallet.inner.lock();
        let tip = inner.latest_checkpoint();

        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(
                    Txid::from_byte_array([tip.height() as u8 + 1; 32]),
                    0,
                ),
                ..TxIn::default()
            }],
            output: vec![TxOut {
                value: amount,
                script_pubkey: address.script_pubkey(),
            }],
        };

        let block = Block {
            header: Header {
                version: block::Version::from_consensus(4),
                prev_blockhash: tip.hash(),
                merkle_root: bitcoin::TxMerkleNode::from_raw_hash(
                    BlockHash::all_zeros().to_raw_hash(),
                ),
                time: 0,
                bits: CompactTarget::from_consensus(0x207fffff),
                nonce: 0,
            },
            txdata: vec![tx],
        };

        inner
            .apply_block_connected_to(&block, tip.height() + 1, tip.block_id())
            .unwrap();
    }

    #[test]
    fn generate() {
        let (mnemonic, descriptor, change_descriptor) =
            Wallet::generate(bitcoin::Network::Regtest).unwrap();

        assert_eq!(mnemonic.split_whitespace().count(), 12);
        assert!(descriptor.starts_with("tr("));
        assert!(change_descriptor.starts_with("tr("));
        assert_ne!(descriptor, change_descriptor);

        let wallet = Wallet::new(
            &descriptor,
            Some(&change_descriptor),
            bitcoin::Network::Regtest,
            "http://127.0.0.1:1",
            bitcoincore_rpc::Auth::None,
            0,
        )
        .unwrap();

        let address = wallet.address();
        assert!(address.address.to_string().starts_with("bcrt1p"));
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
    fn address_reservation() {
        let wallet = test_wallet();

        let a = wallet.reserve_address();
        let b = wallet.reserve_address();
        assert_ne!(a.index, b.index);
        assert_ne!(a.address, b.address);

        let address = a.address.clone();
        wallet.release_address(a.index);
        let c = wallet.reserve_address();
        assert_eq!(c.address, address);
    }

    #[test]
    fn confirmed_received() {
        let wallet = test_wallet();
        let info_a = wallet.reserve_address();
        let info_b = wallet.reserve_address();

        confirm_payment(&wallet, &info_a.address, Amount::from_sat(500));
        confirm_payment(&wallet, &info_a.address, Amount::from_sat(700));

        let (amount, outpoints) = wallet.confirmed_received(info_a.index);
        assert_eq!(amount, Amount::from_sat(1200));
        assert_eq!(outpoints.len(), 2);

        let (amount, outpoints) = wallet.confirmed_received(info_b.index);
        assert_eq!(amount, Amount::ZERO);
        assert!(outpoints.is_empty());
    }

    #[test]
    fn confirmed_received_ignores_unconfirmed() {
        let wallet = test_wallet();
        let info = wallet.reserve_address();

        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(Txid::from_byte_array([1; 32]), 0),
                ..TxIn::default()
            }],
            output: vec![TxOut {
                value: Amount::from_sat(1000),
                script_pubkey: info.address.script_pubkey(),
            }],
        };
        wallet.inner.lock().apply_unconfirmed_txs([(tx, 0)]);

        let (amount, outpoints) = wallet.confirmed_received(info.index);
        assert_eq!(amount, Amount::ZERO);
        assert!(outpoints.is_empty());

        let (amount, outpoints, _) = wallet.check_payment(info.index, false);
        assert_eq!(amount, Amount::from_sat(1000));
        assert_eq!(outpoints.len(), 1);
    }
}
