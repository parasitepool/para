use super::*;

pub struct Bitcoind {
    pub datadir: Option<PathBuf>,
    pub handle: Option<Child>,
    pub network: Network,
    pub rpc_port: u16,
    pub zmq_port: u16,
    pub rpc_user: String,
    pub rpc_password: String,
    pub with_output: bool,
    _tempdir: Option<Arc<TempDir>>,
}

impl Bitcoind {
    pub async fn connect(
        rpc_port: u16,
        rpc_user: String,
        rpc_password: String,
        zmq_port: u16,
        network: Network,
        with_output: bool,
    ) -> Result<Self> {
        let bitcoind = Self {
            datadir: None,
            handle: None,
            network,
            rpc_port,
            zmq_port,
            rpc_user,
            rpc_password,
            with_output,
            _tempdir: None,
        };

        let info = bitcoind.client()?.get_blockchain_info().await?;

        println!(
            "Connected to bitcoind: chain={}, blocks={}",
            info.chain, info.blocks
        );

        Ok(bitcoind)
    }

    pub fn spawn(
        tempdir: Arc<TempDir>,
        bitcoind_port: u16,
        rpc_port: u16,
        zmq_port: u16,
        with_output: bool,
        network: Network,
    ) -> Result<Self> {
        let bitcoind_data_dir = tempdir.path().join("bitcoin");
        fs::create_dir_all(&bitcoind_data_dir)?;

        let bitcoind_conf = bitcoind_data_dir.join("bitcoin.conf");

        let rpc_user = "satoshi".to_string();
        let rpc_password = "nakamoto".to_string();
        let rpcauth = Self::generate_rpcauth(rpc_user.as_str(), rpc_password.as_str(), None);

        let (network_flag, section, extra) = match network {
            Network::Signet => ("signet=1", "[signet]", "signetchallenge=51\n\n"),
            Network::Regtest => ("regtest=1", "[regtest]", ""),
            _ => bail!("unsupported network: {network}"),
        };

        fs::write(
            &bitcoind_conf,
            format!(
                "\
{network_flag}
datadir={}

{section}
{extra}
server=1
txindex=1
zmqpubhashblock=tcp://127.0.0.1:{zmq_port}

port={bitcoind_port}

rpcbind=127.0.0.1
rpcport={rpc_port}
rpcallowip=127.0.0.1
rpcauth={rpcauth}

maxtxfee=1000000
maxconnections=256
maxmempool=2048
minrelaytxfee=0.00001
",
                &bitcoind_data_dir.display(),
            ),
        )?;

        let compiled_bitcoind = format!("{}/bitcoin/build/bin", workspace_root());
        let expanded_path = format!("{compiled_bitcoind}:{}", std::env::var("PATH")?);

        let mut handle = Command::new("bitcoind")
            .env("PATH", &expanded_path)
            .arg(format!("-conf={}", bitcoind_conf.display()))
            .stdout(if with_output {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .spawn()?;

        let rpc_status = Command::new("bitcoin-cli")
            .env("PATH", &expanded_path)
            .args([
                &format!("-conf={}", bitcoind_conf.display()),
                "-rpcwait",
                "-rpcwaittimeout=30",
                "getblockchaininfo",
            ])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status()?;

        if !rpc_status.success() {
            let exited = handle.try_wait()?;
            let stderr = handle
                .stderr
                .take()
                .map(|mut s| {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut s, &mut buf).ok();
                    buf
                })
                .unwrap_or_default();

            let _ = handle.kill();
            let _ = handle.wait();

            if let Some(exit_status) = exited {
                bail!("bitcoind exited early with {exit_status}.\nstderr:\n{stderr}");
            } else {
                bail!(
                    "Failed to connect bitcoind RPC after 30 seconds (process still running).\nstderr:\n{stderr}"
                );
            }
        }

        Ok(Self {
            datadir: Some(bitcoind_data_dir),
            handle: Some(handle),
            network,
            rpc_port,
            zmq_port,
            rpc_user,
            rpc_password,
            with_output,
            _tempdir: Some(tempdir),
        })
    }

    pub fn spawn_no_listen(
        tempdir: Arc<TempDir>,
        rpc_port: u16,
        zmq_port: u16,
        with_output: bool,
        network: Network,
    ) -> Result<Self> {
        let bitcoind_data_dir = tempdir.path().join("bitcoin");
        fs::create_dir_all(&bitcoind_data_dir)?;

        let bitcoind_conf = bitcoind_data_dir.join("bitcoin.conf");

        let rpc_user = "satoshi".to_string();
        let rpc_password = "nakamoto".to_string();
        let rpcauth = Self::generate_rpcauth(rpc_user.as_str(), rpc_password.as_str(), None);

        let (network_flag, section, extra) = match network {
            Network::Signet => ("signet=1", "[signet]", "signetchallenge=51\n\n"),
            Network::Regtest => ("regtest=1", "[regtest]", ""),
            _ => bail!("unsupported network: {network}"),
        };

        fs::write(
            &bitcoind_conf,
            format!(
                "\
{network_flag}
datadir={}

{section}
{extra}
server=1
txindex=1
zmqpubhashblock=tcp://127.0.0.1:{zmq_port}

listen=0

rpcbind=127.0.0.1
rpcport={rpc_port}
rpcallowip=127.0.0.1
rpcauth={rpcauth}

maxtxfee=1000000
maxconnections=256
maxmempool=2048
minrelaytxfee=0.00001
",
                &bitcoind_data_dir.display(),
            ),
        )?;

        let compiled_bitcoind = format!("{}/bitcoin/build/bin", workspace_root());
        let expanded_path = format!("{compiled_bitcoind}:{}", std::env::var("PATH")?);

        let mut handle = Command::new("bitcoind")
            .env("PATH", &expanded_path)
            .arg(format!("-conf={}", bitcoind_conf.display()))
            .stdout(if with_output {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped())
            .spawn()?;

        let rpc_status = Command::new("bitcoin-cli")
            .env("PATH", &expanded_path)
            .args([
                &format!("-conf={}", bitcoind_conf.display()),
                "-rpcwait",
                "-rpcwaittimeout=30",
                "getblockchaininfo",
            ])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .status()?;

        if !rpc_status.success() {
            let exited = handle.try_wait()?;
            let stderr = handle
                .stderr
                .take()
                .map(|mut s| {
                    let mut buf = String::new();
                    std::io::Read::read_to_string(&mut s, &mut buf).ok();
                    buf
                })
                .unwrap_or_default();

            let _ = handle.kill();
            let _ = handle.wait();

            if let Some(exit_status) = exited {
                bail!("bitcoind exited early with {exit_status}.\nstderr:\n{stderr}");
            } else {
                bail!(
                    "Failed to connect bitcoind RPC after 30 seconds (process still running).\nstderr:\n{stderr}"
                );
            }
        }

        Ok(Self {
            datadir: Some(bitcoind_data_dir),
            handle: Some(handle),
            network,
            rpc_port,
            zmq_port,
            rpc_user,
            rpc_password,
            with_output,
            _tempdir: Some(tempdir),
        })
    }

    pub fn generate_rpcauth(username: &str, password: &str, salt_overide: Option<&str>) -> String {
        let salt = if let Some(salt_overide) = salt_overide {
            salt_overide.to_string()
        } else {
            let mut salt_bytes = [0u8; 16];
            rng().fill_bytes(&mut salt_bytes);
            salt_bytes
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>()
        };

        let mut engine = hmac::HmacEngine::<sha256::Hash>::new(salt.as_bytes());
        engine.input(password.as_bytes());
        let password_hmac = Hmac::<sha256::Hash>::from_engine(engine);

        format!("{}:{}${}", username, salt, password_hmac)
    }

    pub fn client(&self) -> Result<Client> {
        Ok(Client::new(
            format!("http://127.0.0.1:{}", self.rpc_port),
            Auth::UserPass(self.rpc_user.clone(), self.rpc_password.clone()),
            None,
            None,
            None,
        )?)
    }

    pub async fn create_or_load_wallet(&self) -> Result {
        match self
            .client()?
            .call_raw::<serde_json::Value>("createwallet", &[json!("testing-harness")])
            .await
        {
            Ok(_) => {}
            Err(_err) => {}
        }

        Ok(())
    }

    /// Submit a pre-mined block to advance the chain by one block.
    /// This block was mined against the custom signet genesis (signetchallenge=51)
    /// and is valid for any fresh instance of that chain.
    pub async fn submit_premined_block(&self) -> Result<()> {
        const SIGNET_BLOCK_1: &str = "0020872af61eee3b63a380a477a063af32b2bbc97c9ff9f01f2c4225e973988108000000e809b8decabf0a7ea3347543028303e07e6a2cb3e48b8c92731674fc24259f2742b1cd69ae77031e7e0d030001020000000001010000000000000000000000000000000000000000000000000000000000000000ffffffff265100b0207a4b00000000000000577c70617261736974657c45b1cd69000000007c706172617cffffffff0200f2052a010000002200204ae81572f06e1b88fd5ced7a1a000945432e83e1551e6f721ee9c00b8cc332600000000000000000266a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf90120000000000000000000000000000000000000000000000000000000000000000000000000";

        match self
            .client()?
            .call_raw::<serde_json::Value>("submitblock", &[json!(SIGNET_BLOCK_1)])
            .await
        {
            Ok(result) => assert!(result.is_null(), "submitblock rejected: {result}"),
            Err(e) => {
                // Check if the block was actually accepted despite the parse error
                let count = self.client()?.get_block_count().await?;
                assert!(count > 0, "submitblock failed and block not accepted: {e}");
            }
        }

        Ok(())
    }

    pub fn op_true_address(&self) -> Address {
        let op_true: ScriptBuf = Builder::new().push_opcode(OP_TRUE).into_script();
        Address::p2wsh(&op_true, self.network)
    }

    pub async fn get_spendable_utxos(&self) -> Result<Vec<(OutPoint, Amount)>> {
        let descriptor = format!("addr({})", self.op_true_address());

        #[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
        #[serde(rename_all = "camelCase")]
        pub struct Utxo {
            pub txid: bitcoin::Txid,
            pub vout: u32,
            pub script_pub_key: bitcoin::ScriptBuf,
            #[serde(rename = "desc")]
            pub descriptor: String,
            #[serde(with = "bitcoin::amount::serde::as_btc")]
            pub amount: bitcoin::Amount,
            pub height: u64,
            pub coinbase: bool,
        }

        #[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
        pub struct ScanTxOutResult {
            pub success: Option<bool>,
            #[serde(rename = "txouts")]
            pub tx_outs: Option<u64>,
            pub height: Option<u64>,
            #[serde(rename = "bestblock")]
            pub best_block_hash: Option<bitcoin::BlockHash>,
            pub unspents: Vec<Utxo>,
            #[serde(with = "bitcoin::amount::serde::as_btc")]
            pub total_amount: bitcoin::Amount,
        }

        let result: ScanTxOutResult = self
            .client()?
            .call_raw("scantxoutset", &[json!("start"), json!([descriptor])])
            .await?;

        let block_count = self.client()?.get_block_count().await?;

        let mut outpoints = Vec::new();
        for utxo in result.unspents {
            if !utxo.coinbase || block_count - utxo.height >= 100 {
                outpoints.push((
                    OutPoint {
                        txid: utxo.txid,
                        vout: utxo.vout,
                    },
                    utxo.amount,
                ));
            }
        }

        println!("Found {} spendable UTXOs", outpoints.len());

        Ok(outpoints)
    }

    pub async fn flood_mempool(&self, breadth: Option<u64>) -> Result<usize> {
        let mut witness = Witness::new();
        witness.push(
            Builder::new()
                .push_opcode(OP_TRUE)
                .into_script()
                .into_bytes(),
        );

        let utxos = self.get_spendable_utxos().await?;
        let mut tx_count = 0;

        for (outpoint, amount) in utxos {
            if let Some(breadth) = breadth {
                let mut outputs = Vec::new();
                let mut value = amount.to_sat() / breadth;
                for i in 1..=breadth {
                    if i == breadth {
                        value -= 10_000;
                    }

                    outputs.push(TxOut {
                        script_pubkey: self.op_true_address().script_pubkey(),
                        value: Amount::from_sat(value),
                    });
                }

                let tx = Transaction {
                    version: Version::TWO,
                    lock_time: LockTime::ZERO,
                    input: vec![TxIn {
                        previous_output: outpoint,
                        script_sig: ScriptBuf::new(),
                        sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                        witness: witness.clone(),
                    }],
                    output: outputs,
                };

                self.client().unwrap().send_raw_transaction(&tx).await?;
                tx_count += 1;
            } else {
                let mut previous_output = outpoint;
                let mut value = amount - Amount::from_sat(1000);
                for _ in 0..25 {
                    let tx = Transaction {
                        version: Version::TWO,
                        lock_time: LockTime::ZERO,
                        input: vec![TxIn {
                            previous_output,
                            script_sig: ScriptBuf::new(),
                            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                            witness: witness.clone(),
                        }],
                        output: vec![TxOut {
                            script_pubkey: self.op_true_address().script_pubkey(),
                            value,
                        }],
                    };

                    self.client().unwrap().send_raw_transaction(&tx).await?;
                    tx_count += 1;

                    previous_output = OutPoint {
                        txid: tx.compute_txid(),
                        vout: 0,
                    };

                    value -= Amount::from_sat(1000);
                }
            }
        }

        Ok(tx_count)
    }

    pub fn shutdown(&mut self) {
        if let Some(ref mut handle) = self.handle {
            let _ = handle.kill();
            let _ = handle.wait();
        }
    }
}

impl Drop for Bitcoind {
    fn drop(&mut self) {
        self.shutdown()
    }
}
