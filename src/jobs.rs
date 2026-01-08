use {super::*, crate::job::Job};

#[derive(Debug)]
pub(crate) struct Jobs<W: Workbase> {
    latest: Option<Arc<Job<W>>>,
    next_id: JobId,
    seen: LruCache<BlockHash, ()>,
    valid: HashMap<JobId, Arc<Job<W>>>,
}

impl<W: Workbase> Jobs<W> {
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

    pub(crate) fn get(&self, id: &JobId) -> Option<Arc<Job<W>>> {
        self.valid.get(id).cloned()
    }

    pub(crate) fn latest_workbase(&self) -> Option<&Arc<W>> {
        self.latest.as_ref().map(|job| &job.workbase)
    }

    pub(crate) fn insert_with_clean(&mut self, job: Arc<Job<W>>, clean_jobs: bool) {
        if clean_jobs {
            self.insert_and_clean(job);
        } else {
            self.insert(job);
        }
    }

    pub(crate) fn is_duplicate(&mut self, block_hash: BlockHash) -> bool {
        self.seen.put(block_hash, ()).is_some()
    }

    fn insert(&mut self, job: Arc<Job<W>>) {
        self.latest = Some(job.clone());
        self.valid.insert(job.job_id, job);
    }

    fn insert_and_clean(&mut self, job: Arc<Job<W>>) {
        self.latest = Some(job.clone());
        self.seen.clear();
        self.valid.clear();
        self.valid.insert(job.job_id, job);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::block;

    trait TestWorkbaseFactory: Workbase + Sized {
        fn workbase_that_cleans(seq: u64) -> Arc<Self>;
        fn workbase_same_group(seq: u64) -> Arc<Self>;
        fn test_address() -> Option<Address>;

        fn create_test_job(workbase: &Arc<Self>, job_id: JobId) -> Arc<Job<Self>> {
            let enonce1 = Extranonce::random(ENONCE1_SIZE);
            Arc::new(workbase.create_job(&enonce1, 8, Self::test_address().as_ref(), job_id, None))
        }
    }

    impl TestWorkbaseFactory for BlockTemplate {
        fn workbase_that_cleans(seq: u64) -> Arc<Self> {
            Arc::new(BlockTemplate {
                height: seq,
                ..Default::default()
            })
        }

        fn workbase_same_group(seq: u64) -> Arc<Self> {
            Arc::new(BlockTemplate {
                height: seq,
                ..Default::default()
            })
        }

        fn test_address() -> Option<Address> {
            Some(
                Address::from_str("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc")
                    .unwrap()
                    .assume_checked(),
            )
        }
    }

    impl TestWorkbaseFactory for Notify {
        fn workbase_that_cleans(_seq: u64) -> Arc<Self> {
            Arc::new(sample_notify(true))
        }

        fn workbase_same_group(_seq: u64) -> Arc<Self> {
            Arc::new(sample_notify(false))
        }

        fn test_address() -> Option<Address> {
            None
        }
    }

    fn sample_notify(clean_jobs: bool) -> Notify {
        Notify {
            job_id: "bf".parse().unwrap(),
            prevhash: "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000"
                .parse()
                .unwrap(),
            coinb1: "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff20020862062f503253482f04b8864e5008".into(),
            coinb2: "072f736c7573682f000000000100f2052a010000001976a914d23fcdf86f7e756a64a7a9688ef9903327048ed988ac00000000".into(),
            merkle_branches: Vec::new(),
            version: Version(block::Version::TWO),
            nbits: "1c2ac4af".parse().unwrap(),
            ntime: "504e86b9".parse().unwrap(),
            clean_jobs,
        }
    }

    #[track_caller]
    fn assert_invariants<W: Workbase>(jobs: &Jobs<W>) {
        assert_eq!(
            jobs.latest.is_some(),
            !jobs.valid.is_empty(),
            "latest/valid mismatch"
        );

        if let Some(latest) = &jobs.latest {
            assert!(jobs.valid.contains_key(&latest.job_id));
        }
    }

    fn check_next_id_monotonic_and_wraps<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();
        let a = jobs.next_id();
        let b = jobs.next_id();
        assert_ne!(a, b);

        jobs.next_id = JobId::new(u64::MAX - 1);
        assert_eq!(jobs.next_id(), JobId::new(u64::MAX - 1));
        assert_eq!(jobs.next_id(), JobId::new(u64::MAX));
        assert_eq!(jobs.next_id(), JobId::new(0));
    }

