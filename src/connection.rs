use {super::*, subcommand::pool::pool_config::ResolvedPoolConfig};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum State {
    Init,
    Configured,
    Subscribed,
    Working,
}

pub(crate) struct Connection<R, W> {
    config: Arc<ResolvedPoolConfig>,
    metatron: Arc<Metatron>,
    worker: SocketAddr,
    reader: FramedRead<R, LinesCodec>,
    writer: FramedWrite<W, LinesCodec>,
    workbase_receiver: watch::Receiver<Arc<Workbase>>,
    cancel_token: CancellationToken,
    jobs: Jobs,
    state: State,
    address: Option<Address>,
    authorized: Option<SystemTime>,
    version_mask: Option<Version>,
    extranonce1: Option<Extranonce>,
    user_agent: Option<String>,
    vardiff: Vardiff,
}

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(crate) fn new(
        config: Arc<ResolvedPoolConfig>,
        metatron: Arc<Metatron>,
        worker: SocketAddr,
        reader: R,
        writer: W,
        workbase_receiver: watch::Receiver<Arc<Workbase>>,
        cancel_token: CancellationToken,
    ) -> Self {
        let vardiff = Vardiff::new(
            config.start_diff(),
            config.vardiff_period(),
            config.vardiff_window(),
        );

        metatron.add_worker();

        Self {
            config,
            metatron,
            worker,
            reader: FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE)),
            writer: FramedWrite::new(writer, LinesCodec::new()),
            workbase_receiver,
            cancel_token,
            jobs: Jobs::new(),
            state: State::Init,
            address: None,
            authorized: None,
            version_mask: None,
            extranonce1: None,
            user_agent: None,
            vardiff,
        }
    }

    pub(crate) async fn serve(&mut self) -> Result {
        let mut workbase_receiver = self.workbase_receiver.clone();
        let cancel_token = self.cancel_token.clone();

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Disconnecting from {}", self.worker);
                    break;
                }
                message = self.read_message() => {
                    let Some(message) = message? else {
                        break;
                    };

                    let Message::Request { id, method, params } = message else {
                        warn!(?message, "Ignoring any notifications or responses from workers");
                        continue;
                    };

                    match method.as_str() {
                        "mining.configure" => {
                            debug!("CONFIGURE from {} with {params}", self.worker);

                            if !matches!(self.state,  State::Init | State::Configured) {
                                self.send_error(
                                    id.clone(),
                                    StratumError::MethodNotAllowed,
                                    Some(serde_json::json!({
                                        "method": "mining.configure",
                                        "current_state": format!("{:?}", self.state)
                                    })),
                                )
                                .await?;
                                continue;
                            };

                            let configure = serde_json::from_value::<Configure>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.configure(id, configure).await?
                        }
                        "mining.subscribe" => {
                            debug!("SUBSCRIBE from {} with {params}", self.worker);

                            if !matches!(self.state,  State::Init | State::Configured) {
                                self.send_error(
                                    id.clone(),
                                    StratumError::MethodNotAllowed,
                                    Some(serde_json::json!({
                                        "method": "mining.subscribe",
                                        "current_state": format!("{:?}", self.state)
                                    })),
                                )
                                .await?;
                                continue;
                            };

                            let subscribe = serde_json::from_value::<Subscribe>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.subscribe(id, subscribe).await?
                        }
                        "mining.authorize" => {
                            debug!("AUTHORIZE from {} with {params}", self.worker);

                            if self.state != State::Subscribed {
                                self.send_error(
                                    id.clone(),
                                    StratumError::MethodNotAllowed,
                                    Some(serde_json::json!({
                                        "method": "mining.authorize",
                                        "current_state": format!("{:?}", self.state)
                                    })),
                                )
                                .await?;
                                continue;
                            }

                            let authorize = serde_json::from_value::<Authorize>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.authorize(id, authorize).await?
                        }
                        "mining.submit" => {
                            debug!("SUBMIT from {} with params {params}", self.worker);

                            if self.state != State::Working {
                                self.send_error(id.clone(), StratumError::Unauthorized, None)
                                    .await?;
                                continue;
                            }

                            let submit = serde_json::from_value::<Submit>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.submit(id, submit).await?;
                        }
                        method => {
                            warn!("UNKNOWN method {method} with {params} from {}", self.worker);
                        }
                    }
                }

                changed = workbase_receiver.changed() => {
                    if changed.is_err() {
                        warn!("Template receiver dropped, closing connection with {}", self.worker);
                        break;
                    }

                    if self.state != State::Working {
                        let _ = workbase_receiver.borrow_and_update();
                        continue;
                    };

                    let workbase = workbase_receiver.borrow_and_update().clone();
                    self.workbase_update(workbase).await?;
                }
            }
        }

        Ok(())
    }

    async fn workbase_update(&mut self, workbase: Arc<Workbase>) -> Result {
        let (address, extranonce1) = match (&self.address, &self.extranonce1) {
            (Some(a), Some(e)) => (a.clone(), e.clone()),
            _ => return Ok(()),
        };

        let new_job = Arc::new(Job::new(
            address,
            extranonce1,
            self.version_mask,
            workbase,
            self.jobs.next_id(),
        )?);

        let clean_jobs = self.jobs.upsert(new_job.clone());

        debug!("Template updated sending NOTIFY");

        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(new_job.notify(clean_jobs)?),
        })
        .await?;

        Ok(())
    }

    async fn configure(&mut self, id: Id, configure: Configure) -> Result {
        if configure.version_rolling_mask.is_some() {
            let version_mask = self.config.version_mask();
            debug!(
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
                error: Some(StratumError::UnsupportedExtension.into_response(Some(
                    serde_json::json!({
                        "extensions": configure.extensions,
                        "supported": ["version-rolling"]
                    }),
                ))),
                reject_reason: None,
            };

            self.send(message).await?;
            self.state = State::Init;
        }

        Ok(())
    }

    async fn subscribe(&mut self, id: Id, subscribe: Subscribe) -> Result {
        if let Some(extranonce1) = subscribe.extranonce1 {
            warn!("Ignoring worker extranonce1 suggestion: {extranonce1}");
        }

        let extranonce1 = Extranonce::random(EXTRANONCE1_SIZE);

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
            extranonce2_size: EXTRANONCE2_SIZE,
        };

        self.send(Message::Response {
            id,
            result: Some(json!(result)),
            error: None,
            reject_reason: None,
        })
        .await?;

        self.extranonce1 = Some(extranonce1.clone());
        self.user_agent = Some(subscribe.user_agent);
        self.state = State::Subscribed;

        Ok(())
    }

    async fn authorize(&mut self, id: Id, authorize: Authorize) -> Result {
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

        let job = Arc::new(Job::new(
            address.clone(),
            extranonce1.clone(),
            self.version_mask,
            self.workbase_receiver.borrow().clone(),
            self.jobs.next_id(),
        )?);

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

        debug!("Sending SET DIFFICULTY");

        self.send(Message::Notification {
            method: "mining.set_difficulty".into(),
            params: json!(SetDifficulty(self.vardiff.current_diff())),
        })
        .await?;

        debug!("Sending NOTIFY");

        let clean_jobs = self.jobs.upsert(job.clone());

        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(job.notify(clean_jobs)?),
        })
        .await?;

        self.state = State::Working;

        Ok(())
    }

    async fn submit(&mut self, id: Id, submit: Submit) -> Result {
        let Some(job) = self.jobs.get(&submit.job_id) else {
            self.send_error(id, StratumError::Stale, None).await?;
            self.metatron.add_rejected();

            return Ok(());
        };

        let version = if let Some(version_bits) = submit.version_bits {
            let Some(version_mask) = job.version_mask else {
                self.send_error(
                    id,
                    StratumError::InvalidVersionMask,
                    Some(serde_json::json!({"reason": "Version rolling not negotiated"})),
                )
                .await?;

                self.metatron.add_rejected();

                return Ok(());
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
                job.workbase.merkle_branches(),
            )?
            .into(),
            time: submit.ntime.into(),
            bits: nbits.to_compact(),
            nonce: submit.nonce.into(),
        };

        if self.jobs.is_duplicate(header.block_hash()) {
            self.send_error(id, StratumError::Duplicate, None).await?;
            self.metatron.add_rejected();

            return Ok(());
        }

        if let Ok(blockhash) = header.validate_pow(Target::from_compact(nbits.into())) {
            info!("Block with hash {blockhash} meets network difficulty");

            let coinbase_bin = hex::decode(format!(
                "{}{}{}{}",
                job.coinb1, job.extranonce1, submit.extranonce2, job.coinb2,
            ))?;

            let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
            let coinbase_tx =
                bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

            let txdata = std::iter::once(coinbase_tx)
                .chain(
                    job.workbase
                        .template()
                        .transactions
                        .iter()
                        .map(|tx| tx.transaction.clone())
                        .collect::<Vec<Transaction>>(),
                )
                .collect();

            let block = Block { header, txdata };

            if job.workbase.template().height > 16 {
                assert!(block.bip34_block_height().is_ok());
            }

            info!("Submitting potential block solve");

            match self.config.bitcoin_rpc_client()?.submit_block(&block) {
                Ok(_) => {
                    info!("SUCCESSFULLY mined block {}", block.block_hash());
                    self.metatron.add_block();
                }
                Err(err) => error!("Failed to submit block: {err}"),
            }
        }

        let current_diff = self.vardiff.current_diff();

        if current_diff.to_target().is_met_by(header.block_hash()) {
            self.metatron.add_accepted(current_diff.as_f64());

            self.send(Message::Response {
                id,
                result: Some(json!(true)),
                error: None,
                reject_reason: None,
            })
            .await?;

            let network_diff = Difficulty::from(job.nbits());

            debug!(
                "Share accepted from {} | diff={} dsps={:.4} shares_since_change={}",
                self.worker,
                current_diff,
                self.vardiff.dsps(),
                self.vardiff.shares_since_change()
            );

            if let Some(new_diff) = self.vardiff.record_share(current_diff, network_diff) {
                debug!(
                    "Adjusting difficulty {} -> {} for {} | dsps={:.4} period={}s",
                    current_diff,
                    new_diff,
                    self.worker,
                    self.vardiff.dsps(),
                    self.config.vardiff_period().as_secs_f64()
                );

                self.send(Message::Notification {
                    method: "mining.set_difficulty".into(),
                    params: json!(SetDifficulty(new_diff)),
                })
                .await?;
            }
        } else {
            self.metatron.add_rejected();
            self.send_error(id, StratumError::AboveTarget, None).await?;
        }

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

    async fn send_error(
        &mut self,
        id: Id,
        error: StratumError,
        traceback: Option<serde_json::Value>,
    ) -> Result {
        self.send(Message::Response {
            id,
            result: None,
            error: Some(error.into_response(traceback)),
            reject_reason: None,
        })
        .await
    }
}

impl<R, W> Drop for Connection<R, W> {
    fn drop(&mut self) {
        self.metatron.sub_worker();
        info!(
            "Worker {} disconnected (remaining: {})",
            self.worker,
            self.metatron.total_workers()
        );
    }
}
