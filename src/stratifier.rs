use {super::*, crate::subcommand::pool::pool_config::PoolConfig};

#[derive(Debug)]
pub(crate) enum State {
    Init,
    Configured {
        version_mask: Option<Version>,
    },
    Subscribed {
        extranonce1: String,
        _user_agent: String,
        version_mask: Option<Version>,
    },
    Authorized,
    Working {
        job: Box<Job>,
    },
}

#[derive(Debug)]
pub(crate) struct Job {
    pub(crate) coinb1: String,
    pub(crate) coinb2: String,
    pub(crate) extranonce1: String,
    pub(crate) gbt: GetBlockTemplateResult,
    pub(crate) job_id: String,
    pub(crate) merkle_branches: Vec<TxMerkleNode>,
    pub(crate) version_mask: Option<Version>,
}

impl Job {
    pub(crate) fn new(
        address: Address,
        extranonce1: String,
        version_mask: Option<Version>,
        gbt: GetBlockTemplateResult,
    ) -> Result<Self> {
        let job_id = "deadbeef".to_string();

        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address,
            extranonce1.clone(),
            EXTRANONCE2_SIZE,
            gbt.height,
            gbt.coinbase_value,
            gbt.default_witness_commitment.clone(),
        )
        .with_pool_sig("|parasite|".into())
        .build()?;

        let merkle_branches = stratum::merkle_branches(
            gbt.transactions
                .clone()
                .into_iter()
                .map(|r| r.txid)
                .collect(),
        );

        Ok(Self {
            coinb1,
            coinb2,
            extranonce1,
            gbt,
            job_id,
            merkle_branches,
            version_mask,
        })
    }

    pub(crate) fn nbits(&self) -> Result<Nbits> {
        Nbits::from_str(&hex::encode(&self.gbt.bits))
    }

    pub(crate) fn prevhash(&self) -> PrevHash {
        PrevHash::from(self.gbt.previous_block_hash)
    }

    pub(crate) fn version(&self) -> Version {
        Version(block::Version::from_consensus(
            self.gbt.version.try_into().unwrap(),
        ))
    }

    pub(crate) fn notify(&self) -> Result<Notify> {
        Ok(Notify {
            job_id: self.job_id.clone(),
            prevhash: self.prevhash(),
            coinb1: self.coinb1.clone(),
            coinb2: self.coinb2.clone(),
            merkle_branches: self.merkle_branches.clone(),
            version: self.version(),
            nbits: self.nbits()?,
            ntime: Ntime::try_from(self.gbt.current_time).expect("fits until ~2106"),
            clean_jobs: true,
        })
    }
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

            match (&mut self.state, method.as_str()) {
                (State::Init, "mining.configure") => {
                    info!("CONFIGURE from {} with {params}", self.worker);

                    let configure = serde_json::from_value::<Configure>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.on_configure(id, configure).await?
                }
                (State::Init, "mining.subscribe") => {
                    info!("SUBSCRIBE from {} with {params}", self.worker);

                    let subscribe = serde_json::from_value::<Subscribe>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.on_subscribe(id, subscribe).await?
                }
                (State::Configured { .. }, "mining.subscribe") => {
                    info!("SUBSCRIBE from {} with {params}", self.worker);

                    let subscribe = serde_json::from_value::<Subscribe>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.on_subscribe(id, subscribe).await?
                }
                (State::Subscribed { .. }, "mining.authorize") => {
                    info!("AUTHORIZE from {} with {params}", self.worker);

                    let authorize = serde_json::from_value::<Authorize>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.on_authorize(id, authorize).await?
                }

                (State::Working { .. }, "mining.submit") => {
                    info!("SUBMIT from {} with params {params}", self.worker);

                    let submit = serde_json::from_value::<Submit>(params)
                        .context(format!("failed to deserialize {method}"))?;

                    self.on_submit(id, submit).await?;

                    break;
                }
                (_state, method) => {
                    warn!("UNKNOWN method {method} with {params} from {}", self.worker);
                }
            }
        }

        Ok(())
    }

    async fn on_configure(&mut self, id: Id, configure: Configure) -> Result {
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

    async fn on_subscribe(&mut self, id: Id, subscribe: Subscribe) -> Result {
        let version_mask = match &self.state {
            State::Init => None,
            State::Configured { version_mask } => *version_mask,
            _ => bail!("SUBSCRIBE not allowed in current state"),
        };

        if let Some(extranonce1) = subscribe.extranonce1 {
            warn!("Ignoring extranonce1 suggestion: {extranonce1}");
        }

        let extranonce1 = "ffeeddcc".to_string();

        let result = SubscribeResult {
            subscriptions: vec![("mining.notify".to_string(), "todo".to_string())],
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

    async fn on_authorize(&mut self, id: Id, authorize: Authorize) -> Result {
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

        let job = Job::new(address, extranonce1.to_string(), *version_mask, self.gbt()?)?;

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

    async fn on_submit(&mut self, id: Id, submit: Submit) -> Result {
        let State::Working { job } = &self.state else {
            bail!("SUBMIT not allowed in current state");
        };

        let version = if let Some(version_bits) = submit.version_bits {
            let _version_mask = job.version_mask.unwrap(); // TODO
            assert!(version_bits != 0.into());
            // (header_version & !version_mask) | (bits & version_mask)
            // assert!((!version_mask & version_bits) != 0.into());

            job.version() | version_bits
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
            )?,
            time: submit.ntime.into(),
            bits: nbits.into(),
            nonce: submit.nonce.into(),
        };

        // TODO: check pool diff here
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

        // TODO: put this in tests
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

        info!("SUCCESS");

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
