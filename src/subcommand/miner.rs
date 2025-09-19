use {super::*, controller::Controller, hasher::Hasher, stratum::Client};

mod controller;
mod hasher;

#[derive(Debug, Clone)]
pub struct PerformanceTracker {
    pub hashes_completed: Arc<AtomicU64>,
    pub shares_accepted: Arc<AtomicU32>,
    pub shares_rejected: Arc<AtomicU32>,
    pub shares_stale: Arc<AtomicU32>,
    pub jobs_received: Arc<AtomicU32>,
    pub last_job_latency_ms: Arc<AtomicU64>,
    pub start_time: Arc<AtomicU64>,
}

impl PerformanceTracker {
    pub fn new() -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        Self {
            hashes_completed: Arc::new(AtomicU64::new(0)),
            shares_accepted: Arc::new(AtomicU32::new(0)),
            shares_rejected: Arc::new(AtomicU32::new(0)),
            shares_stale: Arc::new(AtomicU32::new(0)),
            jobs_received: Arc::new(AtomicU32::new(0)),
            last_job_latency_ms: Arc::new(AtomicU64::new(0)),
            start_time: Arc::new(AtomicU64::new(now)),
        }
    }

    pub fn get_current_hashrate(&self) -> f64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let start = self.start_time.load(Ordering::Relaxed);
        let elapsed_ms = now.saturating_sub(start);

        if elapsed_ms > 0 {
            let total_hashes = self.hashes_completed.load(Ordering::Relaxed) as f64;
            total_hashes / (elapsed_ms as f64 / 1000.0)
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub architecture: &'static str,
    pub os: &'static str,
    pub memory_total_kb: Option<u64>,
}

impl SystemInfo {
    fn read_memory_total() -> Option<u64> {
        Some(system_utils::get_total_memory() / 1024)
    }

    pub fn get_memory_available_kb() -> Option<u64> {
        Some(system_utils::get_available_memory() / 1024)
    }
}

static SYSTEM_INFO_DATA: std::sync::OnceLock<SystemInfo> = std::sync::OnceLock::new();

fn get_system_info() -> &'static SystemInfo {
    SYSTEM_INFO_DATA.get_or_init(|| SystemInfo {
        architecture: std::env::consts::ARCH,
        os: std::env::consts::OS,
        memory_total_kb: SystemInfo::read_memory_total(),
    })
}

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>")]
    username: String,
    #[arg(long, help = "Stratum <PASSWORD>")]
    password: String,
    #[arg(
        long,
        help = "Number of CPU cores to use (default: auto-detect)",
        value_name = "CORES"
    )]
    cpu_cores: Option<usize>,
    #[arg(long, help = "Enable performance monitoring")]
    monitor_performance: bool,
}

struct MinerRuntime {
    thread_pool: rayon::ThreadPool,
    _tokio_runtime: tokio::runtime::Runtime,
    performance_tracker: PerformanceTracker,
    shutdown_flag: Arc<AtomicBool>,
}

impl Miner {
    pub(crate) fn run(&self) -> Result {
        let runtime = self.build_runtime()?;
        let performance_tracker = runtime.performance_tracker.clone();
        let shutdown_flag = runtime.shutdown_flag.clone();
        let thread_pool = &runtime.thread_pool;

        runtime._tokio_runtime.block_on(async {
            self.run_async(thread_pool, performance_tracker, shutdown_flag)
                .await
        })
    }

    async fn run_async(
        &self,
        thread_pool: &rayon::ThreadPool,
        performance_tracker: PerformanceTracker,
        shutdown_flag: Arc<AtomicBool>,
    ) -> Result {
        self.setup_environment(thread_pool)?;

        let client = match self.connect_client().await {
            Ok(client) => client,
            Err(e) => {
                error!(
                    error = %e,
                    host = %self.host,
                    port = self.port,
                    "Failed to connect to stratum server"
                );
                shutdown_flag.store(true, Ordering::Relaxed);
                return Err(e);
            }
        };

        let controller = match self.create_controller(client).await {
            Ok(controller) => controller,
            Err(e) => {
                error!(error = %e, "Failed to create controller");
                shutdown_flag.store(true, Ordering::Relaxed);
                return Err(e);
            }
        };

        self.start_monitoring(thread_pool, performance_tracker, shutdown_flag.clone());

        info!("Miner started successfully, awaiting shutdown signal");

        let result = controller.run().await;

        info!("Controller finished, initiating graceful shutdown");
        shutdown_flag.store(true, Ordering::Relaxed);

        tokio::time::sleep(Duration::from_millis(300)).await;

        result
    }

