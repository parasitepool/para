use {
    super::*,
    bdk_wallet::{ChangeSet, chain::Merge},
    redb::{
        Database, Durability, ReadableDatabase, ReadableTable, TableDefinition, WriteTransaction,
    },
};

pub(crate) mod entry;

const SCHEMA_VERSION: u64 = 1;

const METADATA_KEY: u32 = 0;
const CHANGESET_KEY: u32 = 0;

const METADATA: TableDefinition<u32, &[u8]> = TableDefinition::new("METADATA");
const ORDERS: TableDefinition<u32, &[u8]> = TableDefinition::new("ORDERS");
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
    pub(crate) fn open(path: &Path, chain: Chain) -> Result<Self> {
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
            .create(path)
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
                .get(METADATA_KEY)?
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
                    metadata.insert(METADATA_KEY, bytes.as_slice())?;
                }
            }

            transaction.open_table(ORDERS)?;
            transaction.open_table(WALLET)?;
        }

        transaction.commit()?;

        Ok(store)
    }

    pub(crate) fn read_wallet_changeset(&self) -> Result<ChangeSet> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(WALLET)?;

        table
            .get(CHANGESET_KEY)?
            .map(|value| ciborium::from_reader(value.value()).context("decode wallet state"))
            .transpose()
            .map(|changeset| changeset.unwrap_or_default())
    }

    pub(crate) fn persist_snapshot(
        &self,
        orders: &[(u32, entry::OrderEntry)],
        wallet_delta: &ChangeSet,
    ) -> Result {
        let mut transaction = self.db.begin_write()?;
        transaction.set_quick_repair(true);
        transaction.set_durability(self.durability)?;

        if !wallet_delta.is_empty() {
            Self::merge_wallet_delta(&transaction, wallet_delta)?;
        }

        {
            let mut table = transaction.open_table(ORDERS)?;
            table.retain(|_, _| false)?;

            for (id, order) in orders {
                let mut bytes = Vec::with_capacity(256);
                ciborium::into_writer(order, &mut bytes)
                    .with_context(|| format!("encode order {id}"))?;
                table.insert(id, bytes.as_slice())?;
            }
        }

        transaction.commit()?;

        Ok(())
    }

    pub(crate) fn persist_issued_order(
        &self,
        id: u32,
        order: &entry::OrderEntry,
        wallet_delta: &ChangeSet,
    ) -> Result {
        let mut transaction = self.db.begin_write()?;
        transaction.set_quick_repair(true);
        transaction.set_durability(self.durability)?;

        if !wallet_delta.is_empty() {
            Self::merge_wallet_delta(&transaction, wallet_delta)?;
        }

        {
            let mut table = transaction.open_table(ORDERS)?;
            let mut bytes = Vec::with_capacity(256);
            ciborium::into_writer(order, &mut bytes)
                .with_context(|| format!("encode order {id}"))?;
            table.insert(id, bytes.as_slice())?;
        }

        transaction.commit()?;

        Ok(())
    }

    pub(crate) fn persist_wallet_delta(&self, wallet_delta: &ChangeSet) -> Result {
        if wallet_delta.is_empty() {
            return Ok(());
        }

        let mut transaction = self.db.begin_write()?;
        transaction.set_quick_repair(true);
        transaction.set_durability(self.durability)?;

        Self::merge_wallet_delta(&transaction, wallet_delta)?;

        transaction.commit()?;

        Ok(())
    }

    fn merge_wallet_delta(transaction: &WriteTransaction, wallet_delta: &ChangeSet) -> Result {
        let mut table = transaction.open_table(WALLET)?;
        let mut merged: ChangeSet = table
            .get(CHANGESET_KEY)?
            .map(|value| ciborium::from_reader(value.value()).context("decode wallet state"))
            .transpose()?
            .unwrap_or_default();

        merged.merge(wallet_delta.clone());

        let mut bytes = Vec::with_capacity(64);
        ciborium::into_writer(&merged, &mut bytes).context("encode wallet state")?;
        table.insert(CHANGESET_KEY, bytes.as_slice())?;

        Ok(())
    }

    pub(crate) fn read_orders(&self) -> Result<Vec<(u32, entry::OrderEntry)>> {
        let transaction = self.db.begin_read()?;
        let table = transaction.open_table(ORDERS)?;
        let mut orders = Vec::new();

        for item in table.iter()? {
            let (id, value) = item?;
            let id = id.value();
            orders.push((
                id,
                ciborium::from_reader(value.value())
                    .with_context(|| format!("decode order {id}"))?,
            ));
        }

        Ok(orders)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::router::order::OrderStatus,
        bdk_wallet::chain::{local_chain, tx_graph},
    };

    fn temporary_store(chain: Chain) -> (tempfile::TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("test.redb"), chain).unwrap();
        store.durability = Durability::None;
        (directory, store)
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
        table.get(CHANGESET_KEY).unwrap().is_some()
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

    fn order_entry_count(store: &Store) -> usize {
        let transaction = store.db.begin_read().unwrap();
        let table = transaction.open_table(ORDERS).unwrap();
        table.iter().unwrap().count()
    }

    fn test_order_entry(status: OrderStatus) -> entry::OrderEntry {
        let now = Instant::now();
        let mut stats = Stats::new();
        stats.record_accepted(Difficulty::from(100.0), Difficulty::from(200.0), now);

        entry::OrderEntry {
            status,
            upstream_target: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.worker@bar:3333"
                .parse()
                .unwrap(),
            bucket: Some(entry::BucketEntry {
                target: HashDays::new(10.0).unwrap(),
                address: "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
                    .parse::<Address<NetworkUnchecked>>()
                    .unwrap(),
                derivation_index: 3,
                amount_sat: 1_000,
                created_at_height: 42,
            }),
            created_at_secs: crate::epoch::instant_to_epoch_secs(now, now),
            stats: stats.to_entry(now),
        }
    }

    #[test]
    fn empty_read_returns_default() {
        let (_directory, store) = temporary_store(Chain::Regtest);

        assert_eq!(store.read_wallet_changeset().unwrap(), ChangeSet::default());
        assert_eq!(store.read_orders().unwrap().len(), 0);
        assert_eq!(order_entry_count(&store), 0);
    }

    #[test]
    fn persist_then_read_round_trips() {
        let (_directory, store) = temporary_store(Chain::Regtest);
        let changeset = network_changeset(Network::Regtest);

        store.persist_snapshot(&[], &changeset).unwrap();

        assert_eq!(store.read_wallet_changeset().unwrap(), changeset);
    }

    #[test]
    fn snapshot_persists_wallet_and_replaces_orders() {
        let (_directory, store) = temporary_store(Chain::Regtest);
        let changeset = network_changeset(Network::Regtest);

        store
            .persist_snapshot(&[(7, test_order_entry(OrderStatus::Active))], &changeset)
            .unwrap();

        assert_eq!(store.read_wallet_changeset().unwrap(), changeset);
        let orders = store.read_orders().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, 7);
        assert_eq!(orders[0].1.status, OrderStatus::Active);

        store
            .persist_snapshot(
                &[(8, test_order_entry(OrderStatus::Fulfilled))],
                &ChangeSet::default(),
            )
            .unwrap();

        let orders = store.read_orders().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, 8);
        assert_eq!(orders[0].1.status, OrderStatus::Fulfilled);
        assert_eq!(order_entry_count(&store), 1);
    }

    #[test]
    fn wallet_delta_does_not_replace_orders() {
        let (_directory, store) = temporary_store(Chain::Regtest);

        store
            .persist_snapshot(
                &[(7, test_order_entry(OrderStatus::Active))],
                &ChangeSet::default(),
            )
            .unwrap();
        store
            .persist_wallet_delta(&network_changeset(Network::Regtest))
            .unwrap();

        let orders = store.read_orders().unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].0, 7);
        assert_eq!(orders[0].1.status, OrderStatus::Active);
    }

    #[test]
    fn issued_order_persists_wallet_and_only_inserts_order() {
        let (_directory, store) = temporary_store(Chain::Regtest);

        store
            .persist_snapshot(
                &[(7, test_order_entry(OrderStatus::Active))],
                &ChangeSet::default(),
            )
            .unwrap();
        store
            .persist_issued_order(
                8,
                &test_order_entry(OrderStatus::Pending),
                &network_changeset(Network::Regtest),
            )
            .unwrap();

        assert_eq!(
            store.read_wallet_changeset().unwrap().network,
            Some(Network::Regtest)
        );

        let orders = store.read_orders().unwrap();
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].0, 7);
        assert_eq!(orders[1].0, 8);
    }

    #[test]
    fn persist_empty_is_noop() {
        let (_directory, store) = temporary_store(Chain::Regtest);

        store.persist_snapshot(&[], &ChangeSet::default()).unwrap();

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
            store.persist_snapshot(&[], &delta).unwrap();
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

        store.persist_snapshot(&[], &delta_a).unwrap();
        store.persist_snapshot(&[], &delta_b).unwrap();

        let merged = store.read_wallet_changeset().unwrap();
        assert_eq!(merged.tx_graph.first_seen.get(&txid), Some(&900));
        assert_eq!(merged.tx_graph.last_seen.get(&txid), Some(&2_100));
    }

    #[test]
    fn schema_version_mismatch_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.redb");

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
            table.insert(METADATA_KEY, bytes.as_slice()).unwrap();
        }
        transaction.commit().unwrap();
        drop(db);

        let error = match Store::open(&path, Chain::Regtest) {
            Ok(_) => panic!("schema version mismatch should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("schema version mismatch"));
    }

    #[test]
    fn chain_mismatch_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.redb");

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
            table.insert(METADATA_KEY, bytes.as_slice()).unwrap();
        }
        transaction.commit().unwrap();
        drop(db);

        let error = match Store::open(&path, Chain::Signet) {
            Ok(_) => panic!("chain mismatch should fail"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("chain mismatch"));
    }

    #[test]
    fn reopen_persists_state() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.redb");
        let changeset = network_changeset(Network::Regtest);

        {
            let store = Store::open(&path, Chain::Regtest).unwrap();
            store.persist_snapshot(&[], &changeset).unwrap();
        }

        let store = Store::open(&path, Chain::Regtest).unwrap();

        assert_eq!(store.read_wallet_changeset().unwrap(), changeset);
    }
}
