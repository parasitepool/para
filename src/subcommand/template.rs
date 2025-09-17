use super::*;

#[derive(Debug, Parser)]
pub struct Template {
    #[arg(help = "Pool URL (e.g., stratum+tcp://pool.example.com:4444)")]
    pub url: String,

    #[arg(short, long, help = "Username for pool authentication")]
    pub username: Option<String>,

    #[arg(short, long, help = "Password for pool authentication")]
    pub password: Option<String>,

    #[arg(long, default_value = "10", help = "Update interval in seconds")]
    pub interval: u64,

    #[arg(long, help = "Output only the latest template (no continuous updates)")]
    pub once: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TemplateData {
    pub timestamp: u64,
    pub pool_url: String,
    pub job_id: Option<String>,
    pub prev_hash: Option<String>,
    pub coinbase1: Option<String>,
    pub coinbase2: Option<String>,
    pub merkle_branches: Option<Vec<String>>,
    pub version: Option<String>,
    pub nbits: Option<String>,
    pub ntime: Option<String>,
    pub clean_jobs: Option<bool>,
    pub extranonce1: Option<String>,
    pub extranonce2_length: Option<u64>,
}

impl Template {
    pub async fn run(self) -> anyhow::Result<()> {
        let mut client = StratumClient::new(&self.url).await?;

        client.subscribe().await?;

        if let (Some(username), Some(password)) = (&self.username, &self.password) {
            client.authorize(username, password).await?;
        }

        let mut last_output = Instant::now();
        let interval = Duration::from_secs(self.interval);

        loop {
            if let Some(template) = client.get_latest_template().await? {
                let now = Instant::now();

                if self.once || now.duration_since(last_output) >= interval {
                    let output = serde_json::to_string(&template)?;
                    println!("{}", output);

                    if self.once {
                        break;
                    }

                    last_output = now;
                }
            }

            sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }
}

struct StratumClient {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    url: String,
    extranonce1: Option<String>,
    extranonce2_length: Option<u64>,
    latest_job: Option<Value>,
    id_counter: u64,
}

impl StratumClient {
    async fn new(url: &str) -> anyhow::Result<Self> {
        let url_without_prefix = url.trim_start_matches("stratum+tcp://");
        let parts: Vec<&str> = url_without_prefix.split(':').collect();

        if parts.len() != 2 {
            return Err(anyhow::anyhow!(
                "Invalid URL format. Expected: stratum+tcp://host:port"
            ));
        }

        let host = parts[0];
        let port: u16 = parts[1].parse()?;

        let stream = TcpStream::connect((host, port)).await?;
        let (read_half, write_half) = stream.into_split();
        let reader = BufReader::new(read_half);

        Ok(Self {
            reader,
            writer: write_half,
            url: url.to_string(),
            extranonce1: None,
            extranonce2_length: None,
            latest_job: None,
            id_counter: 1,
        })
    }

    async fn send_request(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let request = json!({
            "id": self.id_counter,
            "method": method,
            "params": params
        });

        self.id_counter += 1;

        let request_str = format!("{}\n", serde_json::to_string(&request)?);
        self.writer.write_all(request_str.as_bytes()).await?;
        self.writer.flush().await?;

        Ok(())
    }

    async fn subscribe(&mut self) -> anyhow::Result<()> {
        self.send_request(
            "mining.subscribe",
            json!(["para-template/1.0", null, "stratum+tcp://127.0.0.1", {}]),
        )
        .await?;

        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        if let Ok(response) = serde_json::from_str::<Value>(&line)
            && let Some(result) = response.get("result").and_then(|r| r.as_array())
            && result.len() >= 3
        {
            self.extranonce1 = result[1].as_str().map(|s| s.to_string());
            self.extranonce2_length = result[2].as_u64();
        }

        Ok(())
    }

    async fn authorize(&mut self, username: &str, password: &str) -> anyhow::Result<()> {
        self.send_request("mining.authorize", json!([username, password]))
            .await?;

        let mut line = String::new();
        self.reader.read_line(&mut line).await?;

        Ok(())
    }

    async fn get_latest_template(&mut self) -> anyhow::Result<Option<TemplateData>> {
        let mut line = String::new();
        match tokio::time::timeout(Duration::from_millis(50), self.reader.read_line(&mut line))
            .await
        {
            Ok(Ok(_)) if !line.trim().is_empty() => {
                if let Ok(message) = serde_json::from_str::<Value>(&line)
                    && let Some(method) = message.get("method").and_then(|m| m.as_str())
                {
                    if method == "mining.notify" {
                        self.latest_job = message.get("params").cloned();
                    } else if method == "mining.set_difficulty" {
                    }
                }
            }
            _ => {}
        }

        if let Some(job_params) = &self.latest_job
            && let Some(params_array) = job_params.as_array()
            && params_array.len() >= 9
        {
            let template = TemplateData {
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                pool_url: self.url.clone(),
                job_id: params_array[0].as_str().map(|s| s.to_string()),
                prev_hash: params_array[1].as_str().map(|s| s.to_string()),
                coinbase1: params_array[2].as_str().map(|s| s.to_string()),
                coinbase2: params_array[3].as_str().map(|s| s.to_string()),
                merkle_branches: params_array[4].as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                }),
                version: params_array[5].as_str().map(|s| s.to_string()),
                nbits: params_array[6].as_str().map(|s| s.to_string()),
                ntime: params_array[7].as_str().map(|s| s.to_string()),
                clean_jobs: params_array[8].as_bool(),
                extranonce1: self.extranonce1.clone(),
                extranonce2_length: self.extranonce2_length,
            };

            return Ok(Some(template));
        }

        Ok(None)
    }
}
