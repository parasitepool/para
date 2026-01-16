use {
    super::*,
    bouncer::{Bouncer, Consequence},
    state::{Session, State},
    upstream::UpstreamSubmit,
};

pub(crate) use session::SessionSnapshot;

mod bouncer;
mod session;
mod state;

pub(crate) struct Stratifier<W: Workbase> {
    state: State,
    socket_addr: SocketAddr,
    settings: Arc<Settings>,
    metatron: Arc<Metatron>,
    upstream_tx: Option<mpsc::Sender<UpstreamSubmit>>,
    reader: FramedRead<OwnedReadHalf, LinesCodec>,
    writer: FramedWrite<OwnedWriteHalf, LinesCodec>,
    workbase_rx: watch::Receiver<Arc<W>>,
    cancel_token: CancellationToken,
    jobs: Jobs<W>,
    vardiff: Vardiff,
    bouncer: Bouncer,
    dropped_by_bouncer: bool,
}

impl<W: Workbase> Stratifier<W> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        socket_addr: SocketAddr,
        settings: Arc<Settings>,
        metatron: Arc<Metatron>,
        upstream_tx: Option<mpsc::Sender<UpstreamSubmit>>,
        tcp_stream: TcpStream,
        workbase_rx: watch::Receiver<Arc<W>>,
        cancel_token: CancellationToken,
    ) -> Self {
        let _ = tcp_stream.set_nodelay(true);

        let (reader, writer) = tcp_stream.into_split();

        let vardiff = Vardiff::new(
            settings.start_diff(),
            settings.vardiff_period(),
            settings.vardiff_window(),
            settings.min_diff(),
            settings.max_diff(),
        );

        let bouncer = Bouncer::new(settings.disable_bouncer());

        metatron.add_connection();

        Self {
            state: State::new(),
            socket_addr,
            settings,
            metatron,
            upstream_tx,
            reader: FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_MESSAGE_SIZE)),
            writer: FramedWrite::new(writer, LinesCodec::new()),
            workbase_rx,
            cancel_token,
            jobs: Jobs::new(),
            vardiff,
            bouncer,
            dropped_by_bouncer: false,
        }
    }

    pub(crate) async fn serve(&mut self) -> Result {
        let mut workbase_rx = self.workbase_rx.clone();
        let cancel_token = self.cancel_token.clone();
        let mut idle_check = tokio::time::interval(self.bouncer.check_interval());

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    info!("Disconnecting from {}", self.socket_addr);
                    break;
                }
                _ = idle_check.tick() => {
                    if self.bouncer.idle_check() == Consequence::Drop {
                        warn!(
                            "Dropping {} - idle for {}s",
                            self.socket_addr,
                            self.bouncer.last_interaction_since().as_secs()
                        );
                        self.dropped_by_bouncer = true;
                        break
                    }
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
                            debug!("CONFIGURE from {} with {params}", self.socket_addr);

                            let configure = serde_json::from_value::<Configure>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.configure(id, configure).await?
                        }
                        "mining.subscribe" => {
                            debug!("SUBSCRIBE from {} with {params}", self.socket_addr);

                            let subscribe = serde_json::from_value::<Subscribe>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.subscribe(id, subscribe).await?
                        }
                        "mining.authorize" => {
                            debug!("AUTHORIZE from {} with {params}", self.socket_addr);

                            let Some(subscription) = self.state.subscribed() else {
                                self.send_error(
                                    id.clone(),
                                    StratumError::MethodNotAllowed,
                                    Some(serde_json::json!({
                                        "method": "mining.authorize",
                                        "current_state": self.state.to_string()
                                    })),
                                )
                                .await?;
                                continue;
                            };

                            let authorize = serde_json::from_value::<Authorize>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            self.authorize(id, authorize, subscription.enonce1).await?
                        }
                        "mining.submit" => {
                            debug!("SUBMIT from {} with params {params}", self.socket_addr);

                            let Some(session) = self.state.working() else {
                                self.send_error(id.clone(), StratumError::Unauthorized, None)
                                    .await?;
                                continue;
                            };

                            let submit = serde_json::from_value::<Submit>(params)
                                .context(format!("failed to deserialize {method}"))?;

                            if self
                                .submit(id, submit, session)
                                .await?
                                == Consequence::Drop
                            {
                                self.dropped_by_bouncer = true;
                                break;
                            }
                        }
                        method => {
                            warn!("UNKNOWN method {method} with {params} from {}", self.socket_addr);
                        }
                    }
                }

                changed = workbase_rx.changed() => {
                    if changed.is_err() {
                        warn!("Template receiver dropped, closing connection with {}", self.socket_addr);
                        break;
                    }

                    if let Some(session)= self.state.working() {
                        let workbase = workbase_rx.borrow_and_update().clone();
                        self.workbase_update(workbase, session).await?;
                    } else {
                        let _ = workbase_rx.borrow_and_update();
                        continue;
                    };

                }
            }
        }

        Ok(())
    }

    async fn handle_consequence(
        &mut self,
        consequence: Consequence,
        address: &Address,
        enonce1: &Extranonce,
    ) {
        match consequence {
            Consequence::None => {}
            Consequence::Warn => {
                info!(
                    "Warning {} - {} consecutive rejects for {}s, sending fresh job",
                    self.socket_addr,
                    self.bouncer.consecutive_rejects(),
                    self.bouncer
                        .reject_duration()
                        .map(|duration| duration.as_secs())
                        .unwrap_or(0)
                );

                let workbase = self.workbase_rx.borrow().clone();

                match workbase.create_job(
                    enonce1,
                    self.metatron.enonce2_size(),
                    Some(address),
                    self.jobs.next_id(),
                    self.state.version_mask(),
                ) {
                    Ok(job) => {
                        let new_job = Arc::new(job);
                        let clean_jobs = self.jobs.insert(new_job.clone());

                        if let Ok(notify) = new_job.notify(clean_jobs) {
                            let _ = self
                                .send(Message::Notification {
                                    method: "mining.notify".into(),
                                    params: json!(notify),
                                })
                                .await;
                        }
                    }
                    Err(err) => {
                        warn!("Failed to create job: {err}");
                    }
                }
            }
            Consequence::Reconnect => {
                info!(
                    "Suggesting reconnect to {} - {} consecutive rejects for {}s",
                    self.socket_addr,
                    self.bouncer.consecutive_rejects(),
                    self.bouncer
                        .reject_duration()
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                );
                let _ = self
                    .send(Message::Notification {
                        method: "client.reconnect".into(),
                        params: json!([]),
                    })
                    .await;
            }
            Consequence::Drop => {
                warn!(
                    "Dropping {} - {} consecutive rejects for {}s",
                    self.socket_addr,
                    self.bouncer.consecutive_rejects(),
                    self.bouncer
                        .reject_duration()
                        .map(|d| d.as_secs())
                        .unwrap_or(0)
                );
            }
        }
    }

    async fn workbase_update(&mut self, workbase: Arc<W>, session: Arc<Session>) -> Result {
        let new_job = Arc::new(
            workbase
                .create_job(
                    &session.enonce1,
                    self.metatron.enonce2_size(),
                    Some(&session.address),
                    self.jobs.next_id(),
                    self.state.version_mask(),
                )
                .context("failed to create job for template update")?,
        );

        let clean_jobs = self.jobs.insert(new_job.clone());

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
            let version_mask = self.settings.version_mask();

            if !self.state.configure(version_mask) {
                self.send_error(
                    id,
                    StratumError::MethodNotAllowed,
                    Some(serde_json::json!({
                        "method": "mining.configure",
                        "current_state": self.state.to_string()
                    })),
                )
                .await?;

                return Ok(());
            }

            debug!(
                "Configuring version rolling for {} with version mask {version_mask}",
                self.socket_addr
            );

            let message = Message::Response {
                id,
                result: Some(
                    json!({"version-rolling": true, "version-rolling.mask": self.settings.version_mask()}),
                ),
                error: None,
                reject_reason: None,
            };

            self.send(message).await?;
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
        }

        Ok(())
    }

    async fn subscribe(&mut self, id: Id, subscribe: Subscribe) -> Result {
        if !self.state.not_subscribed() {
            info!("Client {} resubscribing", self.socket_addr);
            self.jobs = Jobs::<W>::new();
            self.vardiff = Vardiff::new(
                self.settings.start_diff(),
                self.settings.vardiff_period(),
                self.settings.vardiff_window(),
                self.settings.min_diff(),
                self.settings.max_diff(),
            );
        }

        let (enonce1, enonce2_size) = if let Some(ref requested_enonce1) = subscribe.enonce1 {
            let enonce1 = if let Some(session) = self.metatron.take_session(requested_enonce1) {
                info!("Resuming session for enonce1 {}", session.enonce1);
                session.enonce1
            } else {
                self.metatron.next_enonce1()
            };

            (enonce1, self.metatron.enonce2_size())
        } else {
            (self.metatron.next_enonce1(), self.metatron.enonce2_size())
        };

        let subscriptions = vec![
            (
                "mining.set_difficulty".to_string(),
                SUBSCRIPTION_ID.to_string(),
            ),
            ("mining.notify".to_string(), SUBSCRIPTION_ID.to_string()),
        ];

        let result = SubscribeResult {
            subscriptions,
            enonce1: enonce1.clone(),
            enonce2_size,
        };

        self.send(Message::Response {
            id,
            result: Some(json!(result)),
            error: None,
            reject_reason: None,
        })
        .await?;

        self.state.subscribe(enonce1, subscribe.user_agent);

        Ok(())
    }

    async fn authorize(&mut self, id: Id, authorize: Authorize, enonce1: Extranonce) -> Result {
        let address = match authorize
            .username
            .parse_with_network(self.settings.chain().network())
        {
            Ok(parsed) => parsed,
            Err(e) => {
                self.send_error(
                    id,
                    StratumError::Unauthorized,
                    Some(json!({
                        "message": e.to_string(),
                        "username": authorize.username.as_str(),
                    })),
                )
                .await?;

                return Ok(());
            }
        };

        let workbase = self.workbase_rx.borrow().clone();

        let job = Arc::new(
            workbase
                .create_job(
                    &enonce1,
                    self.metatron.enonce2_size(),
                    Some(&address),
                    self.jobs.next_id(),
                    self.state.version_mask(),
                )
                .context("failed to create job for authorize")?,
        );

        self.send(Message::Response {
            id: id.clone(),
            result: Some(json!(true)),
            error: None,
            reject_reason: None,
        })
        .await?;

        let workername = authorize.username.workername().to_string();
        // TODO
        if !self
            .state
            .authorize(address, workername, authorize.username)
        {
            self.send_error(
                id.clone(),
                StratumError::MethodNotAllowed,
                Some(serde_json::json!({
                    "method": "mining.authorize",
                    "current_state": self.state.to_string()
                })),
            )
            .await?;

            return Ok(());
        }

        self.bouncer.authorize();

        debug!("Sending SET DIFFICULTY");

        self.send(Message::Notification {
            method: "mining.set_difficulty".into(),
            params: json!(SetDifficulty(self.vardiff.current_diff())),
        })
        .await?;

        debug!("Sending NOTIFY");

        let clean_jobs = self.jobs.insert(job.clone());

        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(job.notify(clean_jobs)?),
        })
        .await?;

        Ok(())
    }

    async fn submit(
        &mut self,
        id: Id,
        submit: Submit,
        session: Arc<Session>,
    ) -> Result<Consequence> {
        let worker = self
            .metatron
            .get_or_create_worker(session.address.clone(), &session.workername);

        if submit.username != session.username {
            self.send_error(
                id,
                StratumError::WorkerMismatch,
                Some(json!({
                    "authorized": session.username.as_str(),
                    "submitted": submit.username.as_str(),
                })),
            )
            .await?;

            worker.record_rejected();

            let consequence = self.bouncer.reject();
            self.handle_consequence(consequence, &session.address, &session.enonce1)
                .await;

            return Ok(consequence);
        }

        let Some(job) = self.jobs.get(&submit.job_id) else {
            self.send_error(id, StratumError::Stale, None).await?;

            worker.record_rejected();

            let consequence = self.bouncer.reject();
            self.handle_consequence(consequence, &session.address, &session.enonce1)
                .await;

            return Ok(consequence);
        };

        let expected_extranonce2_size = self.metatron.enonce2_size();

        if submit.enonce2.len() != expected_extranonce2_size {
            warn!(
                "Invalid extranonce2 length from {}: got {} bytes, expected {}",
                self.socket_addr,
                submit.enonce2.len(),
                expected_extranonce2_size
            );

            self.send_error(
                id,
                StratumError::InvalidNonce2Length,
                Some(json!({
                    "expected": expected_extranonce2_size,
                    "received": submit.enonce2.len()
                })),
            )
            .await?;

            worker.record_rejected();

            let consequence = self.bouncer.reject();
            self.handle_consequence(consequence, &session.address, &session.enonce1)
                .await;

            return Ok(consequence);
        }

        let job_ntime = job.ntime().0;
        let submit_ntime = submit.ntime.0;
        if submit_ntime < job_ntime || submit_ntime > job_ntime + MAX_NTIME_OFFSET {
            self.send_error(
                id,
                StratumError::NtimeOutOfRange,
                Some(json!({
                    "job_ntime": job_ntime,
                    "submit_ntime": submit_ntime,
                    "max_ntime": job_ntime + MAX_NTIME_OFFSET,
                })),
            )
            .await?;

            worker.record_rejected();

            let consequence = self.bouncer.reject();
            self.handle_consequence(consequence, &session.address, &session.enonce1)
                .await;

            return Ok(consequence);
        }

        let version = if let Some(version_bits) = submit.version_bits {
            let Some(version_mask) = job.version_mask else {
                self.send_error(
                    id,
                    StratumError::InvalidVersionMask,
                    Some(serde_json::json!({"reason": "Version rolling not negotiated"})),
                )
                .await?;

                worker.record_rejected();

                let consequence = self.bouncer.reject();
                self.handle_consequence(consequence, &session.address, &session.enonce1)
                    .await;

                return Ok(consequence);
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
                &job.enonce1,
                &submit.enonce2,
                job.merkle_branches(),
            )?
            .into(),
            time: submit.ntime.into(),
            bits: nbits.to_compact(),
            nonce: submit.nonce.into(),
        };

        let hash = header.block_hash();

        if self.jobs.is_duplicate(hash) {
            self.send_error(id, StratumError::Duplicate, None).await?;

            worker.record_rejected();

            let consequence = self.bouncer.reject();
            self.handle_consequence(consequence, &session.address, &session.enonce1)
                .await;

            return Ok(consequence);
        }

        if let Ok(blockhash) = header.validate_pow(Target::from_compact(nbits.into())) {
            info!("Block with hash {blockhash} meets network difficulty");

            match job.workbase.build_block(&job, &submit, header) {
                Ok(block) => {
                    info!("Submitting potential block solve");

                    match self.settings.bitcoin_rpc_client()?.submit_block(&block) {
                        Ok(_) => {
                            info!("SUCCESSFULLY mined block {}", block.block_hash());
                            self.metatron.add_block();
                        }
                        Err(err) => error!("Failed to submit block: {err}"),
                    }
                }
                Err(err) => {
                    warn!("Failed to build block: {err}");
                }
            }
        }

        let current_diff = self.vardiff.current_diff();

        if current_diff.to_target().is_met_by(hash) {
            self.send(Message::Response {
                id,
                result: Some(json!(true)),
                error: None,
                reject_reason: None,
            })
            .await?;

            let share_diff = Difficulty::from(hash);

            worker.record_accepted(current_diff, share_diff);

            self.submit_to_upstream(&submit, share_diff, &session.enonce1);

            self.bouncer.accept();

            let network_diff = Difficulty::from(job.nbits());

            debug!(
                "Share accepted from {} | diff={} dsps={:.4} shares_since_change={}",
                self.socket_addr,
                current_diff,
                self.vardiff.dsps(),
                self.vardiff.shares_since_change()
            );

            if let Some(new_diff) = self.vardiff.record_share(current_diff, network_diff) {
                debug!(
                    "Adjusting difficulty {} -> {} for {} | dsps={:.4} period={}s",
                    current_diff,
                    new_diff,
                    self.socket_addr,
                    self.vardiff.dsps(),
                    self.settings.vardiff_period().as_secs_f64()
                );

                self.send(Message::Notification {
                    method: "mining.set_difficulty".into(),
                    params: json!(SetDifficulty(new_diff)),
                })
                .await?;
            }

            return Ok(Consequence::None);
        }

        self.send_error(id, StratumError::AboveTarget, None).await?;

        worker.record_rejected();

        let consequence = self.bouncer.reject();
        self.handle_consequence(consequence, &session.address, &session.enonce1)
            .await;

        Ok(consequence)
    }

    fn submit_to_upstream(&self, submit: &Submit, share_diff: Difficulty, enonce1: &Extranonce) {
        let Some(ref tx) = self.upstream_tx else {
            return;
        };

        let enonce2 = match self.metatron.extranonces() {
            Extranonces::Pool(_) => submit.enonce2.clone(),
            Extranonces::Proxy(proxy) => {
                proxy.reconstruct_enonce2_for_upstream(enonce1, &submit.enonce2)
            }
        };

        let upstream_submit = UpstreamSubmit {
            job_id: submit.job_id,
            enonce2,
            nonce: submit.nonce,
            ntime: submit.ntime,
            version_bits: submit.version_bits,
            share_diff,
        };

        if tx.try_send(upstream_submit).is_err() {
            warn!("Upstream channel full, dropping share submission");
        }
    }

    async fn read_message(&mut self) -> Result<Option<Message>> {
        match self.reader.next().await {
            Some(Ok(line)) => {
                let message = serde_json::from_str::<Message>(&line).map_err(|e| {
                    anyhow!(
                        "invalid stratum message from {}: {e}; line={line:?}",
                        self.socket_addr
                    )
                })?;
                Ok(Some(message))
            }
            Some(Err(e)) => Err(anyhow!("read error from {}: {e}", self.socket_addr)),
            None => {
                info!("Connection {} disconnected", self.socket_addr);
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

impl<W: Workbase> Drop for Stratifier<W> {
    fn drop(&mut self) {
        if !self.dropped_by_bouncer
            && let Some(session) = self.state.working()
        {
            self.metatron
                .store_session(SessionSnapshot::new(session.enonce1.clone())); // TODO
        }

        self.metatron.sub_connection();

        info!(
            "Shutting down stratifier for {} (remaining: {})",
            self.socket_addr,
            self.metatron.total_connections()
        );
    }
}