    fn setup_environment(&self, thread_pool: &rayon::ThreadPool) -> Result<()> {
        mining_utils::validate_thread_pool(thread_pool)?;
        self.log_system_info(thread_pool);
        Ok(())
    }

    async fn connect_client(&self) -> Result<Client> {
        info!(
            host = %self.host,
            port = self.port,
            username = %self.username,
            "Connecting to stratum server"
        );

        info!("Attempting TCP connection...");

        let result = Client::connect(
            (self.host.clone(), self.port),
            &self.username,
            &self.password,
            Duration::from_secs(10),
        )
        .await;

        match &result {
            Ok(_) => {
                info!("Successfully connected to stratum server");
            }
            Err(e) => {
                error!(
                    error = %e,
                    "Failed to connect to stratum server"
                );
            }
        }

        result
    }

    async fn create_controller(&self, client: Client) -> Result<Controller> {
        Controller::new(client, self.cpu_cores).await
    }

    fn start_monitoring(
        &self,
        thread_pool: &rayon::ThreadPool,
        performance_tracker: PerformanceTracker,
        shutdown_flag: Arc<AtomicBool>,
    ) {
        if self.monitor_performance {
            self.spawn_performance_monitor(thread_pool, performance_tracker, shutdown_flag);
        }
    }

    fn build_runtime(&self) -> Result<MinerRuntime> {
        let thread_pool = mining_utils::configure_rayon_for_mining(self.cpu_cores)?;
        let num_cores = system_utils::get_cpu_count();
        let tokio_threads = std::cmp::max(2, num_cores / 4);

        info!(
            tokio_threads = tokio_threads,
            rayon_threads = mining_utils::get_pool_thread_count(&thread_pool),
            "Configuring runtime"
        );

        let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(tokio_threads)
            .thread_name("tokio-miner")
            .thread_stack_size(2 * 1024 * 1024)
            .enable_all()
            .build()
            .map_err(|e| anyhow!("Failed to build tokio runtime: {}", e))?;

        Ok(MinerRuntime {
            thread_pool,
            _tokio_runtime: tokio_runtime,
            performance_tracker: PerformanceTracker::new(),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    fn log_system_info(&self, thread_pool: &rayon::ThreadPool) {
        let num_cores = system_utils::get_cpu_count();
        let cores = self.cpu_cores.unwrap_or(num_cores);
        let actual_threads = mining_utils::get_pool_thread_count(thread_pool);

        info!(
            detected_cores = num_cores,
            configured_cores = cores,
            active_threads = actual_threads,
            "System information initialized"
        );

        info!("Benchmarking hashrate");
        let benchmarked_hashrate = mining_utils::benchmark_hashrate(thread_pool);

        info!(
            hashrate = benchmarked_hashrate,
            hashrate_display = %HashRate(benchmarked_hashrate),
            performance_monitoring = self.monitor_performance,
            "Hashrate benchmark completed"
        );

        self.log_detailed_system_info();
    }

    fn log_detailed_system_info(&self) {
        let system_info = get_system_info();

        info!(
            architecture = system_info.architecture,
            os = system_info.os,
            "System architecture details"
        );

        if let Some(memory_total) = &system_info.memory_total_kb {
            info!(memory_total_kb = memory_total, "System memory information");
        }

        if let Some(memory_available) = SystemInfo::get_memory_available_kb() {
            info!(
                memory_available_kb = memory_available,
                "Available memory information"
            );
        }
    }

    fn spawn_performance_monitor(
        &self,
        thread_pool: &rayon::ThreadPool,
        performance_tracker: PerformanceTracker,
        shutdown_flag: Arc<AtomicBool>,
    ) {
        let thread_count = mining_utils::get_pool_thread_count(thread_pool);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(30));
            let mut last_hashes = 0u64;
            let mut last_time = std::time::Instant::now();

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if shutdown_flag.load(Ordering::Relaxed) {
                            info!("Performance monitor shutting down");
                            break;
                        }

                        let current_hashes = performance_tracker.hashes_completed.load(Ordering::Relaxed);
                        let accepted = performance_tracker.shares_accepted.load(Ordering::Relaxed);
                        let rejected = performance_tracker.shares_rejected.load(Ordering::Relaxed);
                        let stale = performance_tracker.shares_stale.load(Ordering::Relaxed);
                        let jobs = performance_tracker.jobs_received.load(Ordering::Relaxed);
                        let job_latency = performance_tracker
                            .last_job_latency_ms
                            .load(Ordering::Relaxed);

                        let now = std::time::Instant::now();
                        let elapsed = now.duration_since(last_time).as_secs_f64();
                        let hash_diff = current_hashes.saturating_sub(last_hashes);
                        let current_hashrate = if elapsed > 0.0 {
                            hash_diff as f64 / elapsed
                        } else {
                            0.0
                        };

                        let total_shares = accepted + rejected + stale;
                        let acceptance_rate = if total_shares > 0 {
                            (accepted as f64 / total_shares as f64) * 100.0
                        } else {
                            0.0
                        };

                        info!(
                            threads = thread_count,
                            current_hashrate = current_hashrate,
                            current_hashrate_display = %HashRate(current_hashrate),
                            average_hashrate = performance_tracker.get_current_hashrate(),
                            average_hashrate_display = %HashRate(performance_tracker.get_current_hashrate()),
                            shares_accepted = accepted,
                            shares_rejected = rejected,
                            shares_stale = stale,
                            total_shares = total_shares,
                            acceptance_rate = ?acceptance_rate,
                            jobs_received = jobs,
                            last_job_latency_ms = ?job_latency,
                            "Performance monitor update"
                        );

                        last_hashes = current_hashes;
                        last_time = now;
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if shutdown_flag.load(Ordering::Relaxed) {
                            info!("Performance monitor shutting down");
                            break;
                        }
                    }
                }
            }
        });
    }
}

