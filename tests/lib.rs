use {
    bitcoin::{Address, address::NetworkUnchecked},
    command_builder::CommandBuilder,
    executable_path::executable_path,
    para::{
        ckpool::{self, HashRate, HashRateStatus, PoolStatus, ShareStatus, User, Worker},
        subcommand::server::{
            Status,
            notifications::{NotificationHandler, NotificationPriority, NotificationType},
        },
    },
    pretty_assertions::assert_eq as pretty_assert_eq,
    reqwest::{StatusCode, Url},
    serde::{Deserialize, Serialize, de::DeserializeOwned},
    serial_test::serial,
    std::{
        collections::{BTreeMap, HashSet},
        ffi::{OsStr, OsString},
        fs,
        io::Write,
        net::TcpListener,
        path::PathBuf,
        process::{Child, Command, Stdio},
        str::FromStr,
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    },
    tempfile::TempDir,
    test_server::TestServer,
    to_args::ToArgs,
    tokio::time::timeout,
};

#[cfg(target_os = "linux")]
use {
    crate::{
        sync::BATCH_COUNTER,
        test_psql::{
            create_test_block, create_test_shares, insert_test_account, insert_test_block,
            insert_test_remote_shares, insert_test_shares, setup_test_schema,
        },
    },
    anyhow::Error,
    base64::{Engine, engine::general_purpose},
    bip322::sign_simple_encoded,
    bitcoin::{
        CompressedPublicKey, Network, PrivateKey, block::Header, hashes::Hash,
        key::UntweakedPublicKey, secp256k1::Secp256k1, sign_message::MessageSignature,
    },
    bitcoincore_rpc::RpcApi,
    harness::bitcoind::Bitcoind,
    ntest::timeout,
    para::{
        USER_AGENT,
        stratum::{
            self, ClientError, Difficulty, Extranonce, JobId, Nonce, Notify, Ntime, StratumError,
            Username, Version,
        },
        subcommand::{
            miner::Share,
            server::{
                account::{Account, AccountUpdate},
                database::{Database, HighestDiff, Payout},
            },
            sync::{ShareBatch, Sync, SyncResponse},
        },
    },
    pgtemp::{PgTempDB, PgTempDBBuilder},
    reqwest::Response,
    std::{
        io::{BufReader, stderr},
        net::TcpStream,
        process::ChildStdout,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
    },
    tempfile::tempdir,
    test_ckpool::TestCkpool,
    test_pool::TestPool,
};

mod command_builder;
#[cfg(target_os = "linux")]
mod test_ckpool;
#[cfg(target_os = "linux")]
mod test_pool;
#[cfg(target_os = "linux")]
mod test_psql;
mod test_server;
mod to_args;

#[cfg(target_os = "linux")]
mod account;
mod alerts;
#[cfg(target_os = "linux")]
mod payouts;
#[cfg(target_os = "linux")]
mod ping;
#[cfg(target_os = "linux")]
mod pool;
mod server;
#[cfg(target_os = "linux")]
mod server_with_db;
#[cfg(target_os = "linux")]
mod sync;
#[cfg(target_os = "linux")]
mod template;

#[cfg(target_os = "linux")]
fn next_json<T: DeserializeOwned>(r: &mut BufReader<ChildStdout>) -> T {
    let de = serde_json::Deserializer::from_reader(&mut *r);
    let mut stream = de.into_iter::<T>();
    stream.next().expect("stream ended").expect("bad json")
}

#[cfg(target_os = "linux")]
fn signet_username() -> Username {
    Username::new("tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.tick.abcdef@lnurl.com")
}

#[cfg(target_os = "linux")]
fn solve_share(
    notify: &stratum::Notify,
    enonce1: &Extranonce,
    enonce2: &Extranonce,
    difficulty: stratum::Difficulty,
) -> (Ntime, Nonce) {
    let merkle_root = stratum::merkle_root(
        &notify.coinb1,
        &notify.coinb2,
        enonce1,
        enonce2,
        &notify.merkle_branches,
    )
    .unwrap();

    let mut header = Header {
        version: notify.version.into(),
        prev_blockhash: notify.prevhash.clone().into(),
        merkle_root: merkle_root.into(),
        time: notify.ntime.into(),
        bits: notify.nbits.into(),
        nonce: 0,
    };

    let target = difficulty.to_target();

    loop {
        let hash = header.block_hash();
        if target.is_met_by(hash) {
            return (Ntime::from(header.time), Nonce::from(header.nonce));
        }

        header.nonce += 1;
        if header.nonce == 0 {
            panic!(
                "Nonce wrapped around without finding share at diff {}",
                difficulty
            );
        }
    }
}

pub(crate) fn address(n: u32) -> Address {
    match n {
        0 => "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        1 => "bc1qhl452zcq3ng5kzajzkx9jnzncml9tnsk3w96s6",
        2 => "bc1qqqcjq9jydx79rywltc38g5qfrjq485a8xfmkf7",
        3 => "bc1qcq2uv5nk6hec6kvag3wyevp6574qmsm9scjxc2",
        4 => "bc1qukgekwq8e68ay0mewdrvg0d3cfuc094aj2rvx9",
        5 => "bc1qtdjs8tgkaja5ddxs0j7rn52uqfdtqa53mum8xc",
        6 => "bc1qd3ex6kwlc5ett55hgsnk94y8q2zhdyxyqyujkl",
        7 => "bc1q8dcv8r903evljd87mcg0hq8lphclch7pd776wt",
        8 => "bc1q9j6xvm3td447ygnhfra5tfkpkcupwe9937nhjq",
        9 => "bc1qlyrhjzvxdzmvxe2mnr37p68vkl5fysyhfph8z0",
        _ => panic!(),
    }
    .parse::<Address<NetworkUnchecked>>()
    .unwrap()
    .assume_checked()
}

