use {super::*, crate::store::entry::UserEntry};

pub(crate) struct User {
    pub(crate) address: Address,
    pub(crate) workers: DashMap<String, Arc<Worker>>,
    pub(crate) authorized: u64,
}

impl User {
    pub(crate) fn new(address: Address) -> Self {
        Self {
            address,
            workers: DashMap::new(),
            authorized: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time went backwards")
                .as_secs(),
        }
    }

    pub(super) fn new_session(&self, workername: &str, session: Arc<Session>) {
        self.workers
            .entry(workername.to_string())
            .or_insert_with(|| Arc::new(Worker::new(workername.to_string())))
            .new_session(session);
    }

    pub(crate) fn session_count(&self) -> usize {
        self.workers
            .iter()
            .map(|worker| worker.session_count())
            .sum()
    }

    pub(crate) fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub(crate) fn snapshot(&self) -> Stats {
        let now = Instant::now();

        self.workers
            .iter()
            .fold(Stats::new(), |mut combined, worker| {
                combined.absorb(worker.snapshot(), now);
                combined
            })
    }

    pub(crate) fn sessions(&self) -> Vec<Arc<Session>> {
        self.workers
            .iter()
            .flat_map(|worker| worker.sessions().collect::<Vec<_>>())
            .collect()
    }

    pub(crate) fn workers(&self) -> impl Iterator<Item = Arc<Worker>> {
        self.workers.iter().map(|entry| entry.value().clone())
    }

    pub(crate) fn from_entry(address: Address, entry: UserEntry) -> Result<Self> {
        let workers = entry
            .workers
            .into_iter()
            .map(|worker| {
                Worker::from_entry(worker)
                    .map(|worker| (worker.workername().to_string(), Arc::new(worker)))
            })
            .collect::<Result<_>>()?;

        Ok(Self {
            address,
            workers,
            authorized: entry.authorized_secs,
        })
    }

    pub(crate) fn to_entry(&self, now: Instant) -> UserEntry {
        UserEntry {
            authorized_secs: self.authorized,
            workers: self
                .workers
                .iter()
                .map(|worker| worker.to_entry(now))
                .collect(),
        }
    }
}

impl From<Address> for User {
    fn from(address: Address) -> Self {
        Self::new(address)
    }
}