pub mod mining_utils {
    use super::*;
    use crate::system_utils;

    pub fn calculate_optimal_chunk_size() -> u32 {
        let num_cores = system_utils::get_cpu_count();

        match num_cores {
            1..=2 => 100_000,
            3..=4 => 75_000,
            5..=8 => 50_000,
            9..=16 => 25_000,
            _ => 10_000,
        }
    }

    pub fn configure_rayon_for_mining(cpu_cores: Option<usize>) -> Result<rayon::ThreadPool> {
        let num_cores = cpu_cores.unwrap_or_else(system_utils::get_cpu_count);

        let create_builder = || {
            rayon::ThreadPoolBuilder::new()
                .num_threads(num_cores)
                .thread_name(|index| format!("rayon-miner-{}", index))
                .stack_size(1024 * 1024)
                .panic_handler(|panic_info| {
                    error!(
                        panic_info = ?panic_info,
                        thread_type = "rayon",
                        "Thread panicked"
                    );
                })
        };

        match create_builder().build_global() {
            Ok(()) => {
                info!(
                    num_threads = num_cores,
                    pool_type = "global",
                    "Configured Rayon thread pool"
                );
                create_builder()
                    .build()
                    .map_err(|e| anyhow!("Failed to create reference pool: {}", e))
            }
            Err(_) => {
                info!(
                    num_threads = num_cores,
                    pool_type = "scoped",
                    "Global thread pool already configured, creating scoped pool"
                );
                create_builder()
                    .build()
                    .map_err(|e| anyhow!("Failed to create scoped Rayon thread pool: {}", e))
            }
        }
    }

