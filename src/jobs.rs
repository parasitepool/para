use super::*;

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

    pub(crate) fn insert(&mut self, job: Arc<Job<W>>) -> bool {
        let prev = self.latest.as_ref().map(|j| j.workbase.as_ref());
        let clean = job.workbase.clean_jobs(prev);

        self.latest = Some(job.clone());

        if clean {
            self.seen.clear();
            self.valid.clear();
        }

        self.valid.insert(job.job_id, job);
        clean
    }

    pub(crate) fn is_duplicate(&mut self, block_hash: BlockHash) -> bool {
        self.seen.put(block_hash, ()).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::block;

    trait TestWorkbaseFactory: Workbase + Sized {
        fn workbase_that_cleans(seq: u64, job_id: JobId) -> Arc<Self>;
        fn workbase_same_group(seq: u64, job_id: JobId) -> Arc<Self>;
        fn test_address() -> Option<Address>;

        fn create_test_job(workbase: &Arc<Self>, job_id: JobId) -> Arc<Job<Self>> {
            let enonce1 = Extranonce::random(ENONCE1_SIZE);
            Arc::new(
                workbase
                    .create_job(&enonce1, 8, Self::test_address().as_ref(), job_id, None)
                    .unwrap(),
            )
        }
    }

    impl TestWorkbaseFactory for BlockTemplate {
        fn workbase_that_cleans(seq: u64, _job_id: JobId) -> Arc<Self> {
            Arc::new(BlockTemplate {
                height: seq,
                ..Default::default()
            })
        }

        fn workbase_same_group(seq: u64, _job_id: JobId) -> Arc<Self> {
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
        fn workbase_that_cleans(_seq: u64, job_id: JobId) -> Arc<Self> {
            Arc::new(sample_notify(true, job_id))
        }

        fn workbase_same_group(_seq: u64, job_id: JobId) -> Arc<Self> {
            Arc::new(sample_notify(false, job_id))
        }

        fn test_address() -> Option<Address> {
            None
        }
    }

    fn sample_notify(clean_jobs: bool, job_id: JobId) -> Notify {
        Notify {
            job_id,
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

        let id_1 = jobs.next_id();
        let workbase_1 = W::workbase_that_cleans(100, id_1);
        let job_1 = W::create_test_job(&workbase_1, id_1);

        let clean_jobs = jobs.insert(job_1.clone());
        assert!(clean_jobs, "first insert should clean");
        assert_invariants(&jobs);

        let id_2 = jobs.next_id();
        let workbase_2 = W::workbase_same_group(100, id_2);
        let job_2 = W::create_test_job(&workbase_2, id_2);

        let clean_jobs = jobs.insert(job_2.clone());
        assert!(!clean_jobs, "same group should not clean");
        assert_invariants(&jobs);

        assert_eq!(jobs.latest.as_ref().unwrap().job_id, id_2);
        assert!(jobs.valid.contains_key(&id_1));
        assert!(jobs.valid.contains_key(&id_2));
        assert_eq!(jobs.valid.len(), 2);
    }

    fn check_insert_new_work_cleans_and_clears_seen<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let id_1 = jobs.next_id();
        let workbase_1 = W::workbase_that_cleans(100, id_1);
        let job_1 = W::create_test_job(&workbase_1, id_1);

        let clean_jobs = jobs.insert(job_1.clone());
        assert!(clean_jobs);

        let blockhash = BlockHash::from_byte_array([7u8; 32]);
        assert!(!jobs.is_duplicate(blockhash));
        assert!(jobs.is_duplicate(blockhash));

        let id_2 = jobs.next_id();
        let workbase_2 = W::workbase_that_cleans(101, id_2);
        let job_2 = W::create_test_job(&workbase_2, id_2);

        let clean_jobs = jobs.insert(job_2.clone());
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

        let id = jobs.next_id();
        let workbase = W::workbase_that_cleans(100, id);
        let job = W::create_test_job(&workbase, id);

        jobs.insert(job.clone());

        assert!(jobs.get(&id).is_some());
        assert!(jobs.get(&JobId::new(999)).is_none());
    }

    fn check_insert_returns_clean_jobs<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let id = jobs.next_id();
        let workbase = W::workbase_that_cleans(100, id);
        let job = W::create_test_job(&workbase, id);

        let clean = jobs.insert(job);
        assert!(clean, "first insert should return true for clean_jobs");

        let id2 = jobs.next_id();
        let workbase2 = W::workbase_same_group(100, id2);
        let job2 = W::create_test_job(&workbase2, id2);

        let clean = jobs.insert(job2);
        assert!(
            !clean,
            "same group insert should return false for clean_jobs"
        );
    }

    fn check_create_job_assigns_fields<W: TestWorkbaseFactory>() {
        let enonce1 = Extranonce::random(4);
        let job_id = JobId::new(42);
        let workbase = W::workbase_that_cleans(100, job_id);
        let version_mask = Some(Version::from_str("1fffe000").unwrap());

        let job = workbase
            .create_job(
                &enonce1,
                8,
                W::test_address().as_ref(),
                job_id,
                version_mask,
            )
            .unwrap();

        assert_eq!(job.job_id, job_id);
        assert_eq!(job.enonce1, enonce1);
        assert_eq!(job.version_mask, version_mask);
        assert!(Arc::ptr_eq(&job.workbase, &workbase));
    }

    fn check_clean_jobs_returns_true_for_new_work<W: TestWorkbaseFactory>() {
        let workbase1 = W::workbase_that_cleans(100, JobId::new(1));
        let workbase2 = W::workbase_that_cleans(101, JobId::new(2));

        assert!(workbase1.clean_jobs(None));
        assert!(workbase2.clean_jobs(Some(workbase1.as_ref())));
    }

    fn check_clean_jobs_returns_false_for_same_group<W: TestWorkbaseFactory>() {
        let workbase1 = W::workbase_that_cleans(100, JobId::new(1));
        let workbase2 = W::workbase_same_group(100, JobId::new(2));

        assert!(workbase1.clean_jobs(None));
        assert!(!workbase2.clean_jobs(Some(workbase1.as_ref())));
    }

    fn check_job_notify_roundtrip<W: TestWorkbaseFactory>() {
        let job_id = JobId::new(1);
        let workbase = W::workbase_that_cleans(100, job_id);
        let job = W::create_test_job(&workbase, job_id);

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

    fn check_empty_jobs_get_returns_none<W: TestWorkbaseFactory>() {
        let jobs: Jobs<W> = Jobs::new();

        assert!(jobs.get(&JobId::new(0)).is_none());
        assert!(jobs.get(&JobId::new(1)).is_none());
        assert!(jobs.get(&JobId::new(u64::MAX)).is_none());
        assert!(jobs.latest.is_none());
        assert!(jobs.valid.is_empty());
    }

    fn check_insert_same_job_id_replaces<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let job_id = JobId::new(42);
        let workbase1 = W::workbase_that_cleans(100, job_id);

        let enonce1 = Extranonce::random(ENONCE1_SIZE);
        let job1 = Arc::new(
            workbase1
                .create_job(&enonce1, 8, W::test_address().as_ref(), job_id, None)
                .unwrap(),
        );

        jobs.insert(job1.clone());
        assert_eq!(jobs.valid.len(), 1);

        let workbase2 = W::workbase_same_group(100, job_id);
        let enonce2 = Extranonce::random(ENONCE1_SIZE);
        let job2 = Arc::new(
            workbase2
                .create_job(&enonce2, 8, W::test_address().as_ref(), job_id, None)
                .unwrap(),
        );

        jobs.insert(job2.clone());

        assert_eq!(jobs.valid.len(), 1);

        let retrieved = jobs.get(&job_id).unwrap();
        assert!(Arc::ptr_eq(&retrieved, &job2));
        assert!(!Arc::ptr_eq(&retrieved, &job1));
    }

    fn check_lru_eviction<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        for i in 0..LRU_CACHE_SIZE {
            let mut bytes = [0u8; 32];
            bytes[0] = (i & 0xff) as u8;
            bytes[1] = ((i >> 8) & 0xff) as u8;
            let hash = BlockHash::from_byte_array(bytes);
            assert!(
                !jobs.is_duplicate(hash),
                "hash {i} should not be duplicate on first insert"
            );
        }

        let new_hash = BlockHash::from_byte_array([255u8; 32]);
        assert!(
            !jobs.is_duplicate(new_hash),
            "new hash should not be duplicate"
        );

        let oldest_hash = BlockHash::from_byte_array([0u8; 32]);
        assert!(
            !jobs.is_duplicate(oldest_hash),
            "oldest hash should have been evicted and not be duplicate"
        );
    }

    fn check_multiple_jobs_accumulation<W: TestWorkbaseFactory>() {
        let mut jobs: Jobs<W> = Jobs::new();

        let first_id = jobs.next_id();
        let workbase_first = W::workbase_that_cleans(100, first_id);
        let first_job = W::create_test_job(&workbase_first, first_id);
        let clean = jobs.insert(first_job);
        assert!(clean, "first insert should clean");

        let mut job_ids = vec![first_id];
        for _ in 0..4 {
            let id = jobs.next_id();
            let workbase = W::workbase_same_group(100, id);
            job_ids.push(id);
            let job = W::create_test_job(&workbase, id);

            let clean = jobs.insert(job);
            assert!(!clean, "same group should not clean");
        }

        assert_eq!(jobs.valid.len(), 5);
        for id in &job_ids {
            assert!(jobs.get(id).is_some(), "job {id:?} should exist");
        }

        assert_eq!(jobs.latest.as_ref().unwrap().job_id, job_ids[4]);

        let new_id = jobs.next_id();
        let workbase_new = W::workbase_that_cleans(101, new_id);
        let new_job = W::create_test_job(&workbase_new, new_id);

        let clean = jobs.insert(new_job);
        assert!(clean, "new height should clean");

        assert_eq!(jobs.valid.len(), 1);
        for id in &job_ids {
            assert!(jobs.get(id).is_none(), "old job {id:?} should be cleaned");
        }
        assert!(jobs.get(&new_id).is_some());
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
    fn insert_returns_clean_jobs() {
        check_insert_returns_clean_jobs::<BlockTemplate>();
        check_insert_returns_clean_jobs::<Notify>();
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

    #[test]
    fn empty_jobs_get_returns_none() {
        check_empty_jobs_get_returns_none::<BlockTemplate>();
        check_empty_jobs_get_returns_none::<Notify>();
    }

    #[test]
    fn insert_same_job_id_replaces() {
        check_insert_same_job_id_replaces::<BlockTemplate>();
        check_insert_same_job_id_replaces::<Notify>();
    }

    #[test]
    fn lru_eviction() {
        check_lru_eviction::<BlockTemplate>();
        check_lru_eviction::<Notify>();
    }

    #[test]
    fn multiple_jobs_accumulation() {
        check_multiple_jobs_accumulation::<BlockTemplate>();
        check_multiple_jobs_accumulation::<Notify>();
    }
}
