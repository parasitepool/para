use {
    super::*,
    bdk_wallet::{ChangeSet, chain::Merge},
    redb::{Database, Durability, ReadableDatabase, ReadableTable, TableDefinition},
};

const SCHEMA_VERSION: u64 = 1;

const META: u32 = 0;
const METADATA: TableDefinition<u32, &[u8]> = TableDefinition::new("METADATA");

const CHANGESET: u32 = 0;
const WALLET: TableDefinition<u32, &[u8]> = TableDefinition::new("WALLET");

#[derive(Serialize, Deserialize)]
struct Metadata {
    schema_version: u64,
    chain: Chain,
}

pub(crate) struct Store {
    db: Database,
    durability: Durability,
}

impl Store {
    pub(crate) fn open(settings: Arc<Settings>) -> Result<Self> {
        let path = settings.store_path()?;
        let chain = settings.chain();

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create store directory {}", parent.display())
            })?;
        }

        let mut builder = Database::builder();

        builder.set_repair_callback(|session| {
            warn!("redb repair {:.0}%", session.progress() * 100.0);
        });

        let db = builder
            .create(&path)
            .with_context(|| format!("failed to open store database {}", path.display()))?;

        let store = Self {
            db,
            durability: Durability::Immediate,
        };

        let mut transaction = store.db.begin_write()?;
        transaction.set_quick_repair(true);
        transaction.set_durability(store.durability)?;

        {
            let mut metadata = transaction.open_table(METADATA)?;

            let stored: Option<Metadata> = metadata
                .get(META)?
                .map(|value| ciborium::from_reader(value.value()).context("decode store meta"))
                .transpose()?;

            match stored {
                Some(stored) => {
                    ensure!(
                        stored.schema_version == SCHEMA_VERSION,
                        "store schema version mismatch at {}: expected {SCHEMA_VERSION}, got {}",
                        path.display(),
                        stored.schema_version,
                    );

                    ensure!(
                        stored.chain == chain,
                        "store chain mismatch at {}: expected {chain}, got {}",
                        path.display(),
                        stored.chain,
                    );
                }
                None => {
                    let mut bytes = Vec::with_capacity(32);
                    ciborium::into_writer(
                        &Metadata {
                            schema_version: SCHEMA_VERSION,
                            chain,
                        },
                        &mut bytes,
                    )
                    .context("encode store meta")?;
                    metadata.insert(META, bytes.as_slice())?;
                }
            }
        }

        {
            transaction.open_table(WALLET)?;
        }

        transaction.commit()?;

        Ok(store)
    }

    pub(crate) fn read_wallet_changeset(&self) -> Result<ChangeSet> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(WALLET)?;

        table
            .get(CHANGESET)?
            .map(|value| ciborium::from_reader(value.value()).context("decode wallet state"))
            .transpose()
            .map(|changeset| changeset.unwrap_or_default())
    }

    pub(crate) fn persist_wallet_changeset(&self, delta: &ChangeSet) -> Result {
        if delta.is_empty() {
            return Ok(());
        }

        let mut transaction = self.db.begin_write()?;
        transaction.set_quick_repair(true);
        transaction.set_durability(self.durability)?;

        {
            let mut table = transaction.open_table(WALLET)?;
            let mut merged: ChangeSet = table
                .get(CHANGESET)?
                .map(|value| ciborium::from_reader(value.value()).context("decode wallet state"))
                .transpose()?
                .unwrap_or_default();

            merged.merge(delta.clone());

            let mut bytes = Vec::with_capacity(64);
            ciborium::into_writer(&merged, &mut bytes).context("encode wallet state")?;
            table.insert(CHANGESET, bytes.as_slice())?;
        }

        transaction.commit()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        bdk_wallet::chain::{local_chain, tx_graph},
    };

    fn temporary_store(chain: Chain) -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(test_settings(directory.path(), chain)).unwrap();
        store.durability = Durability::None;
        (directory, store)
    }

    fn test_settings(data_dir: &Path, chain: Chain) -> Arc<Settings> {
        Arc::new(
            Settings::from_wallet_options(
                BitcoinOptions {
                    chain: Some(chain),
                    bitcoin_data_dir: None,
                    bitcoin_rpc_port: Some(1),
                    bitcoin_rpc_cookie_file: None,
                    bitcoin_rpc_username: Some("user".into()),
                    bitcoin_rpc_password: Some("pass".into()),
                },
                Some(data_dir.to_path_buf()),
                None,
                None,
                0,
            )
            .unwrap(),
        )
    }

    fn txid(byte: u8) -> Txid {
        Txid::from_byte_array([byte; 32])
    }

    fn block_hash(byte: u8) -> BlockHash {
        BlockHash::from_byte_array([byte; 32])
    }

    fn network_changeset(network: Network) -> ChangeSet {
        ChangeSet {
            network: Some(network),
            ..Default::default()
        }
    }

    fn wallet_state_exists(store: &Store) -> bool {
        let transaction = store.db.begin_read().unwrap();
        let table = transaction.open_table(WALLET).unwrap();
        table.get(CHANGESET).unwrap().is_some()
    }

    fn metadata_entry_count(store: &Store) -> usize {
        let transaction = store.db.begin_read().unwrap();
        let table = transaction.open_table(METADATA).unwrap();
        table.iter().unwrap().count()
    }

    fn wallet_entry_count(store: &Store) -> usize {
        let transaction = store.db.begin_read().unwrap();
        let table = transaction.open_table(WALLET).unwrap();
        table.iter().unwrap().count()
    }

    #[test]
    fn empty_read_returns_default() {
        let (_directory, store) = temporary_store(Chain::Regtest);

        assert_eq!(store.read_wallet_changeset().unwrap(), ChangeSet::default());
    }

    #[test]
    fn persist_then_read_round_trips() {
        let (_directory, store) = temporary_store(Chain::Regtest);
        let changeset = network_changeset(Network::Regtest);

        store.persist_wallet_changeset(&changeset).unwrap();

        assert_eq!(store.read_wallet_changeset().unwrap(), changeset);
    }

    #[test]
    fn persist_empty_is_noop() {
        let (_directory, store) = temporary_store(Chain::Regtest);

        store
            .persist_wallet_changeset(&ChangeSet::default())
            .unwrap();

        assert!(!wallet_state_exists(&store));
        assert_eq!(store.read_wallet_changeset().unwrap(), ChangeSet::default());
    }

    #[test]
    fn multiple_persists_read_back_merged_state() {
        let (_directory, store) = temporary_store(Chain::Regtest);
        let deltas = [
            network_changeset(Network::Regtest),
            ChangeSet {
                local_chain: local_chain::ChangeSet {
                    blocks: [(0, Some(block_hash(0)))].into(),
                },
                ..Default::default()
            },
            ChangeSet {
                tx_graph: tx_graph::ChangeSet {
                    first_seen: [(txid(1), 1_000)].into(),
                    last_seen: [(txid(1), 2_000)].into(),
                    ..Default::default()
                },
                ..Default::default()
            },
        ];

        let mut expected = ChangeSet::default();
        for delta in deltas {
            store.persist_wallet_changeset(&delta).unwrap();
            expected.merge(delta);
        }

        assert_eq!(metadata_entry_count(&store), 1);
        assert_eq!(wallet_entry_count(&store), 1);
        assert_eq!(store.read_wallet_changeset().unwrap(), expected);
    }

    #[test]
    fn tx_graph_timestamps_invariants() {
        let (_directory, store) = temporary_store(Chain::Regtest);
        let txid = txid(42);
        let delta_a = ChangeSet {
            tx_graph: tx_graph::ChangeSet {
                first_seen: [(txid, 1_000)].into(),
                last_seen: [(txid, 2_000)].into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let delta_b = ChangeSet {
            tx_graph: tx_graph::ChangeSet {
                first_seen: [(txid, 900)].into(),
                last_seen: [(txid, 2_100)].into(),
                ..Default::default()
            },
            ..Default::default()
        };

        store.persist_wallet_changeset(&delta_a).unwrap();
        store.persist_wallet_changeset(&delta_b).unwrap();

        let merged = store.read_wallet_changeset().unwrap();
        assert_eq!(merged.tx_graph.first_seen.get(&txid), Some(&900));
        assert_eq!(merged.tx_graph.last_seen.get(&txid), Some(&2_100));
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let settings = test_settings(directory.path(), Chain::Regtest);
        let path = settings.store_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let db = Database::create(&path).unwrap();
        let transaction = db.begin_write().unwrap();
        {
            let mut table = transaction.open_table(METADATA).unwrap();
            let mut bytes = Vec::with_capacity(32);
            ciborium::into_writer(
                &Metadata {
                    schema_version: SCHEMA_VERSION + 1,
                    chain: Chain::Regtest,
                },
                &mut bytes,
            )
            .unwrap();
            table.insert(META, bytes.as_slice()).unwrap();
        }
        transaction.commit().unwrap();
        drop(db);

        let error = match Store::open(settings) {
            Ok(_) => panic!("schema version mismatch should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("schema version mismatch"));
    }

    #[test]
    fn chain_mismatch_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let settings = test_settings(directory.path(), Chain::Signet);
        let path = settings.store_path().unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let db = Database::create(&path).unwrap();
        let transaction = db.begin_write().unwrap();
        {
            let mut table = transaction.open_table(METADATA).unwrap();
            let mut bytes = Vec::with_capacity(32);
            ciborium::into_writer(
                &Metadata {
                    schema_version: SCHEMA_VERSION,
                    chain: Chain::Regtest,
                },
                &mut bytes,
            )
            .unwrap();
            table.insert(META, bytes.as_slice()).unwrap();
        }
        transaction.commit().unwrap();
        drop(db);

        let error = match Store::open(settings) {
            Ok(_) => panic!("chain mismatch should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("chain mismatch"));
    }

    #[test]
    fn reopen_persists_state() {
        let directory = tempfile::tempdir().unwrap();
        let settings = test_settings(directory.path(), Chain::Regtest);
        let changeset = network_changeset(Network::Regtest);

        {
            let store = Store::open(settings.clone()).unwrap();
            store.persist_wallet_changeset(&changeset).unwrap();
        }

        let store = Store::open(settings).unwrap();

        assert_eq!(store.read_wallet_changeset().unwrap(), changeset);
    }
}
