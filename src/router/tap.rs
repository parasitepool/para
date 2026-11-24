use {super::*, epoch};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TapStatus {
    Active,
    Cancelled,
}

struct TapState {
    status: TapStatus,
    processed_outpoints: BTreeMap<OutPoint, u32>,
}

pub struct Tap {
    pub(crate) id: u32,
    pub(crate) upstream_target: UpstreamTarget,
    pub(crate) address: Address,
    pub(crate) derivation_index: u32,
    pub(crate) created_at: Instant,
    pub(crate) created_at_height: u32,
    state: Mutex<TapState>,
    persist_lock: Mutex<()>,
}

impl Tap {
    pub(crate) fn new(
        id: u32,
        upstream_target: UpstreamTarget,
        address: Address,
        derivation_index: u32,
        created_at_height: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            id,
            upstream_target,
            address,
            derivation_index,
            created_at: Instant::now(),
            created_at_height,
            state: Mutex::new(TapState {
                status: TapStatus::Active,
                processed_outpoints: BTreeMap::new(),
            }),
            persist_lock: Mutex::new(()),
        })
    }

    fn build_entry(
        &self,
        status: TapStatus,
        processed_outpoints: BTreeMap<OutPoint, u32>,
    ) -> entry::TapEntry {
        let now = Instant::now();

        entry::TapEntry {
            status,
            upstream_target: self.upstream_target.clone(),
            address: self.address.as_unchecked().clone(),
            derivation_index: self.derivation_index,
            created_at_secs: epoch::instant_to_epoch_secs(self.created_at, now),
            created_at_height: self.created_at_height,
            processed_outpoints,
        }
    }

    pub(crate) fn to_entry(&self) -> entry::TapEntry {
        let state = self.state.lock();
        self.build_entry(state.status, state.processed_outpoints.clone())
    }

    pub(crate) fn restore(
        id: u32,
        tap_entry: entry::TapEntry,
        network: Network,
    ) -> Result<Arc<Self>> {
        let address = tap_entry
            .address
            .require_network(network)
            .with_context(|| format!("restore tap {id} payment address"))?;

        Ok(Arc::new(Self {
            id,
            upstream_target: tap_entry.upstream_target,
            address,
            derivation_index: tap_entry.derivation_index,
            created_at: epoch::epoch_secs_to_instant(tap_entry.created_at_secs),
            created_at_height: tap_entry.created_at_height,
            state: Mutex::new(TapState {
                status: tap_entry.status,
                processed_outpoints: tap_entry.processed_outpoints,
            }),
            persist_lock: Mutex::new(()),
        }))
    }

    pub(crate) fn status(&self) -> TapStatus {
        self.state.lock().status
    }

    pub(crate) fn is_processed(&self, outpoint: OutPoint) -> bool {
        self.state
            .lock()
            .processed_outpoints
            .contains_key(&outpoint)
    }

    pub(crate) fn order_count(&self) -> usize {
        self.state.lock().processed_outpoints.len()
    }

    pub(crate) fn order_ids(&self) -> Vec<u32> {
        let mut ids = self
            .state
            .lock()
            .processed_outpoints
            .values()
            .copied()
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    pub(crate) fn cancel_with(
        &self,
        persist: impl FnOnce(&entry::TapEntry) -> Result,
    ) -> RouterResult<()> {
        let _guard = self.persist_lock.lock();

        let entry = {
            let state = self.state.lock();

            if state.status == TapStatus::Cancelled {
                return Ok(());
            }

            self.build_entry(TapStatus::Cancelled, state.processed_outpoints.clone())
        };

        persist(&entry).map_err(|error| RouterError::WalletPersistence { error })?;

        self.state.lock().status = TapStatus::Cancelled;

        info!("Tap {} at {} cancelled", self.id, self.upstream_target);

        Ok(())
    }

    pub(crate) fn process_outpoint_with<T>(
        &self,
        outpoint: OutPoint,
        allocate_order_id: impl FnOnce() -> RouterResult<u32>,
        persist: impl FnOnce(u32, TapStatus, &entry::TapEntry) -> Result<T>,
    ) -> RouterResult<Option<T>> {
        let _guard = self.persist_lock.lock();

        let (status, mut processed) = {
            let state = self.state.lock();

            if state.processed_outpoints.contains_key(&outpoint) {
                return Ok(None);
            }

            (state.status, state.processed_outpoints.clone())
        };

        let order_id = allocate_order_id()?;
        processed.insert(outpoint, order_id);
        let entry = self.build_entry(status, processed);

        let value = persist(order_id, status, &entry)
            .map_err(|error| RouterError::WalletPersistence { error })?;

        self.state
            .lock()
            .processed_outpoints
            .insert(outpoint, order_id);

        Ok(Some(value))
    }
}

struct Taps {
    by_id: BTreeMap<u32, Arc<Tap>>,
    by_index: HashMap<u32, u32>,
    next_id: u32,
}

impl Taps {
    fn new() -> Self {
        Self {
            by_id: BTreeMap::new(),
            by_index: HashMap::new(),
            next_id: 0,
        }
    }

    fn insert(&mut self, tap: Arc<Tap>) {
        self.by_index.insert(tap.derivation_index, tap.id);
        self.by_id.insert(tap.id, tap);
    }
}

pub(crate) struct TapBook {
    taps: RwLock<Taps>,
}

impl TapBook {
    pub(crate) fn new() -> Self {
        Self {
            taps: RwLock::new(Taps::new()),
        }
    }

    pub(crate) fn add_tap(
        &self,
        make_tap: impl FnOnce(u32) -> RouterResult<Arc<Tap>>,
    ) -> RouterResult<Arc<Tap>> {
        let mut taps = self.taps.write();

        let id = taps.next_id;
        let next_id = id.checked_add(1).ok_or(RouterError::TapIdExhausted)?;

        let tap = make_tap(id)?;
        taps.next_id = next_id;
        taps.insert(tap.clone());

        Ok(tap)
    }

