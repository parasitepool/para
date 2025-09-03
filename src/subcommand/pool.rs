use {super::*, pool_config::PoolConfig};

pub(crate) mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self) -> Result {
        let config = Arc::new(self.config.clone());
        let address = config.address();
        let port = config.port();

        let listener = TcpListener::bind((address.clone(), port)).await?;

        info!("Listening on {address}:{port}");

        loop {
            tokio::select! {
                result = Self::handle_single_worker(config.clone(), &listener) => {
                    if let Err(err) = result {
                        error!("Worker connection error: {err}")
                    }
                }
                _ = ctrl_c() => {
                        info!("Shutting down stratum server");
                        break;
                    }
            }
        }

        Ok(())
    }

    async fn handle_single_worker(config: Arc<PoolConfig>, listener: &TcpListener) -> Result {
        let (stream, worker) = listener.accept().await?;

        info!("Accepted connection from {worker}");

        let (reader, writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        let mut conn = Connection::new(config.clone(), worker, reader, writer);

        conn.serve().await
    }
}
