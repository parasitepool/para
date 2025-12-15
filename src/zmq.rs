use {super::*, settings::Settings, subcommand::pool::pool_config::PoolConfig};

pub struct Zmq {
    socket: SubSocket,
}

impl Zmq {
    pub async fn connect(settings: &Settings, config: &PoolConfig) -> Result<Self> {
        let endpoint = config.zmq_block_notifications(settings).to_string();

        info!("Subscribing to hashblock on ZMQ endpoint {endpoint}");

        let socket = timeout(Duration::from_secs(1), async {
            let mut socket = SubSocket::new();
            socket.connect(&endpoint).await?;
            socket.subscribe("hashblock").await?;

            Ok::<_, Error>(socket)
        })
        .await??;

        Ok(Self { socket })
    }

    pub async fn recv_blockhash(&mut self) -> Result<BlockHash> {
        let message = self.socket.recv().await?;

        ensure!(
            message.len() == 3,
            "hashblock: expected 3 frames, got {}",
            message.len()
        );

        let topic = message.get(0).context("hashblock: missing topic")?;

        ensure!(topic.as_ref() == b"hashblock", "hashblock: wrong topic");

        let body = message.get(1).context("hashblock: missing body")?;

        ensure!(body.len() == 32, "hashblock: body len {}", body.len());

        let sequence_number = message
            .get(2)
            .context("hashblock: missing sequence number")?;

        ensure!(
            sequence_number.len() == 4,
            "hashblock: seq len {}",
            sequence_number.len()
        );

        let mut arr = [0u8; 32];
        arr.copy_from_slice(body);
        arr.reverse();

        BlockHash::from_slice(&arr).context("blockhash parse")
    }
}