fn zero_status() -> ckpool::Status {
    ckpool::Status {
        pool: PoolStatus {
            runtime: 0,
            lastupdate: 0,
            users: 0,
            workers: 0,
            idle: 0,
            disconnected: 0,
        },
        hash_rates: HashRateStatus {
            hashrate1m: HashRate(0.0),
            hashrate5m: HashRate(0.0),
            hashrate15m: HashRate(0.0),
            hashrate1hr: HashRate(0.0),
            hashrate6hr: HashRate(0.0),
            hashrate1d: HashRate(0.0),
            hashrate7d: HashRate(0.0),
        },
        shares: ShareStatus {
            diff: 0.0,
            accepted: 0,
            rejected: 0,
            bestshare: 0,
            sps1m: 0.0,
            sps5m: 0.0,
            sps15m: 0.0,
            sps1h: 0.0,
        },
    }
}

fn typical_status() -> ckpool::Status {
    ckpool::Status {
        pool: PoolStatus {
            runtime: 86400,
            lastupdate: 0,
            users: 5000,
            workers: 20000,
            idle: 1000,
            disconnected: 500,
        },
        hash_rates: HashRateStatus {
            hashrate1m: HashRate::from_str("100E").unwrap(),
            hashrate5m: HashRate::from_str("95E").unwrap(),
            hashrate15m: HashRate::from_str("98E").unwrap(),
            hashrate1hr: HashRate::from_str("102E").unwrap(),
            hashrate6hr: HashRate::from_str("99E").unwrap(),
            hashrate1d: HashRate::from_str("100E").unwrap(),
            hashrate7d: HashRate::from_str("105E").unwrap(),
        },
        shares: ShareStatus {
            diff: 120.5,
            accepted: 1_000_000_000_000,
            rejected: 10_000_000_000,
            bestshare: 500_000_000_000,
            sps1m: 5000.0,
            sps5m: 4900.0,
            sps15m: 4950.0,
            sps1h: 5050.0,
        },
    }
}

fn zero_user() -> User {
    User {
        hashrate1m: HashRate(0.0),
        hashrate5m: HashRate(0.0),
        hashrate1hr: HashRate(0.0),
        hashrate1d: HashRate(0.0),
        hashrate7d: HashRate(0.0),
        lastshare: 0,
        workers: 0,
        shares: 0,
        bestshare: 0.0,
        bestever: 0,
        authorised: 0,
        worker: vec![],
    }
}

fn typical_user() -> User {
    let now = 1755043200;
    User {
        hashrate1m: HashRate::from_str("10P").unwrap(),
        hashrate5m: HashRate::from_str("9.5P").unwrap(),
        hashrate1hr: HashRate::from_str("10.2P").unwrap(),
        hashrate1d: HashRate::from_str("10P").unwrap(),
        hashrate7d: HashRate::from_str("10.5P").unwrap(),
        lastshare: now - 60,
        workers: 3,
        shares: 1_000_000_000,
        bestshare: 1e12,
        bestever: 5_000_000_000,
        authorised: 1,
        worker: vec![
            Worker {
                workername: "rig1".to_string(),
                hashrate1m: HashRate::from_str("4P").unwrap(),
                hashrate5m: HashRate::from_str("3.8P").unwrap(),
                hashrate1hr: HashRate::from_str("4.1P").unwrap(),
                hashrate1d: HashRate::from_str("4P").unwrap(),
                hashrate7d: HashRate::from_str("4.2P").unwrap(),
                lastshare: now - 120,
                shares: 400_000_000,
                bestshare: 5e11,
                bestever: 2_000_000_000,
            },
            Worker {
                workername: "rig2".to_string(),
                hashrate1m: HashRate::from_str("3P").unwrap(),
                hashrate5m: HashRate::from_str("2.9P").unwrap(),
                hashrate1hr: HashRate::from_str("3.0P").unwrap(),
                hashrate1d: HashRate::from_str("3P").unwrap(),
                hashrate7d: HashRate::from_str("3.1P").unwrap(),
                lastshare: now - 180,
                shares: 300_000_000,
                bestshare: 3e11,
                bestever: 1_500_000_000,
            },
            Worker {
                workername: "rig3".to_string(),
                hashrate1m: HashRate::from_str("3P").unwrap(),
                hashrate5m: HashRate::from_str("2.8P").unwrap(),
                hashrate1hr: HashRate::from_str("3.1P").unwrap(),
                hashrate1d: HashRate::from_str("3P").unwrap(),
                hashrate7d: HashRate::from_str("3.2P").unwrap(),
                lastshare: now - 60,
                shares: 300_000_000,
                bestshare: 4e11,
                bestever: 1_500_000_000,
            },
        ],
    }
}