    pub fn execute_in_pool<F, R>(pool: &rayon::ThreadPool, work: F) -> R
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        pool.install(work)
    }

    pub fn get_pool_thread_count(pool: &rayon::ThreadPool) -> usize {
        pool.current_num_threads()
    }

    pub fn validate_thread_pool(pool: &rayon::ThreadPool) -> Result<()> {
        let thread_count = get_pool_thread_count(pool);
        if thread_count == 0 {
            return Err(anyhow!("Thread pool has no active threads"));
        }

        let test_result = execute_in_pool(pool, || {
            let chunk_size = calculate_optimal_chunk_size();
            (0..chunk_size).fold(0u64, |acc, x| acc.wrapping_add(x.wrapping_mul(2) as u64))
        });

        if test_result == 0 {
            return Err(anyhow!("Thread pool validation failed"));
        }

        info!(
            thread_count = thread_count,
            test_result = test_result,
            "Thread pool validation completed"
        );
        Ok(())
    }

    pub fn benchmark_hashrate(pool: &rayon::ThreadPool) -> f64 {
        use rayon::prelude::*;

        let iterations = Arc::new(AtomicU64::new(0));
        let start_time = std::time::Instant::now();
        let target_duration = Duration::from_secs(1);

        execute_in_pool(pool, || {
            while start_time.elapsed() < target_duration {
                let batch_size = 10_000u32;
                let local_iterations = (0..batch_size)
                    .into_par_iter()
                    .map(|nonce| {
                        let mut hash_input = [0u8; 32];
                        hash_input[..4].copy_from_slice(&nonce.to_le_bytes());

                        let mut result = 0u64;
                        for _ in 0..100 {
                            result = result.wrapping_add(
                                hash_input
                                    .iter()
                                    .enumerate()
                                    .map(|(i, &b)| (b as u64).wrapping_mul(i as u64 + 1))
                                    .sum::<u64>(),
                            );
                        }
                        result
                    })
                    .count();

                iterations.fetch_add(local_iterations as u64 * 100, Ordering::Relaxed);

                if start_time.elapsed() >= target_duration {
                    break;
                }
            }
        });

        let elapsed = start_time.elapsed();
        let total_iterations = iterations.load(Ordering::Relaxed);

        total_iterations as f64 / elapsed.as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_miner_args(args: &str) -> Miner {
        match Arguments::try_parse_from(args.split_whitespace()) {
            Ok(arguments) => match arguments.subcommand {
                Subcommand::Miner(miner) => miner,
                subcommand => panic!("unexpected subcommand: {subcommand:?}"),
            },
            Err(err) => panic!("error parsing arguments: {err}"),
        }
    }

    #[test]
    fn parse_basic_args() {
        parse_miner_args(
            "para miner parasite.wtf:42069 \
                --username bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1.aed48ef@parasite.sati.pro \
                --password x",
        );
    }

    #[test]
    fn parse_args_with_cpu_cores() {
        let miner = parse_miner_args(
            "para miner \
                --host parasite.wtf \
                --port 42069 \
                --username test.worker \
                --password x \
                --cpu-cores 8 \
                --monitor-performance",
        );

        assert_eq!(miner.cpu_cores, Some(8));
        assert!(miner.monitor_performance);
    }

    #[test]
    fn test_chunk_size_calculation() {
        let chunk_size = mining_utils::calculate_optimal_chunk_size();
        assert!(chunk_size > 0);
        assert!(chunk_size <= 100_000);
    }

    #[test]
    fn test_performance_tracker() {
        let tracker = PerformanceTracker::new();

        tracker.hashes_completed.store(1000, Ordering::Relaxed);
        tracker.shares_accepted.store(5, Ordering::Relaxed);
        tracker.shares_rejected.store(1, Ordering::Relaxed);
        tracker.last_job_latency_ms.store(25, Ordering::Relaxed);
        tracker.jobs_received.store(1, Ordering::Relaxed);

        assert_eq!(tracker.hashes_completed.load(Ordering::Relaxed), 1000);
        assert_eq!(tracker.shares_accepted.load(Ordering::Relaxed), 5);
        assert_eq!(tracker.shares_rejected.load(Ordering::Relaxed), 1);
        assert_eq!(tracker.last_job_latency_ms.load(Ordering::Relaxed), 25);
        assert_eq!(tracker.jobs_received.load(Ordering::Relaxed), 1);

        let hashrate = tracker.get_current_hashrate();
        assert!(hashrate >= 0.0);
    }

    #[test]
    fn test_system_info_caching() {
        let system_info1 = get_system_info();
        let system_info2 = get_system_info();

        assert_eq!(system_info1.architecture, system_info2.architecture);
        assert_eq!(system_info1.os, system_info2.os);
        assert_eq!(system_info1.memory_total_kb, system_info2.memory_total_kb);

        assert!(!system_info1.architecture.is_empty());
        assert!(!system_info1.os.is_empty());
    }

    #[test]
    fn test_memory_available_fresh_read() {
        let mem1 = SystemInfo::get_memory_available_kb();
        let mem2 = SystemInfo::get_memory_available_kb();

        assert_eq!(mem1.is_some(), mem2.is_some());

        assert!(
            mem1.is_some(),
            "Should read memory info on all supported platforms"
        );
        assert!(
            mem1.unwrap() > 0,
            "Available memory should be greater than 0"
        );
    }

    #[test]
    fn test_miner_configuration_validation() {
        let miner = Miner {
            host: "localhost".to_string(),
            port: 42069,
            username: "test_user".to_string(),
            password: "x".to_string(),
            cpu_cores: Some(2),
            monitor_performance: true,
        };

        let thread_pool_result = mining_utils::configure_rayon_for_mining(miner.cpu_cores);
        assert!(
            thread_pool_result.is_ok(),
            "Thread pool should build successfully"
        );

        let thread_pool = thread_pool_result.unwrap();

        assert_eq!(mining_utils::get_pool_thread_count(&thread_pool), 2);

        let validation_result = mining_utils::validate_thread_pool(&thread_pool);
        assert!(
            validation_result.is_ok(),
            "Thread pool should validate successfully"
        );

        let tracker = PerformanceTracker::new();
        assert_eq!(tracker.hashes_completed.load(Ordering::Relaxed), 0);
        assert_eq!(tracker.shares_accepted.load(Ordering::Relaxed), 0);

        let initial_hashrate = tracker.get_current_hashrate();
        assert_eq!(initial_hashrate, 0.0);

        let chunk_size = mining_utils::calculate_optimal_chunk_size();
        assert!(chunk_size > 0);
        assert!(chunk_size <= 100_000);

        let benchmarked_hashrate = mining_utils::benchmark_hashrate(&thread_pool);
        assert!(benchmarked_hashrate > 0.0);

        let system_info = get_system_info();
        assert!(!system_info.architecture.is_empty());
        assert!(!system_info.os.is_empty());
    }

    #[tokio::test]
    async fn test_miner_connection_failure() {
        let miner = Miner {
            host: "127.0.0.1".to_string(),
            port: 12345,
            username: "test_user".to_string(),
            password: "x".to_string(),
            cpu_cores: Some(1),
            monitor_performance: false,
        };

        let result = tokio::time::timeout(Duration::from_secs(15), miner.connect_client()).await;
        assert!(result.is_ok(), "Test should not timeout");
        assert!(
            result.unwrap().is_err(),
            "Connection to non-existent server should fail"
        );
    }

    #[test]
    fn test_hashrate_benchmark() {
        let pool = mining_utils::configure_rayon_for_mining(Some(2)).unwrap();
        let benchmarked = mining_utils::benchmark_hashrate(&pool);
        assert!(benchmarked > 0.0);
    }

    #[test]
    fn test_scoped_thread_pool_creation() {
        let pool1 = mining_utils::configure_rayon_for_mining(Some(2));
        assert!(pool1.is_ok());

        let pool2 = mining_utils::configure_rayon_for_mining(Some(4));
        assert!(pool2.is_ok());

        let pool1 = pool1.unwrap();
        let pool2 = pool2.unwrap();

        let result1 = mining_utils::execute_in_pool(&pool1, || 42);
        let result2 = mining_utils::execute_in_pool(&pool2, || 24);

        assert_eq!(result1, 42);
        assert_eq!(result2, 24);
    }

    #[test]
    fn test_thread_pool_utilities() {
        let pool = mining_utils::configure_rayon_for_mining(Some(2)).unwrap();
        let thread_count = mining_utils::get_pool_thread_count(&pool);
        assert!(thread_count <= 2);

        let work_result =
            mining_utils::execute_in_pool(&pool, || (0..100).map(|x| x * x).sum::<i32>());
        assert_eq!(work_result, 328350);
    }

    #[test]
    fn test_mining_work_execution() {
        let pool = mining_utils::configure_rayon_for_mining(Some(4)).unwrap();

        let chunk_size = mining_utils::calculate_optimal_chunk_size();
        let mock_mining_work = mining_utils::execute_in_pool(&pool, move || {
            (0..chunk_size).fold(0u64, |acc, nonce| {
                acc.wrapping_add(nonce.wrapping_mul(0xdeadbeef) as u64)
            })
        });

        assert!(mock_mining_work > 0);
        assert_eq!(mining_utils::get_pool_thread_count(&pool), 4);
    }

    #[test]
    fn test_thread_pool_validation() {
        let pool = mining_utils::configure_rayon_for_mining(Some(2)).unwrap();
        assert!(mining_utils::validate_thread_pool(&pool).is_ok());

        let thread_count = mining_utils::get_pool_thread_count(&pool);
        assert!(thread_count > 0 && thread_count <= 2);
    }

    #[tokio::test]
    async fn test_graceful_shutdown() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::time::{Duration, sleep};

        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = shutdown_flag.clone();

        let handle = tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            flag_clone.store(true, Ordering::Relaxed);
        });

        let start_time = std::time::Instant::now();
        while !shutdown_flag.load(Ordering::Relaxed) {
            if start_time.elapsed() > Duration::from_secs(1) {
                panic!("Shutdown flag was never set");
            }
            sleep(Duration::from_millis(5)).await;
            tokio::task::yield_now().await;
        }

        handle.await.unwrap();
        assert!(shutdown_flag.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_performance_monitor_shutdown() {
        let _tracker = PerformanceTracker::new();
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = shutdown_flag.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(10));
            let mut iterations = 0;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        iterations += 1;
                        if flag_clone.load(Ordering::Relaxed) || iterations > 5 {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(1)) => {
                        if flag_clone.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
            }
            iterations
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        shutdown_flag.store(true, Ordering::Relaxed);

        let iterations = handle.await.unwrap();
        assert!(iterations > 0, "Monitor should have run at least once");
        assert!(iterations <= 10, "Monitor should have shut down quickly");
    }
}
