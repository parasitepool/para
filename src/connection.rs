use {super::*, crate::job::Job};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum State {
    Init,
    Configured,
    Subscribed,
    Working,
}

pub(crate) struct Connection<R, W> {
    config: Arc<PoolConfig>,
    worker: SocketAddr,
    reader: FramedRead<R, LinesCodec>,
    writer: FramedWrite<W, LinesCodec>,
    template_receiver: watch::Receiver<Arc<BlockTemplate>>,
    jobs: HashMap<JobId, Arc<Job>>,
    state: State,
    difficulty: Difficulty,
    address: Option<Address>,
    authorized: Option<SystemTime>,
    version_mask: Option<Version>,
    extranonce1: Option<Extranonce>,
    _user_agent: Option<String>,
}

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(crate) fn new(
        config: Arc<PoolConfig>,
        worker: SocketAddr,
        reader: R,
        writer: W,
        template_receiver: watch::Receiver<Arc<BlockTemplate>>,
    ) -> Self {
        Self {
            config,
            worker,
            reader: FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE)),
            writer: FramedWrite::new(writer, LinesCodec::new()),
            template_receiver,
            jobs: HashMap::new(),
            state: State::Init,
            difficulty: Difficulty::default(),
            address: None,
            authorized: None,
            version_mask: None,
            extranonce1: None,
            _user_agent: None,
        }
    }

    pub(crate) async fn serve(&mut self) -> Result {
        let mut template_receiver = self.template_receiver.clone();

        loop {
            tokio::select! {
                message = self.read_message() => {
                    let Some(message) = message? else {
                        error!("Error reading message in connection serve");
                        break;
                    };

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
                        }
                        method => {
                            warn!("UNKNOWN method {method} with {params} from {}", self.worker);
                        }
                    }
                }

                changed = template_receiver.changed() => {
                    if changed.is_err() {
                        info!("Template receiver dropped, closing connection with {}", self.worker);
                        break;
                    }
                    let template = template_receiver.borrow().clone();
                    self.template_update(template).await?;
                }
            }
        }

        Ok(())
    }

    async fn template_update(&mut self, template: Arc<BlockTemplate>) -> Result {
        if self.state != State::Working {
            return Ok(());
        };

        let new_job = Job::new(
            self.address.clone().unwrap(),     // TODO
            self.extranonce1.clone().unwrap(), // TODO
            self.version_mask,
            template,
            JobId::from_str("deadbeef").unwrap(), // TODO
        )?;

//        let old_nbits = Nbits::from(self.difficulty.to_target().to_compact_lossy()); // TODO
//        let new_nbits = new_job.nbits();
//        if new_nbits != old_nbits {
//            let difficulty = Difficulty::from(new_nbits);
//            info!("Sending new difficulty {difficulty}");
//            self.send(Message::Notification {
//                method: "mining.set_difficulty".into(),
//                params: json!(SetDifficulty(difficulty)),
//            })
//            .await?;
//        }

        info!("Template updated sending NOTIFY");
        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(new_job.notify()?),
        })
        .await?;

        self.state = State::Working;

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
            self.version_mask = Some(version_mask);
            self.state = State::Configured;
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
            State::Configured => self.version_mask,
            _ => bail!("SUBSCRIBE not allowed in current state"),
        };

        if let Some(extranonce1) = subscribe.extranonce1 {
            warn!("Ignoring worker extranonce1 suggestion: {extranonce1}");
        }

        let extranonce1 = Extranonce::generate(EXTRANONCE1_SIZE);

        self.extranonce1 = Some(extranonce1.clone());
        self._user_agent = Some(subscribe.user_agent);
        self.version_mask = version_mask;
        self.state = State::Subscribed;

        let subscriptions = vec![
            (
                "mining.set_difficulty".to_string(),
                SUBSCRIPTION_ID.to_string(),
            ),
            ("mining.notify".to_string(), SUBSCRIPTION_ID.to_string()),
        ];

        let result = SubscribeResult {
            subscriptions,
            extranonce1: extranonce1.clone(),
            extranonce2_size: EXTRANONCE2_SIZE.try_into().unwrap(),
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
        ensure!(
            matches!(self.state, State::Subscribed),
            "AUTHORIZE before SUBSCRIBE"
        );

        let extranonce1 = self
            .extranonce1
            .clone()
            .ok_or_else(|| anyhow!("missing extranonce1 do SUBSCRIBE first"))?;

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

        let job_id = "deadbeef".parse().unwrap(); // TODO

        let job = Job::new(
            address.clone(),
            extranonce1.clone(),
            self.version_mask,
            self.template_receiver.borrow().clone(),
            job_id,
        )?;

        self.send(Message::Response {
            id,
            result: Some(json!(true)),
            error: None,
            reject_reason: None,
        })
        .await?;

        self.address = Some(address);

        if self.authorized.is_none() {
            self.authorized = Some(SystemTime::now());
        }

        info!("Sending SET DIFFICULTY");

        self.send(Message::Notification {
            method: "mining.set_difficulty".into(),
            params: json!(SetDifficulty(Difficulty::from(job.nbits()))),
        })
        .await?;

        info!("Sending NOTIFY");

        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(job.notify()?),
        })
        .await?;

        self.jobs.insert(job_id, Arc::new(job));

        self.state = State::Working;

        Ok(())
    }

    async fn submit(&mut self, id: Id, submit: Submit) -> Result {
        if self.state != State::Working {
            bail!("SUBMIT not allowed in current state");
        }

        let Some(job) = self.jobs.get(&submit.job_id) else {
            bail!("Stale job"); // TODO
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

        let nbits = job.nbits();

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
            bits: nbits.to_compact(),
            nonce: submit.nonce.into(),
        };

        let blockhash = header.validate_pow(Target::from_compact(nbits.into()))?;

        info!("Block with hash {blockhash} meets PoW");

        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            job.coinb1, job.extranonce1, submit.extranonce2, job.coinb2,
        ))?;

        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

        let txdata = std::iter::once(coinbase_tx)
            .chain(
                job.template
                    .transactions
                    .iter()
                    .map(|tx| tx.transaction.clone())
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
}
