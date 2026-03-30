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
    inner: bdk_wallet::Wallet,
    rpc: bitcoincore_rpc::Client,
}

impl Wallet {
    pub fn new(
        descriptor: &str,
        change_descriptor: Option<&str>,
        network: Network,
        rpc_url: &str,
        rpc_auth: bitcoincore_rpc::Auth,
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

        Ok(Self { inner, rpc })
    }

    pub fn sync(&mut self, birthday: u32) -> Result {
        let expected_mempool_txs = self
            .inner
            .transactions()
            .filter(|tx| !tx.chain_position.is_confirmed())
            .map(|tx| tx.tx_node.tx.clone())
            .collect::<Vec<_>>();

        let mut emitter = Emitter::new(
            &self.rpc,
            self.inner.latest_checkpoint(),
            birthday,
            expected_mempool_txs,
        );

        while let Some(event) = emitter.next_block()? {
            self.inner.apply_block_connected_to(
                &event.block,
                event.block_height(),
                event.connected_to(),
            )?;
        }

        let mempool = emitter.mempool()?;
        self.inner.apply_unconfirmed_txs(mempool.update);
        self.inner.apply_evicted_txs(mempool.evicted);

        Ok(())
    }

    pub fn address(&mut self) -> bdk_wallet::AddressInfo {
        self.inner.reveal_next_address(KeychainKind::External)
    }

    pub fn balance(&self) -> bdk_wallet::Balance {
        self.inner.balance()
    }

    pub fn send(&mut self, to: Address, amount: Amount, fee_rate: FeeRate) -> Result<Txid> {
        let mut builder = self.inner.build_tx();
        builder.add_recipient(to.script_pubkey(), amount);
        builder.fee_rate(fee_rate);
        let mut psbt = builder.finish()?;

        #[allow(deprecated)]
        let finalized = self
            .inner
            .sign(&mut psbt, bdk_wallet::SignOptions::default())?;

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

    #[test]
    fn generate() {
        let (mnemonic, descriptor, change_descriptor) =
            Wallet::generate(bitcoin::Network::Regtest).unwrap();

        assert_eq!(mnemonic.split_whitespace().count(), 12);
        assert!(descriptor.starts_with("tr("));
        assert!(change_descriptor.starts_with("tr("));
        assert_ne!(descriptor, change_descriptor);

        let mut wallet = Wallet::new(
            &descriptor,
            Some(&change_descriptor),
            bitcoin::Network::Regtest,
            "http://127.0.0.1:1",
            bitcoincore_rpc::Auth::None,
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
}
