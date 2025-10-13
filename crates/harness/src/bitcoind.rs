use super::*;

pub struct Bitcoind {
    pub datadir: PathBuf,
    pub handle: Child,
    pub network: Network,
    pub rpc_port: u16,
    pub rpc_user: String,
    pub rpc_password: String,
    pub with_output: bool,
}

impl Bitcoind {
    pub fn spawn(
        tempdir: Arc<TempDir>,
        bitcoind_port: u16,
        rpc_port: u16,
        zmq_port: u16,
        with_output: bool,
    ) -> Result<Self> {
        let bitcoind_data_dir = tempdir.path().join("bitcoin");
        fs::create_dir(&bitcoind_data_dir)?;

        let bitcoind_conf = bitcoind_data_dir.join("bitcoin.conf");

        let network = Network::Signet;
        let rpc_user = "satoshi".to_string();
        let rpc_password = "nakamoto".to_string();

        fs::write(
            &bitcoind_conf,
            format!(
                "
signet=1
datadir={}

[signet]
# OP_TRUE
signetchallenge=51

server=1
txindex=1
zmqpubhashblock=tcp://127.0.0.1:{zmq_port}

port={bitcoind_port}

maxconnections=256
maxmempool=2048
minrelaytxfee=0.00001

rpcbind=127.0.0.1
rpcport={rpc_port}
rpcallowip=127.0.0.1
rpcauth={}

maxtxfee=1000000
",
                &bitcoind_data_dir.display(),
                Self::generate_rpcauth(rpc_user.as_str(), rpc_password.as_str(), None)
            ),
        )?;

        let handle = Command::new("bitcoind")
            .arg(format!("-conf={}", bitcoind_conf.display()))
            .stdout(if with_output {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(if with_output {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .spawn()?;

        let status = Command::new("bitcoin-cli")
            .args([
                &format!("-conf={}", bitcoind_conf.display()),
                "-rpcwait",
                "-rpcwaittimeout=5",
                "getblockchaininfo",
            ])
            .stdout(Stdio::null())
            .status()?;

        assert!(
            status.success(),
            "Failed to connect bitcoind after 5 seconds"
        );

        Ok(Self {
            datadir: bitcoind_data_dir,
            handle,
            network,
            rpc_port,
            rpc_user,
            rpc_password,
            with_output,
        })
    }

    pub fn generate_rpcauth(username: &str, password: &str, salt_overide: Option<&str>) -> String {
        let salt = if let Some(salt_overide) = salt_overide {
            salt_overide.to_string()
        } else {
            let mut salt_bytes = [0u8; 16];
            thread_rng().fill_bytes(&mut salt_bytes);
            salt_bytes
                .clone()
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
            &format!("127.0.0.1:{}", self.rpc_port),
            Auth::UserPass(self.rpc_user.clone(), self.rpc_password.clone()),
        )?)
    }

    pub fn create_or_load_wallet(&self) -> Result {
        match self
            .client()?
            .create_wallet("testing-harness", None, None, None, None)
        {
            Ok(_) => {}
            Err(_err) => {}
        }

        Ok(())
    }

    pub fn op_true_address(&self) -> Address {
        let op_true: ScriptBuf = Builder::new().push_opcode(OP_TRUE).into_script();
        Address::p2wsh(&op_true, self.network)
    }

    pub fn get_spendable_utxos(&self) -> Result<Vec<(OutPoint, Amount)>> {
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
            .call("scantxoutset", &["start".into(), json!([descriptor])])?;

        let block_count = self.client()?.get_block_count()?;

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

        println!("Num utxos: {}", outpoints.len());

        Ok(outpoints)
    }

    pub fn flood_mempool(&self, breadth: Option<u64>) -> Result {
        let mut witness = Witness::new();
        witness.push(
            Builder::new()
                .push_opcode(OP_TRUE)
                .into_script()
                .into_bytes(),
        );

        let utxos = self.get_spendable_utxos()?;

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

                self.client().unwrap().send_raw_transaction(&tx)?;
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

                    self.client().unwrap().send_raw_transaction(&tx)?;

                    previous_output = OutPoint {
                        txid: tx.compute_txid(),
                        vout: 0,
                    };

                    value -= Amount::from_sat(1000);
                }
            }
        }

        Ok(())
    }

    pub fn mine_blocks(&self, n: usize) -> Result {
        self.create_or_load_wallet()?;

        let workspace = workspace_root();

        // quick hack, refactor later
        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
{workspace}/bitcoin/contrib/signet/miner \
    --cli="{workspace}/bitcoin/build/bin/bitcoin-cli -datadir={} -signet -rpcport={}" \
    generate \
    --grind-cmd="{workspace}/bitcoin/build/bin/bitcoin-util grind" \
    --address="{}" \
    --nbits=1d00ffff
"#,
            self.datadir.display(),
            self.rpc_port,
            self.op_true_address()
        );

        for _ in 0..n {
            let status = Command::new("bash")
                .arg("-c")
                .arg(script.clone())
                .stdin(Stdio::null())
                .stdout(if self.with_output {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                })
                .stderr(if self.with_output {
                    Stdio::inherit()
                } else {
                    Stdio::null()
                })
                .status()?;

            if !status.success() {
                anyhow::bail!("bash script failed");
            }
        }

        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.handle.kill().unwrap();
        self.handle.wait().unwrap();
    }
}

impl Drop for Bitcoind {
    fn drop(&mut self) {
        self.shutdown()
    }
}
