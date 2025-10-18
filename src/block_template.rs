use super::*;

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct BlockTemplate {
    pub bits: Nbits,
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: BlockHash,
    #[serde(rename = "curtime", deserialize_with = "ntime_from_u64")]
    pub current_time: Ntime,
    pub height: u64,
    #[serde(deserialize_with = "version_from_i32")]
    pub version: Version,
    pub transactions: Vec<TemplateTransaction>,
    #[serde(with = "bitcoin::script::ScriptBuf", default)]
    pub default_witness_commitment: ScriptBuf,
    pub coinbaseaux: BTreeMap<String, String>,
    #[serde(
        rename = "coinbasevalue",
        with = "bitcoin::amount::serde::as_sat",
        default
    )]
    pub coinbase_value: Amount,
}

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct TemplateTransaction {
    pub txid: Txid,
    #[serde(rename = "data", deserialize_with = "tx_from_hex")]
    pub transaction: Transaction,
}

fn version_from_i32<'de, D>(d: D) -> Result<Version, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let x = i32::deserialize(d)?;
    Ok(Version::from(x))
}

fn tx_from_hex<'de, D>(d: D) -> Result<Transaction, D::Error>
where
    D: Deserializer<'de>,
{
    let s = <&str>::deserialize(d)?;
    encode::deserialize_hex(s).map_err(serde::de::Error::custom)
}

fn ntime_from_u64<'de, D>(d: D) -> Result<Ntime, D::Error>
where
    D: Deserializer<'de>,
{
    let v = u64::deserialize(d)?;
    Ntime::try_from(v).map_err(de::Error::custom)
}
