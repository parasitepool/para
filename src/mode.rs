use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    Pool,
    Proxy {
        enonce1: Extranonce,
        enonce2_size: usize,
    },
}
