use {
    super::*,
    para::{
        COIN_VALUE,
        subcommand::wallet::{balance, generate, receive, send},
    },
};

#[tokio::test]
#[timeout(60000)]
async fn end_to_end() {
    let bitcoind = spawn_regtest();

    let sender_descriptor = CommandBuilder::new("wallet --chain regtest generate")
        .run_and_deserialize_output::<generate::Output>()
        .descriptor;

    let sender_address = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {sender_descriptor} \
         receive",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<receive::Output>()
    .address;

    generate_to_address(
        &bitcoind,
        101,
        &sender_address.clone().assume_checked().to_string(),
    )
    .await;

    let sender_balance = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {sender_descriptor} \
         balance",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<balance::Output>();

    assert!(sender_balance.confirmed > 0);

    let receiver_descriptor = CommandBuilder::new("wallet --chain regtest generate")
        .run_and_deserialize_output::<generate::Output>()
        .descriptor;

    let receiver_address = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {receiver_descriptor} \
         receive",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<receive::Output>()
    .address;

    let send_output = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {sender_descriptor} \
         send --fee-rate 3 --address {} --amount 50000",
        bitcoind.rpc_port,
        bitcoind.rpc_user,
        bitcoind.rpc_password,
        receiver_address.assume_checked()
    ))
    .run_and_deserialize_output::<send::Output>();

    let mempool_entry: serde_json::Value = bitcoind
        .client()
        .unwrap()
        .call_raw("getmempoolentry", &[json!(send_output.txid.to_string())])
        .await
        .unwrap();

    let vsize = mempool_entry["vsize"].as_u64().unwrap();
    let fee_btc = mempool_entry["fees"]["base"].as_f64().unwrap();
    let fee_sats = (fee_btc * COIN_VALUE as f64).round() as u64;

    assert_eq!(fee_sats / vsize, 3);

    generate_to_address(&bitcoind, 1, &sender_address.assume_checked().to_string()).await;

    let receiver_balance = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {receiver_descriptor} \
         balance",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<balance::Output>();

    assert_eq!(receiver_balance.confirmed, 50_000);
}

#[tokio::test]
#[timeout(60000)]
async fn change_descriptor() {
    let bitcoind = spawn_regtest();

    let generate::Output {
        descriptor,
        change_descriptor,
        ..
    } = CommandBuilder::new("wallet --chain regtest generate").run_and_deserialize_output();

    let sender_address = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {descriptor} \
         --change-descriptor {change_descriptor} \
         receive",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<receive::Output>()
    .address;

    generate_to_address(
        &bitcoind,
        101,
        &sender_address.clone().assume_checked().to_string(),
    )
    .await;

    let receiver_descriptor = CommandBuilder::new("wallet --chain regtest generate")
        .run_and_deserialize_output::<generate::Output>()
        .descriptor;

    let receiver_address = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {receiver_descriptor} \
         receive",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<receive::Output>()
    .address;

    let balance_before = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {descriptor} \
         --change-descriptor {change_descriptor} \
         balance",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<balance::Output>();

    let total_before = balance_before.total;

    CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {descriptor} \
         --change-descriptor {change_descriptor} \
         send --fee-rate 1 --address {} --amount 10000",
        bitcoind.rpc_port,
        bitcoind.rpc_user,
        bitcoind.rpc_password,
        receiver_address.clone().assume_checked(),
    ))
    .run_and_deserialize_output::<send::Output>();

    generate_to_address(&bitcoind, 1, &receiver_address.assume_checked().to_string()).await;

    let balance_after = CommandBuilder::new(format!(
        "wallet \
         --chain regtest \
         --bitcoin-rpc-port {} \
         --bitcoin-rpc-username {} \
         --bitcoin-rpc-password {} \
         --descriptor {descriptor} \
         --change-descriptor {change_descriptor} \
         balance",
        bitcoind.rpc_port, bitcoind.rpc_user, bitcoind.rpc_password,
    ))
    .run_and_deserialize_output::<balance::Output>();

    assert!(balance_after.total < total_before);
    assert!(balance_after.total > total_before - 20000);
}
