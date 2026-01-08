use {super::*, crate::job::Job};

#[derive(Debug)]
pub(crate) struct Jobs<S: Source> {
    latest: Option<Arc<Job<S>>>,
    next_id: JobId,
    seen: LruCache<BlockHash, ()>,
    valid: HashMap<JobId, Arc<Job<S>>>,
}

impl<S: Source> Jobs<S> {
    pub(crate) fn new() -> Self {
        Self {
            next_id: JobId::new(0),
            valid: HashMap::new(),
            latest: None,
            seen: LruCache::new(NonZeroUsize::new(LRU_CACHE_SIZE).expect("should be non-zero")),
        }
    }

    pub(crate) fn next_id(&mut self) -> JobId {
        let id = self.next_id;
        self.next_id = self.next_id.next();
        id
    }

    pub(crate) fn get(&self, id: &JobId) -> Option<Arc<Job<S>>> {
        self.valid.get(id).cloned()
    }

    pub(crate) fn latest_workbase(&self) -> Option<&Arc<S>> {
        self.latest.as_ref().map(|job| &job.workbase)
    }

    pub(crate) fn insert_with_clean(&mut self, job: Arc<Job<S>>, clean_jobs: bool) {
        if clean_jobs {
            self.insert_and_clean(job);
        } else {
            self.insert(job);
        }
    }

    pub(crate) fn is_duplicate(&mut self, block_hash: BlockHash) -> bool {
        self.seen.put(block_hash, ()).is_some()
    }

    fn insert(&mut self, job: Arc<Job<S>>) {
        self.latest = Some(job.clone());
        self.valid.insert(job.job_id, job);
    }

    fn insert_and_clean(&mut self, job: Arc<Job<S>>) {
        self.latest = Some(job.clone());
        self.seen.clear();
        self.valid.clear();
        self.valid.insert(job.job_id, job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type PoolWorkbase = Workbase<BlockTemplate>;

    fn create_template(block_height: u64) -> Arc<BlockTemplate> {
        let template = BlockTemplate {
            height: block_height,
            ..Default::default()
        };

        Arc::new(template)
    }

    fn create_job(id: JobId, template: Arc<BlockTemplate>) -> Arc<Job<PoolWorkbase>> {
        let address = Address::from_str("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc")
            .unwrap()
            .assume_checked();

        let workbase = Arc::new(Workbase::<BlockTemplate>::new((*template).clone()));
        let enonce1 = Extranonce::random(ENONCE1_SIZE);

        Arc::new(workbase.create_job(&enonce1, 8, Some(&address), id, None))
    }

    #[track_caller]
    fn assert_invariants(jobs: &Jobs<PoolWorkbase>) {
        assert_eq!(
            jobs.latest.is_some(),
            !jobs.valid.is_empty(),
            "latest/valid mismatch"
        );

        if let Some(latest) = &jobs.latest {
            let current_height = latest.workbase.template().height;

            let heights = jobs
                .valid
                .values()
                .map(|job| job.workbase.template().height)
                .collect::<HashSet<u64>>();

            assert_eq!(heights.len(), 1, "all jobs should be same height");
            assert!(heights.contains(&current_height));
            assert!(jobs.valid.contains_key(&latest.job_id));
        }
    }

    #[test]
    fn next_id_monotonic_and_wraps() {
        let mut jobs: Jobs<PoolWorkbase> = Jobs::new();
        let a = jobs.next_id();
        let b = jobs.next_id();
        assert_ne!(a, b);

        jobs.next_id = JobId::new(u64::MAX - 1);
        assert_eq!(jobs.next_id(), JobId::new(u64::MAX - 1));
        assert_eq!(jobs.next_id(), JobId::new(u64::MAX));
        assert_eq!(jobs.next_id(), JobId::new(0));
    }

    #[test]
    fn insert_same_height_does_not_clean() {
        let mut jobs: Jobs<PoolWorkbase> = Jobs::new();

        let id_1 = jobs.next_id();
        let job_1 = create_job(id_1, create_template(100));

        let workbase_1 = job_1.workbase.clone();
        let clean_jobs = workbase_1.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_1.clone(), clean_jobs);
        assert!(clean_jobs);
        assert_invariants(&jobs);

        let id_2 = jobs.next_id();
        let job_2 = create_job(id_2, create_template(100));

        let workbase_2 = job_2.workbase.clone();
        let clean_jobs = workbase_2.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_2.clone(), clean_jobs);
        assert!(!clean_jobs);
        assert_invariants(&jobs);

        assert_eq!(jobs.latest.as_ref().unwrap().job_id, id_2);
        assert!(jobs.valid.contains_key(&id_1));
        assert!(jobs.valid.contains_key(&id_2));
        assert!(
            jobs.valid
                .values()
                .all(|job| job.workbase.template().height == 100)
        );
    }

    #[test]
    fn insert_new_height_cleans_and_clears_seen() {
        let mut jobs: Jobs<PoolWorkbase> = Jobs::new();

        let id_1 = jobs.next_id();
        let job_1 = create_job(id_1, create_template(100));
        let workbase_1 = job_1.workbase.clone();
        let clean_jobs = workbase_1.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_1.clone(), clean_jobs);
        assert!(clean_jobs);

        let blockhash = BlockHash::from_byte_array([7u8; 32]);
        assert!(!jobs.is_duplicate(blockhash));
        assert!(jobs.is_duplicate(blockhash));

        let id_2 = jobs.next_id();
        let job_2 = create_job(id_2, create_template(101));
        let workbase_2 = job_2.workbase.clone();
        let clean_jobs = workbase_2.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_2.clone(), clean_jobs);
        assert!(clean_jobs);

        assert_invariants(&jobs);
        assert!(!jobs.valid.contains_key(&id_1));
        assert!(jobs.valid.contains_key(&id_2));
        assert_eq!(jobs.latest.as_ref().unwrap().job_id, id_2);
        assert_eq!(
            jobs.latest.as_ref().unwrap().workbase.template().height,
            101
        );

        assert!(!jobs.is_duplicate(blockhash));
        assert!(jobs.is_duplicate(blockhash));
    }

    #[test]
    fn duplicate_lru() {
        let mut jobs: Jobs<PoolWorkbase> = Jobs::new();
        let h1 = BlockHash::from_byte_array([1u8; 32]);
        let h2 = BlockHash::from_byte_array([2u8; 32]);

        assert!(!jobs.is_duplicate(h1));
        assert!(jobs.is_duplicate(h1));

        assert!(!jobs.is_duplicate(h2));
        assert!(jobs.is_duplicate(h2));
    }
}