    fn check_insert_same_group_does_not_clean<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let workbase_1 = W::workbase_that_cleans(100);
        let id_1 = jobs.next_id();
        let job_1 = W::create_test_job(&workbase_1, id_1);

        let clean_jobs = workbase_1.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_1.clone(), clean_jobs);
        assert!(clean_jobs, "first insert should clean");
        assert_invariants(&jobs);

        let workbase_2 = W::workbase_same_group(100);
        let id_2 = jobs.next_id();
        let job_2 = W::create_test_job(&workbase_2, id_2);

        let clean_jobs = workbase_2.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_2.clone(), clean_jobs);
        assert!(!clean_jobs, "same group should not clean");
        assert_invariants(&jobs);

        assert_eq!(jobs.latest.as_ref().unwrap().job_id, id_2);
        assert!(jobs.valid.contains_key(&id_1));
        assert!(jobs.valid.contains_key(&id_2));
        assert_eq!(jobs.valid.len(), 2);
    }

    fn check_insert_new_work_cleans_and_clears_seen<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let workbase_1 = W::workbase_that_cleans(100);
        let id_1 = jobs.next_id();
        let job_1 = W::create_test_job(&workbase_1, id_1);

        let clean_jobs = workbase_1.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_1.clone(), clean_jobs);
        assert!(clean_jobs);

        let blockhash = BlockHash::from_byte_array([7u8; 32]);
        assert!(!jobs.is_duplicate(blockhash));
        assert!(jobs.is_duplicate(blockhash));

        let workbase_2 = W::workbase_that_cleans(101);
        let id_2 = jobs.next_id();
        let job_2 = W::create_test_job(&workbase_2, id_2);

        let clean_jobs = workbase_2.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job_2.clone(), clean_jobs);
        assert!(clean_jobs, "new work should clean");

        assert_invariants(&jobs);
        assert!(!jobs.valid.contains_key(&id_1), "old job should be cleaned");
        assert!(jobs.valid.contains_key(&id_2));
        assert_eq!(jobs.latest.as_ref().unwrap().job_id, id_2);
        assert_eq!(jobs.valid.len(), 1);

        assert!(
            !jobs.is_duplicate(blockhash),
            "seen should be cleared on clean"
        );
        assert!(jobs.is_duplicate(blockhash));
    }

    fn check_duplicate_lru<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();
        let h1 = BlockHash::from_byte_array([1u8; 32]);
        let h2 = BlockHash::from_byte_array([2u8; 32]);

        assert!(!jobs.is_duplicate(h1));
        assert!(jobs.is_duplicate(h1));

        assert!(!jobs.is_duplicate(h2));
        assert!(jobs.is_duplicate(h2));
    }

    fn check_get_returns_valid_job<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let workbase = W::workbase_that_cleans(100);
        let id = jobs.next_id();
        let job = W::create_test_job(&workbase, id);

        let clean_jobs = workbase.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job.clone(), clean_jobs);

        assert!(jobs.get(&id).is_some());
        assert!(jobs.get(&JobId::new(999)).is_none());
    }

    fn check_latest_workbase<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        assert!(jobs.latest_workbase().is_none());

        let workbase = W::workbase_that_cleans(100);
        let id = jobs.next_id();
        let job = W::create_test_job(&workbase, id);

        let clean_jobs = workbase.clean_jobs(jobs.latest_workbase().map(|w| w.as_ref()));
        jobs.insert_with_clean(job, clean_jobs);

        assert!(jobs.latest_workbase().is_some());
    }

    fn check_create_job_assigns_fields<W: TestWorkbaseFactory>() {
        let workbase = W::workbase_that_cleans(100);
        let enonce1 = Extranonce::random(4);
        let job_id = JobId::new(42);
        let version_mask = Some(Version::from_str("1fffe000").unwrap());

        let job = workbase.create_job(
            &enonce1,
            8,
            W::test_address().as_ref(),
            job_id,
            version_mask,
        );

        assert_eq!(job.job_id, job_id);
        assert_eq!(job.enonce1, enonce1);
        assert_eq!(job.version_mask, version_mask);
        assert!(Arc::ptr_eq(&job.workbase, &workbase));
    }

    fn check_clean_jobs_returns_true_for_new_work<W: TestWorkbaseFactory>() {
        let workbase1 = W::workbase_that_cleans(100);
        let workbase2 = W::workbase_that_cleans(101);

        assert!(workbase1.clean_jobs(None));
        assert!(workbase2.clean_jobs(Some(workbase1.as_ref())));
    }

    fn check_clean_jobs_returns_false_for_same_group<W: TestWorkbaseFactory>() {
        let workbase1 = W::workbase_that_cleans(100);
        let workbase2 = W::workbase_same_group(100);

        assert!(workbase1.clean_jobs(None));
        assert!(!workbase2.clean_jobs(Some(workbase1.as_ref())));
    }

    fn check_job_notify_roundtrip<W: TestWorkbaseFactory>() {
        let workbase = W::workbase_that_cleans(100);
        let job = W::create_test_job(&workbase, JobId::new(1));

        let notify = job.notify(true).unwrap();

        assert_eq!(notify.job_id, job.job_id);
        assert_eq!(notify.prevhash, job.prevhash());
        assert_eq!(notify.coinb1, job.coinb1);
        assert_eq!(notify.coinb2, job.coinb2);
        assert_eq!(notify.merkle_branches, job.merkle_branches());
        assert_eq!(notify.version, job.version());
        assert_eq!(notify.nbits, job.nbits());
        assert_eq!(notify.ntime, job.ntime());
        assert!(notify.clean_jobs);
    }

    #[test]
    fn next_id_monotonic_and_wraps() {
        check_next_id_monotonic_and_wraps::<BlockTemplate>();
        check_next_id_monotonic_and_wraps::<Notify>();
    }

    #[test]
    fn insert_same_group_does_not_clean() {
        check_insert_same_group_does_not_clean::<BlockTemplate>();
        check_insert_same_group_does_not_clean::<Notify>();
    }

    #[test]
    fn insert_new_work_cleans_and_clears_seen() {
        check_insert_new_work_cleans_and_clears_seen::<BlockTemplate>();
        check_insert_new_work_cleans_and_clears_seen::<Notify>();
    }

    #[test]
    fn duplicate_lru() {
        check_duplicate_lru::<BlockTemplate>();
        check_duplicate_lru::<Notify>();
    }

    #[test]
    fn get_returns_valid_job() {
        check_get_returns_valid_job::<BlockTemplate>();
        check_get_returns_valid_job::<Notify>();
    }

    #[test]
    fn latest_workbase() {
        check_latest_workbase::<BlockTemplate>();
        check_latest_workbase::<Notify>();
    }

    #[test]
    fn create_job_assigns_fields() {
        check_create_job_assigns_fields::<BlockTemplate>();
        check_create_job_assigns_fields::<Notify>();
    }

    #[test]
    fn clean_jobs_returns_true_for_new_work() {
        check_clean_jobs_returns_true_for_new_work::<BlockTemplate>();
        check_clean_jobs_returns_true_for_new_work::<Notify>();
    }

    #[test]
    fn clean_jobs_returns_false_for_same_group() {
        check_clean_jobs_returns_false_for_same_group::<BlockTemplate>();
        check_clean_jobs_returns_false_for_same_group::<Notify>();
    }

    #[test]
    fn job_notify_roundtrip() {
        check_job_notify_roundtrip::<BlockTemplate>();
        check_job_notify_roundtrip::<Notify>();
    }
}
