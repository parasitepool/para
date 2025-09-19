use super::*;

pub struct Bitcoind {
    pub datadir: PathBuf,
    pub handle: Child,
    pub network: Network,
    pub rpc_port: u16,
    pub rpc_user: String,
    pub rpc_password: String,
}

impl Bitcoind {
    pub fn spawn(
        tempdir: Arc<TempDir>,
        bitcoind_port: u16,
        rpc_port: u16,
        zmq_port: u16,
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

datacarriersize=100000
maxconnections=256
maxmempool=2048
mempoolfullrbf=1
minrelaytxfee=0

rpcbind=127.0.0.1
rpcport={rpc_port}
rpcallowip=127.0.0.1
rpcuser={rpc_user}
rpcpassword={rpc_password}

maxtxfee=1000000
",
                &bitcoind_data_dir.display()
            ),
        )?;

        let handle = Command::new("bitcoind")
            .arg(format!("-conf={}", bitcoind_conf.display()))
            .stderr(Stdio::null())
            .stdout(Stdio::null())
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
        })
    }

    pub fn get_spendable_utxos(&self) -> Result<Vec<(OutPoint, Amount)>> {
        let descriptor = format!("addr({})", self.op_true_address());

        let result = self
            .client()?
            .scan_tx_out_set_blocking(&[ScanTxOutRequest::Single(descriptor)])?;

        let block_count = self.client()?.get_block_count()?;

        let mut outpoints = Vec::new();
        for utxo in result.unspents {
            if block_count - utxo.height >= MATURITY {
                outpoints.push((
                    OutPoint {
                        txid: utxo.txid,
                        vout: utxo.vout,
                    },
                    utxo.amount,
                ));
            }
        }

        Ok(outpoints)
    }

    pub fn client(&self) -> Result<Client> {
        Ok(Client::new(
            &format!("127.0.0.1:{}", self.rpc_port),
            Auth::UserPass(self.rpc_user.clone(), self.rpc_password.clone()),
        )?)
    }

    pub fn op_true_address(&self) -> Address {
        let op_true: ScriptBuf = Builder::new().push_opcode(OP_TRUE).into_script();
        Address::p2wsh(&op_true, self.network)
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

    // quick hack, refactor later
    pub fn mine_blocks(&self, n: usize) -> Result {
        self.create_or_load_wallet()?;

        let script = format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
CLI="../../bitcoin/build/bin/bitcoin-cli -datadir={} -signet -rpcport={}"
MINER="../../bitcoin/contrib/signet/miner"
GRIND="../../bitcoin/build/bin/bitcoin-util grind"
ADDR={}
NBITS=1d00ffff
$MINER --cli="$CLI" generate --grind-cmd="$GRIND" --address="$ADDR" --nbits=$NBITS
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
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
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
