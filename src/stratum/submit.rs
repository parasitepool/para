use super::*;

#[derive(Debug, PartialEq)]
pub struct Submit {
    pub username: String,
    pub job_id: String,
    pub extranonce2: String,
    pub ntime: Ntime,
    pub nonce: Nonce,
    pub version_bits: Option<Version>,
}

impl Serialize for Submit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let len = if self.version_bits.is_some() { 6 } else { 5 };
        let mut seq = serializer.serialize_seq(Some(len))?;
        seq.serialize_element(&self.username)?;
        seq.serialize_element(&self.job_id)?;
        seq.serialize_element(&self.extranonce2)?;
        seq.serialize_element(&self.ntime)?;
        seq.serialize_element(&self.nonce)?;
        if let Some(v) = &self.version_bits {
            seq.serialize_element(v)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Submit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (username, job_id, extranonce2, ntime, nonce, version_bits) =
            <(String, String, String, Ntime, Nonce, Option<Version>)>::deserialize(deserializer)?;

        Ok(Submit {
            username,
            job_id,
            extranonce2,
            ntime,
            nonce,
            version_bits,
        })
    }
}
