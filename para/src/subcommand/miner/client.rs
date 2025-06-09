use super::*;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Status {
    Initialized,
    //Configured,
    //Authorized,
    Subscribed,
}

// Handles all the stratum protocol messages. Holds all the client information and updates the
// hasher with new work/templates. Has a couple channels to the Miner for communication and
// listens/talks to upstream mining pool
pub struct Client {
    pub notifications: (),
    pub requests: (),
    pub status: Status,
    pub stream: TcpStream,
    pub user: String,
    pub password: String,
}

impl Client {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: Option<String>,
    ) -> Result<Self> {
        let stream = TcpStream::connect((host, port)).await?;

        log::info!("Connected to {host}:{port} with user {user}");

        Ok(Self {
            notifications: (),
            requests: (),
            status: Status::Initialized,
            stream,
            user: user.to_string(),
            password: password.unwrap_or("x".to_string()),
        })
    }

    pub async fn subscribe(&mut self) -> Result {
        log::info!("Subscribing...");

        self.stream
            .write_all(
                format!(
                    "{}\n",
                    json!({"id": 1, "method": "mining.subscribe", "params": ["ParaMiner/0.0.1"]})
                )
                .as_bytes(),
            )
            .await?;

        self.stream.flush().await?;

        let (reader, _) = self.stream.split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        // Parse response
        let response: Value = serde_json::from_str(&line)?;

        log::info!("{response}");

        log::info!("Subscribed");
        self.status = Status::Subscribed;

        Ok(())
    }

    pub async fn authorize(&mut self) -> Result {
        log::info!("Authorizing...");

        self.stream
            .write_all(
                format!(
                    "{}\n",
                    json!({"id": 2, "method": "mining.authorize", "params": ["bc1q8jx6g9ujlqmdx3jnt3ap6ll2fdwqjdkdgs959m.worker1", "x"]})
                )
                .as_bytes(),
            )
            .await?;

        self.stream.flush().await?;

        log::info!("Authorized");
        self.status = Status::Subscribed;

        Ok(())
    }
}

//struct Client {
//    client_id: u32,
//    extranonce1: Option<Extranonce<'static>>,
//    extranonce2_size: Option<usize>,
//    version_rolling_mask: Option<HexU32Be>,
//    version_rolling_min_bit: Option<HexU32Be>,
//    miner: Miner,
//}

// Comese from the mining.notify message
//struct Job {
//    job_id: u32,
//    prev_hash: [u8; 32],
//    coinbase_1: Vec<u32>,
//    coinbase_2: Vec<u32>,
//    merkle_brances: Vec<[u8; 32]>,
//    merkle_root: [u8; 32],
//    version: u32,
//    nbits: u32,
//    _ntime: u32,       // not needed?
//    _clean_jobs: bool, // not needed
//}
//
