use super::*;

#[derive(Clone, PartialEq, Eq, Debug, Deserialize, Serialize)]
pub struct BlockTemplate {
    pub bits: Nbits,
    #[serde(rename = "previousblockhash")]
    pub previous_block_hash: bitcoin::BlockHash,
    #[serde(rename = "curtime")]
    pub current_time: u64,
    pub height: u64,
    #[serde(deserialize_with = "version_from_i32")]
    pub version: Version,
    pub transactions: Vec<GetBlockTemplateResultTransaction>,
    #[serde(with = "bitcoin::script::ScriptBuf", default)]
    pub default_witness_commitment: bitcoin::script::ScriptBuf,
    pub coinbaseaux: BTreeMap<String, String>,
    #[serde(
        rename = "coinbasevalue",
        with = "bitcoin::amount::serde::as_sat",
        default
    )]
    pub coinbase_value: Amount,
}

fn version_from_i32<'de, D>(d: D) -> Result<Version, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let x = i32::deserialize(d)?;
    Ok(Version::from(x))
}
