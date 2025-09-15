use {
    super::*,
    crate::{job::Job, subcommand::pool::pool_config::PoolConfig},
};

#[derive(Debug)]
pub(crate) enum State {
    Init,
    Configured {
        version_mask: Option<Version>,
    },
    Subscribed {
        extranonce1: Extranonce,
        _user_agent: String,
        version_mask: Option<Version>,
    },
    Authorized,
    Working {
        job: Box<Job>,
    },
}

pub(crate) struct Connection<R, W> {
    config: Arc<PoolConfig>,
    worker: SocketAddr,
    reader: FramedRead<R, LinesCodec>,
    writer: FramedWrite<W, LinesCodec>,
    state: State,
}

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin + AsyncBufReadExt,
    W: AsyncWrite + Unpin,
{
    pub(crate) fn new(config: Arc<PoolConfig>, worker: SocketAddr, reader: R, writer: W) -> Self {
        Self {
            config,
            worker,
            reader: FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE)),
            writer: FramedWrite::new(writer, LinesCodec::new()),
            state: State::Init,
        }
    }

    pub(crate) async fn serve(&mut self) -> Result {
        while let Some(message) = self.read_message().await? {
            let Message::Request { id, method, params } = message else {
                warn!(?message, "Ignoring any notifications or responses");
                continue;
            };

            match method.as_str() {
                "mining.configure" => {
                    info!("CONFIGURE from {} with {params}", self.worker);

                    let configure = serde_json::from_value::<Configure>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.configure(id, configure).await?
                }
                "mining.subscribe" => {
                    info!("SUBSCRIBE from {} with {params}", self.worker);

                    let subscribe = serde_json::from_value::<Subscribe>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.subscribe(id, subscribe).await?
                }
                "mining.authorize" => {
                    info!("AUTHORIZE from {} with {params}", self.worker);

                    let authorize = serde_json::from_value::<Authorize>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.authorize(id, authorize).await?
                }

                "mining.submit" => {
                    info!("SUBMIT from {} with params {params}", self.worker);

                    let submit = serde_json::from_value::<Submit>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.submit(id, submit).await?;

                    break;
                }
                method => {
                    warn!("UNKNOWN method {method} with {params} from {}", self.worker);
                }
            }
        }

        Ok(())
    }

    async fn configure(&mut self, id: Id, configure: Configure) -> Result {
        if configure.version_rolling_mask.is_some() {
            let version_mask = self.config.version_mask();
            info!(
                "Configuring version rolling for {} with version mask {version_mask}",
                self.worker
            );

            let message = Message::Response {
                id,
                result: Some(
                    json!({"version-rolling": true, "version-rolling.mask": self.config.version_mask()}),
                ),
                error: None,
                reject_reason: None,
            };

            self.send(message).await?;

            self.state = State::Configured {
                version_mask: Some(version_mask),
            };
        } else {
            warn!("Unsupported extension {:?}", configure);

            let message = Message::Response {
                id,
                result: None,
                error: Some(JsonRpcError {
                    error_code: -1,
                    message: "Unsupported extension".into(),
                    traceback: Some(serde_json::to_value(configure)?),
                }),
                reject_reason: None,
            };

            self.send(message).await?;
            self.state = State::Init;
        }

        Ok(())
    }

    async fn subscribe(&mut self, id: Id, subscribe: Subscribe) -> Result {
        let version_mask = match &self.state {
            State::Init => None,
            State::Configured { version_mask } => *version_mask,
            _ => bail!("SUBSCRIBE not allowed in current state"),
        };

        if let Some(extranonce1) = subscribe.extranonce1 {
            warn!("Ignoring extranonce1 suggestion: {extranonce1}");
        }

        let extranonce1 = Extranonce::generate(EXTRANONCE1_SIZE);

        let result = SubscribeResult {
            subscriptions: vec![("mining.notify".to_string(), "deadbeef".to_string())],
            extranonce1: extranonce1.clone(),
            extranonce2_size: EXTRANONCE2_SIZE.try_into().unwrap(),
        };

        self.state = State::Subscribed {
            _user_agent: subscribe.user_agent,
            extranonce1,
            version_mask,
        };

        self.send(Message::Response {
            id,
            result: Some(json!(result)),
            error: None,
            reject_reason: None,
        })
        .await?;

        Ok(())
    }

    async fn authorize(&mut self, id: Id, authorize: Authorize) -> Result {
        let State::Subscribed {
            extranonce1,
            version_mask,
            ..
        } = &self.state
        else {
            bail!("AUTHORIZE not allowed in current state");
        };

        let address = Address::from_str(
            authorize
                .username
                .trim_matches('"')
                .split('.')
                .next()
                .ok_or_else(|| anyhow!("invalid username {}", authorize.username))?,
        )?
        .require_network(self.config.chain().network())
        .context(format!(
            "invalid username {} for worker {}",
            authorize.username, self.worker
        ))?;

        let job = Job::new(
            address,
            extranonce1.clone(),
            *version_mask,
            self.gbt()?,
            "deadbeef".to_string(),
        )?;

        self.send(Message::Response {
            id,
            result: Some(json!(true)),
            error: None,
            reject_reason: None,
        })
        .await?;

        self.state = State::Authorized;

        info!("Sending SET DIFFICULTY");

        self.send(Message::Notification {
            method: "mining.set_difficulty".into(),
            params: json!(SetDifficulty(Difficulty(1))),
        })
        .await?;

        info!("Sending NOTIFY");

        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(job.notify()?),
        })
        .await?;

        self.state = State::Working { job: Box::new(job) };

        Ok(())
    }

    async fn submit(&mut self, id: Id, submit: Submit) -> Result {
        let State::Working { job } = &self.state else {
            bail!("SUBMIT not allowed in current state");
        };

        let version = if let Some(version_bits) = submit.version_bits {
            let Some(version_mask) = job.version_mask else {
                bail!("Version bits found but no version rolling was negotiated");
            };

            assert!(version_bits != Version::from(0));

            let disallowed = version_bits & !version_mask;

            ensure!(
                disallowed == Version::from(0),
                "miner set disallowed version bits: {disallowed}"
            );

            (job.version() & !version_mask) | (version_bits & version_mask)
        } else {
            job.version()
        };

        let nbits = job.nbits()?;

        let header = Header {
            version: version.into(),
            prev_blockhash: job.prevhash().into(),
            merkle_root: stratum::merkle_root(
                &job.coinb1,
                &job.coinb2,
                &job.extranonce1,
                &submit.extranonce2,
                &job.merkle_branches,
            )?
            .into(),
            time: submit.ntime.into(),
            bits: nbits.into(),
            nonce: submit.nonce.into(),
        };

        let blockhash = header.validate_pow(Target::from_compact(nbits.into()))?;

        info!("Block with hash {blockhash} mets PoW");

        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            job.coinb1, job.extranonce1, submit.extranonce2, job.coinb2,
        ))?;
        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

        let txdata = vec![coinbase_tx]
            .into_iter()
            .chain(
                job.gbt
                    .clone()
                    .transactions
                    .iter()
                    .map(|result| {
                        Transaction::consensus_decode(&mut result.raw_tx.as_slice()).unwrap()
                    })
                    .collect::<Vec<Transaction>>(),
            )
            .collect();

        let block = Block { header, txdata };

        assert!(block.bip34_block_height().is_ok());

        info!("Submitting block solve");

        self.config.bitcoin_rpc_client()?.submit_block(&block)?;

        self.send(Message::Response {
            id,
            result: Some(json!(true)),
            error: None,
            reject_reason: None,
        })
        .await?;

        info!("SUCCESS: mined block {}", block.block_hash());

        Ok(())
    }

    async fn read_message(&mut self) -> Result<Option<Message>> {
        match self.reader.next().await {
            Some(Ok(line)) => {
                let message = serde_json::from_str::<Message>(&line).map_err(|e| {
                    anyhow!(
                        "invalid stratum message from {}: {e}; line={line:?}",
                        self.worker
                    )
                })?;
                Ok(Some(message))
            }
            Some(Err(e)) => Err(anyhow!("read error from {}: {e}", self.worker)),
            None => {
                info!("Worker {} disconnected", self.worker);
                Ok(None)
            }
        }
    }

    async fn send(&mut self, message: Message) -> Result<()> {
        let frame = serde_json::to_string(&message)?;
        self.writer.send(frame).await?;
        Ok(())
    }

    fn gbt(&self) -> Result<GetBlockTemplateResult> {
        let mut rules = vec!["segwit"];
        if self.config.chain().network() == Network::Signet {
            rules.push("signet");
        }

        let params = json!({
            "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
            "rules": rules,
        });

        Ok(self
            .config
            .bitcoin_rpc_client()?
            .call::<GetBlockTemplateResult>("getblocktemplate", &[params])?)
    }
}
