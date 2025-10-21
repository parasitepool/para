use {super::*, controller::Controller, hasher::Hasher, stratum::Client};

mod controller;
mod hasher;

#[derive(Debug, Parser)]
pub(crate) struct Miner {
    #[arg(help = "Stratum <HOST:PORT>.")]
    stratum_endpoint: String,
    #[arg(long, help = "Stratum <USERNAME>.")]
    username: String,
    #[arg(long, help = "Stratum <PASSWORD>.")]
    password: Option<String>,
    #[arg(long, help = "Exit <ONCE> a share is found.")]
    once: bool,
    #[arg(long, help = "Number of CPU cores to use.")]
    cpu_cores: Option<usize>,
    #[arg(long, help = "Enable performance monitoring.")]
    monitor_performance: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Share {
    pub extranonce1: Extranonce,
    pub extranonce2: Extranonce,
    pub job_id: JobId,
    pub nonce: Nonce,
    pub ntime: Ntime,
    pub username: String,
    pub version_bits: Option<Version>,
}

impl Miner {
    pub(crate) fn run(&self) -> Result {
        let runtime = self.build_runtime()?;

        mining_utils::configure_rayon_for_mining(self.cpu_cores)?;

        runtime.block_on(async {
            self.initialize_logging();
            self.log_system_info();

            info!(
                "Connecting to {} with user {}",
                self.stratum_endpoint, self.username
            );

            let address = resolve_stratum_endpoint(&self.stratum_endpoint).await?;

            let client = Client::connect(
                address,
                self.username.clone(),
                self.password.clone(),
                Duration::from_secs(10),
            )
            .await?;

            if self.monitor_performance {
                self.spawn_performance_monitor();
            }

            let controller = Controller::new(client, self.cpu_cores, self.once).await?;

            let shares = controller.run().await?;

            println!("{}", serde_json::to_string_pretty(&shares)?);

            Ok(())
        })
    }

    fn build_runtime(&self) -> Result<tokio::runtime::Runtime> {
        let num_cores = mining_utils::get_cpu_count();

        let tokio_threads = std::cmp::max(2, num_cores / 4);

        info!(
            "Configuring runtime: {} tokio threads, {} cpu cores",
            tokio_threads,
            self.cpu_cores.unwrap_or(num_cores)
        );

        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(tokio_threads)
            .thread_name("tokio-miner")
            .thread_stack_size(2 * 1024 * 1024)
            .enable_all()
            .build()
            .map_err(|e| anyhow!("Failed to build tokio runtime: {}", e))
    }

    fn initialize_logging(&self) {
        use tracing_subscriber::{EnvFilter, FmtSubscriber};

        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        let subscriber = FmtSubscriber::builder()
            .with_env_filter(filter)
            .with_target(false)
            .with_thread_ids(true)
            .with_line_number(true)
            .finish();

        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    fn log_system_info(&self) {
        let num_cores = mining_utils::get_cpu_count();
        let cores = self.cpu_cores.unwrap_or(num_cores);
        let estimated_hashrate = mining_utils::estimate_hashrate();

        info!("System Information:");
        info!("  - CPU cores detected: {}", num_cores);
        info!("  - CPU cores to use: {}", cores);
        info!("  - Estimated hashrate: {}", HashRate(estimated_hashrate));
        info!("  - Performance monitoring: {}", self.monitor_performance);

        self.log_detailed_system_info();
    }

    fn log_detailed_system_info(&self) {
        info!("  - Architecture: {}", std::env::consts::ARCH);
        info!("  - OS: {}", std::env::consts::OS);

        if std::path::Path::new("/proc/meminfo").exists()
            && let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo")
        {
            for line in meminfo.lines().take(3) {
                if line.starts_with("MemTotal") || line.starts_with("MemAvailable") {
                    info!("  - {}", line.replace(":", ""));
                }
            }
        }
    }

    fn spawn_performance_monitor(&self) {
        tokio::spawn(async {
            let mut interval = tokio::time::interval(Duration::from_secs(30));

            loop {
                interval.tick().await;

                let num_threads = rayon::current_num_threads();
                info!("Performance: {} Rayon threads active", num_threads);
            }
        });
    }
}

pub mod mining_utils {
    use super::*;
    use sysinfo::System;

    pub fn get_cpu_count() -> usize {
        let mut system = System::new();
        system.refresh_cpu_all();
        system.cpus().len()
    }

    pub fn configure_rayon_for_mining(cpu_cores: Option<usize>) -> Result<()> {
        let num_cores = cpu_cores.unwrap_or_else(get_cpu_count);

        rayon::ThreadPoolBuilder::new()
            .num_threads(num_cores)
            .thread_name(|index| format!("rayon-miner-{}", index))
            .stack_size(1024 * 1024)
            .panic_handler(|panic_info| {
                error!("Rayon thread panicked: {:?}", panic_info);
            })
            .build_global()
            .map_err(|e| anyhow!("Failed to configure Rayon: {}", e))
    }

    pub fn estimate_hashrate() -> f64 {
        let num_cores = get_cpu_count();

        let base_hashrate_per_core = match std::env::consts::ARCH {
            "x86_64" => 50_000.0,
            "aarch64" => 30_000.0,
            _ => 25_000.0,
        };

        let efficiency = match num_cores {
            1 => 1.0,
            2..=4 => 0.95,
            5..=8 => 0.90,
            9..=16 => 0.85,
            _ => 0.80,
        };

        base_hashrate_per_core * num_cores as f64 * efficiency
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
    fn parse_args() {
        parse_miner_args(
            "para miner parasite.wtf:42069 \
                --username bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1.aed48ef@parasite.sati.pro \
                --password x",
        );
    }

    #[test]
    fn parse_args_with_cpu_cores() {
        let miner = parse_miner_args(
            "para miner parasite.wtf:42069 \
                --username test.worker \
                --password x \
                --cpu-cores 8 \
                --monitor-performance",
        );

        assert_eq!(miner.cpu_cores, Some(8));
        assert!(miner.monitor_performance);
    }

    #[test]
    fn test_hashrate_estimation() {
        let estimated = mining_utils::estimate_hashrate();
        assert!(estimated > 0.0);

        let cores = mining_utils::get_cpu_count();
        assert!(estimated > cores as f64 * 10_000.0);
    }
}
