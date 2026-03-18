use super::*;

pub struct Zmq {
    monitor: futures::channel::mpsc::Receiver<SocketEvent>,
    socket: SubSocket,
}

impl Zmq {
    pub async fn connect(settings: Arc<Settings>) -> Result<Self> {
        let endpoint = settings.zmq_block_notifications().to_string();

        info!("Subscribing to hashblock on ZMQ endpoint {endpoint}");

        let (socket, monitor) = connect(&endpoint).await?;

        Ok(Self { monitor, socket })
    }

    pub async fn recv_blockhash(&mut self) -> Result<BlockHash> {
        let socket = &mut self.socket;
        let monitor = &mut self.monitor;

        let message = loop {
            tokio::select! {
                message = socket.recv() => break message?,
                event = monitor.next() => match event {
                    Some(SocketEvent::Disconnected(_)) => bail!("disconnected from ZMQ endpoint"),
                    Some(_) => {}
                    None => bail!("ZMQ monitor closed"),
                }
            }
        };

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

async fn connect(
    endpoint: &str,
) -> Result<(SubSocket, futures::channel::mpsc::Receiver<SocketEvent>)> {
    let socket = match timeout(Duration::from_secs(1), async {
        let mut socket = SubSocket::new();
        let monitor = socket.monitor();

        socket
            .connect(endpoint)
            .await
            .with_context(|| format!("failed to connect to ZMQ endpoint `{endpoint}`"))?;

        socket
            .subscribe("hashblock")
            .await
            .with_context(|| format!("failed to subscribe to hashblock on `{endpoint}`"))?;

        Ok::<_, Error>((socket, monitor))
    })
    .await
    {
        Ok(Ok(socket)) => socket,
        Ok(Err(err)) => return Err(err),
        Err(_) => bail!(
            "timed out connecting to ZMQ endpoint `{endpoint}` - ensure bitcoind is running with `-zmqpubhashblock={endpoint}`"
        ),
    };

    Ok(socket)
}
