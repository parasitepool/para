use {super::*, crate::subcommand::pool::pool_config::PoolConfig};

#[derive(Debug, Display)]
pub(crate) enum State {
    Init,
    Configured,
    Subscribed,
    Authorized,
    Working,
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
        let extranonce1 = "abcdef12".to_string();
        let gbt = self.gbt()?;
        let prevhash = PrevHash::from(gbt.previous_block_hash);
        let merkle_branches = stratum::merkle_branches(
            gbt.transactions
                .clone()
                .into_iter()
                .map(|r| r.txid)
                .collect(),
        );
        let version = Version(block::Version::from_consensus(
            gbt.version.try_into().unwrap(),
        ));

        let nbits = Nbits::from_str(&hex::encode(gbt.bits.clone()))?;
        let ntime = Ntime::try_from(gbt.current_time).expect("fits into u32 until ~2106");

        let mut coinb1_foo: Option<String> = None;
        let mut coinb2_foo: Option<String> = None;

        while let Some(message) = self.read_message().await? {
            let Message::Request { id, method, params } = message else {
                warn!(?message, "Ignoring non-request");
                continue;
            };

            match (&mut self.state, method.as_str()) {
                (State::Init, "mining.configure") => self.handle_configure(id, params).await?,
                (State::Init | State::Configured, "mining.subscribe") => {
                    self.handle_subscribe(id, params, extranonce1.clone())
                        .await?
                }
                (State::Subscribed, "mining.authorize") => {
                    self.handle_authorize(
                        id,
                        params,
                        extranonce1.clone(),
                        &gbt,
                        version,
                        ntime,
                        nbits,
                        prevhash.clone(),
                        merkle_branches.clone(),
                        &mut coinb1_foo,
                        &mut coinb2_foo,
                    )
                    .await?
                }

                (State::Working, "mining.submit") => {
                    self.handle_submit(
                        id,
                        params,
                        version,
                        nbits,
                        extranonce1.clone(),
                        &gbt,
                        &prevhash,
                        &merkle_branches,
                        &mut coinb1_foo,
                        &mut coinb2_foo,
                    )
                    .await?
                }
                (state, method) => {
                    warn!(
                        "Unhandled combination, state: {state}, method: {method}, params: {params}"
                    );
                }
            }
        }

