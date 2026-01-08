use super::*;

pub(crate) trait Source: Clone + Send + Sync + 'static {
    fn create_job(
        workbase: &Arc<Workbase<Self>>,
        enonce1: &Extranonce,
        enonce2_size: usize,
        address: Option<&Address>,
        job_id: JobId,
        version_mask: Option<Version>,
    ) -> Job<Self>
    where
        Self: Sized;

    fn clean_jobs(prev: Option<&Workbase<Self>>, curr: &Workbase<Self>) -> bool
    where
        Self: Sized;
    fn build_block(job: &Job<Self>, submit: &Submit, header: Header) -> Option<Block>
    where
        Self: Sized;

    fn height(workbase: &Workbase<Self>) -> u64
    where
        Self: Sized;

    fn parse_address(username: &Username, network: Network) -> Result<Option<Address>>;
}
