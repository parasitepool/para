use {super::*, crate::job::Job};

#[derive(Debug)]
pub struct Jobs {
    next: u64,
    valid: HashMap<JobId, Arc<Job>>,
    latest: Option<Arc<Job>>,
    seen: LruCache<BlockHash, ()>,
}

impl Jobs {
    pub fn new() -> Self {
        Self {
            next: 0,
            valid: HashMap::new(),
            latest: None,
            seen: LruCache::new(NonZeroUsize::new(LRU_CACHE_SIZE).expect("should be non-zero")),
        }
    }

    pub fn next_id(&mut self) -> JobId {
        let id = JobId::from(self.next);
        self.next = self.next.wrapping_add(1);
        id
    }

    pub fn insert(&mut self, job: Arc<Job>) {
        self.latest = Some(job.clone());
        self.valid.insert(job.job_id, job);
    }

    pub fn insert_and_clean(&mut self, job: Arc<Job>) {
        self.valid.clear();
        self.valid.insert(job.job_id, job.clone());
        self.latest = Some(job);
        self.seen.clear();
    }

    pub fn get(&self, id: &JobId) -> Option<&Arc<Job>> {
        self.valid.get(id)
    }

    pub fn latest(&self) -> Option<&Arc<Job>> {
        self.latest.as_ref()
    }

    pub fn is_duplicate(&mut self, block_hash: BlockHash) -> bool {
        self.seen.put(block_hash, ()).is_some()
    }
}
