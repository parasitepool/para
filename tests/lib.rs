use {
    bitcoin::{Address, address::NetworkUnchecked},
    command_builder::CommandBuilder,
    executable_path::executable_path,
    para::{
        ckpool::{HashRateStatus, PoolStatus, ShareStatus, Status, User, Worker},
        hash_rate::HashRate,
    },
    pretty_assertions::assert_eq as pretty_assert_eq,
    reqwest::{StatusCode, Url},
    serde::de::DeserializeOwned,
    std::{
        collections::{BTreeMap, HashSet},
        fs,
        io::Write,
        net::TcpListener,
        path::PathBuf,
        process::{Child, Command, Stdio},
        str::FromStr,
        sync::Arc,
        thread,
        time::Duration,
    },
    tempfile::TempDir,
    test_server::TestServer,
    to_args::ToArgs,
};

mod command_builder;
mod test_server;
mod to_args;

mod server;

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