        Ok(())
    }

    async fn handle_configure(&mut self, id: Id, params: Value) -> Result {
        info!("CONFIGURE from {} with {params}", self.worker);

        let configure = serde_json::from_value::<Configure>(params)?;

        if configure.version_rolling_mask.is_some() {
            let message = Message::Response {
                id,
                result: Some(
                    json!({"version-rolling": true, "version-rolling.mask": self.config.version_mask()}),
                ),
                error: None,
                reject_reason: None,
            };

            self.send(message).await?;
            self.state = State::Configured;
        } else {
            warn!("Unsupported extension {:?}", configure);
        }

        Ok(())
    }

    async fn handle_subscribe(&mut self, id: Id, params: Value, extranonce1: String) -> Result {
        info!("SUBSCRIBE from {} with {params}", self.worker);

        let subscribe = serde_json::from_value::<Subscribe>(params)?;

        if let Some(extranonce1) = subscribe.extranonce1 {
            warn!("Ignoring extranonce1 suggestion: {extranonce1}");
        }

        let result = SubscribeResult {
            subscriptions: vec![("mining.notify".into(), "todo".into())],
            extranonce1,
            extranonce2_size: EXTRANONCE2_SIZE.try_into().unwrap(),
        };

        self.send(Message::Response {
            id,
            result: Some(json!(result)),
            error: None,
            reject_reason: None,
        })
        .await?;

        self.state = State::Subscribed;

        Ok(())
    }

    async fn handle_authorize(
        &mut self,
        id: Id,
        params: Value,
        extranonce1: String,
        gbt: &GetBlockTemplateResult,
        version: Version,
        ntime: Ntime,
        nbits: Nbits,
        prevhash: PrevHash,
        merkle_branches: Vec<TxMerkleNode>,
        coinb1_foo: &mut Option<String>,
        coinb2_foo: &mut Option<String>,
    ) -> Result {
        info!("AUTHORIZE from {} with {params}", self.worker);

        let authorize = serde_json::from_value::<Authorize>(params)?;

        let address = Address::from_str(
            authorize
                .username
                .trim_matches('"')
                .split('.')
                .next()
                .ok_or_else(|| anyhow!("invalid username format"))?,
        )?
        .require_network(self.config.chain().network())
        .context(format!(
            "invalid username {} for worker {}",
            authorize.username, self.worker
        ))?;

        self.send(Message::Response {
            id,
            result: Some(json!(true)),
            error: None,
            reject_reason: None,
        })
        .await?;

        self.state = State::Authorized;

        info!("Sending set difficulty");

        self.send(Message::Notification {
            method: "mining.set_difficulty".into(),
            params: json!(SetDifficulty(Difficulty(1))),
        })
        .await?;

        let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
            address.clone(),
            extranonce1.clone(),
            EXTRANONCE2_SIZE,
            gbt.height,
            gbt.coinbase_value,
            gbt.default_witness_commitment.clone(),
        )
        .with_pool_sig("|parasite|".into())
        .build()?;

        *coinb1_foo = Some(coinb1.clone());
        *coinb2_foo = Some(coinb2.clone());

        let notify = Notify {
            job_id: "def123".into(), // TODO
            prevhash,
            coinb1,
            coinb2,
            merkle_branches,
            version,
            nbits,
            ntime,
            clean_jobs: true,
        };

        info!("Sending NOTIFY");

        self.send(Message::Notification {
            method: "mining.notify".into(),
            params: json!(notify),
        })
        .await?;

        self.state = State::Working;

        Ok(())
    }

    async fn handle_submit(
        &mut self,
        id: Id,
        params: Value,
        version: Version,
        nbits: Nbits,
        extranonce1: String,
        gbt: &GetBlockTemplateResult,
        prevhash: &PrevHash,
        merkle_branches: &[TxMerkleNode],
        coinb1_foo: &mut Option<String>,
        coinb2_foo: &mut Option<String>,
    ) -> Result {
        info!("SUBMIT from {} with params {}", self.worker, params);

        let submit = serde_json::from_value::<Submit>(params)?;

        let version = if let Some(version_bits) = submit.version_bits {
            assert!(version_bits != 0.into());
            // assert!((!self.config.version_mask() & version_bits) != 0.into());

            version | version_bits
        } else {
            version
        };

        let header = Header {
            version: version.into(),
            prev_blockhash: prevhash.clone().into(),
            merkle_root: stratum::merkle_root(
                &coinb1_foo.clone().unwrap(),
                &coinb2_foo.clone().unwrap(),
                &extranonce1,
                &submit.extranonce2,
                merkle_branches,
            )?,
            time: submit.ntime.into(),
            bits: nbits.into(),
            nonce: submit.nonce.into(),
        };

        let blockhash = header.validate_pow(Target::from_compact(nbits.into()))?;

        info!("Block with hash {blockhash} mets PoW");

        let coinbase_bin = hex::decode(format!(
            "{}{}{}{}",
            coinb1_foo.clone().unwrap(),
            extranonce1,
            submit.extranonce2,
            coinb2_foo.clone().unwrap()
        ))?;
        let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
        let coinbase_tx = bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

        let txdata = vec![coinbase_tx]
            .into_iter()
            .chain(
                gbt.clone()
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

        info!("submitting block solve");

        self.config.bitcoin_rpc_client()?.submit_block(&block)?;

        self.send(Message::Response {
            id,
            result: Some(json!(true)),
            error: None,
            reject_reason: None,
        })
        .await?;

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
        //
        //
        // TODO: make signet configurable
        // TODO: what other capabilities and rules are there?
        let params = json!({
            "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
            "rules": ["segwit", "signet"]
        });

        Ok(self
            .config
            .bitcoin_rpc_client()?
            // TODO: Use own GetBlockTemplateResult so I can deserialize directly from hex myself.
            // Use BTreeMap instead of HashMap
            .call::<GetBlockTemplateResult>("getblocktemplate", &[params])?)
    }
}