    pub(crate) fn get(&self, id: u32) -> Option<Arc<Tap>> {
        self.taps.read().by_id.get(&id).cloned()
    }

    pub(crate) fn by_derivation_index(&self, index: u32) -> Option<Arc<Tap>> {
        let taps = self.taps.read();
        taps.by_index
            .get(&index)
            .and_then(|id| taps.by_id.get(id))
            .cloned()
    }

    pub(crate) fn all(&self) -> Vec<Arc<Tap>> {
        self.taps.read().by_id.values().cloned().collect()
    }

    pub(crate) fn tap_page(
        &self,
        cursor: Option<u32>,
        limit: usize,
        filter: impl Fn(&Tap) -> bool,
    ) -> Vec<Arc<Tap>> {
        self.taps
            .read()
            .by_id
            .iter()
            .rev()
            .skip_while(|(id, _)| cursor.is_some_and(|cursor| **id >= cursor))
            .filter(|(_, tap)| filter(tap))
            .take(limit)
            .map(|(_, tap)| tap.clone())
            .collect()
    }

    pub(crate) fn count_taps(&self, filter: impl Fn(&Tap) -> bool) -> u64 {
        self.taps
            .read()
            .by_id
            .values()
            .filter(|tap| filter(tap))
            .count() as u64
    }

    pub(crate) fn restore(&self, store: &Store, network: Network) -> Result {
        let entries = store.read_taps()?;
        let mut next_id = 0u32;
        let mut taps = self.taps.write();

        for (id, entry) in entries {
            let candidate = id
                .checked_add(1)
                .with_context(|| format!("persisted tap id {id} exhausts u32 tap ids"))?;

            next_id = next_id.max(candidate);

            let tap = Tap::restore(id, entry, network)?;
            taps.insert(tap);
        }

        taps.next_id = next_id;

        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_next_id(&self, next_id: u32) {
        self.taps.write().next_id = next_id;
    }

    #[cfg(test)]
    pub(crate) fn next_id(&self) -> u32 {
        self.taps.read().next_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_address() -> Address {
        "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc"
            .parse::<Address<NetworkUnchecked>>()
            .unwrap()
            .assume_checked()
    }

    fn test_tap(id: u32) -> Arc<Tap> {
        Tap::new(
            id,
            "tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.foo@bar:3333"
                .parse()
                .unwrap(),
            test_address(),
            id,
            10,
        )
    }

    fn outpoint(byte: u8, vout: u32) -> OutPoint {
        OutPoint {
            txid: Txid::from_byte_array([byte; 32]),
            vout,
        }
    }

    #[test]
    fn status_serde() {
        #[track_caller]
        fn case(status: TapStatus, expected: &str) {
            assert_eq!(serde_json::to_string(&status).unwrap(), expected);
            assert_eq!(serde_json::from_str::<TapStatus>(expected).unwrap(), status);
        }

        case(TapStatus::Active, "\"active\"");
        case(TapStatus::Cancelled, "\"cancelled\"");
    }

    #[test]
    fn entry_round_trip() {
        let tap = test_tap(7);
        let op = outpoint(1, 0);

        tap.process_outpoint_with(op, || Ok(42), |order_id, status, _| Ok((order_id, status)))
            .unwrap();
        tap.cancel_with(|_| Ok(())).unwrap();

        let restored = Tap::restore(7, tap.to_entry(), Network::Testnet).unwrap();
        assert_eq!(restored.id, 7);
        assert_eq!(restored.status(), TapStatus::Cancelled);
        assert_eq!(restored.derivation_index, 7);
        assert_eq!(restored.created_at_height, 10);
        assert!(restored.is_processed(op));
        assert_eq!(restored.order_ids(), vec![42]);
    }

    #[test]
    fn failed_persist_leaves_memory_unchanged() {
        let tap = test_tap(0);
        let op = outpoint(1, 0);

        assert!(
            tap.process_outpoint_with::<()>(op, || Ok(5), |_, _, _| bail!("foo"))
                .is_err()
        );
        assert!(!tap.is_processed(op));
        assert!(tap.order_ids().is_empty());

        assert!(tap.cancel_with(|_| bail!("bar")).is_err());
        assert_eq!(tap.status(), TapStatus::Active);
    }

    #[test]
    fn duplicate_outpoint_is_skipped() {
        let tap = test_tap(0);
        let op = outpoint(1, 0);

        let first = tap
            .process_outpoint_with(op, || Ok(1), |order_id, _, _| Ok(order_id))
            .unwrap();
        assert_eq!(first, Some(1));

        let second = tap
            .process_outpoint_with(op, || panic!("should not allocate"), |_, _, _| Ok(2))
            .unwrap();
        assert_eq!(second, None);
    }

    #[test]
    fn tap_id_exhaustion() {
        let book = TapBook::new();
        book.set_next_id(u32::MAX);

        assert!(matches!(
            book.add_tap(|id| Ok(test_tap(id))),
            Err(RouterError::TapIdExhausted),
        ));
        assert_eq!(book.next_id(), u32::MAX);
    }

    #[test]
    fn failed_add_tap_does_not_burn_tap_id() {
        let book = TapBook::new();

        assert!(matches!(
            book.add_tap(|_| Err(RouterError::WalletSyncing)),
            Err(RouterError::WalletSyncing),
        ));
        assert_eq!(book.next_id(), 0);

        let tap = book.add_tap(|id| Ok(test_tap(id))).unwrap();
        assert_eq!(tap.id, 0);
        assert_eq!(book.next_id(), 1);
    }
}
