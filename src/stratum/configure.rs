use {
    super::*,
    serde::{de::Error as DeError, ser::SerializeMap},
};

// TODO: this is wrong
#[derive(Debug, PartialEq)]
pub struct Configure {
    pub extensions: Vec<String>,
    pub minimum_difficulty_value: Option<u64>,
    pub version_rolling_mask: Option<Version>,
    pub version_rolling_min_bit_count: Option<u32>,
}

impl Serialize for Configure {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let have_opts = self.minimum_difficulty_value.is_some()
            || self.version_rolling_mask.is_some()
            || self.version_rolling_min_bit_count.is_some();

        let mut seq = serializer.serialize_seq(Some(if have_opts { 2 } else { 1 }))?;
        seq.serialize_element(&self.extensions)?;

        if have_opts {
            struct Opts<'a>(&'a Configure);
            impl<'a> Serialize for Opts<'a> {
                fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
                where
                    S: Serializer,
                {
                    let mut n = 0usize;
                    if self.0.minimum_difficulty_value.is_some() {
                        n += 1;
                    }
                    if self.0.version_rolling_mask.is_some() {
                        n += 1;
                    }
                    if self.0.version_rolling_min_bit_count.is_some() {
                        n += 1;
                    }

                    let mut map = serializer.serialize_map(Some(n))?;
                    if let Some(v) = self.0.minimum_difficulty_value {
                        map.serialize_entry("minimum-difficulty.value", &v)?;
                    }
                    if let Some(ref mask) = self.0.version_rolling_mask {
                        // Version already SerializeDisplay -> hex string
                        map.serialize_entry("version-rolling.mask", mask)?;
                    }
                    if let Some(bits) = self.0.version_rolling_min_bit_count {
                        map.serialize_entry("version-rolling.min-bit-count", &bits)?;
                    }
                    map.end()
                }
            }
            seq.serialize_element(&Opts(self))?;
        }

        seq.end()
    }
}

impl<'de> Deserialize<'de> for Configure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(Vec<String>),
            Two(Vec<String>, HashMap<String, serde_json::Value>),
        }

        let raw = Raw::deserialize(deserializer)?;
        match raw {
            Raw::One(extensions) => Ok(Configure {
                extensions,
                minimum_difficulty_value: None,
                version_rolling_mask: None,
                version_rolling_min_bit_count: None,
            }),
            Raw::Two(extensions, mut opts) => {
                fn take_u64<E: DeError>(
                    m: &mut HashMap<String, serde_json::Value>,
                    k: &str,
                ) -> Result<Option<u64>, E> {
                    match m.remove(k) {
                        None => Ok(None),
                        Some(v) => v
                            .as_u64()
                            .map(Some)
                            .ok_or_else(|| DeError::custom(format!("`{k}` must be a u64"))),
                    }
                }
                fn take_u32<E: DeError>(
                    m: &mut HashMap<String, serde_json::Value>,
                    k: &str,
                ) -> Result<Option<u32>, E> {
                    match m.remove(k) {
                        None => Ok(None),
                        Some(v) => match v.as_u64() {
                            Some(n) => u32::try_from(n).map(Some).map_err(|_| {
                                DeError::custom(format!("`{k}` out of range for u32"))
                            }),
                            None => Err(DeError::custom(format!("`{k}` must be a number"))),
                        },
                    }
                }
                fn take_version<E: DeError>(
                    m: &mut HashMap<String, serde_json::Value>,
                    k: &str,
                ) -> Result<Option<Version>, E> {
                    match m.remove(k) {
                        None => Ok(None),
                        Some(v) => {
                            let s = v.as_str().ok_or_else(|| {
                                DeError::custom(format!("`{k}` must be a hex string"))
                            })?;
                            Version::from_str(s)
                                .map(Some)
                                .map_err(|e| DeError::custom(format!("`{k}` parse error: {e}")))
                        }
                    }
                }

                let minimum_difficulty_value =
                    take_u64::<D::Error>(&mut opts, "minimum-difficulty.value")?;
                let version_rolling_mask =
                    take_version::<D::Error>(&mut opts, "version-rolling.mask")?;
                let version_rolling_min_bit_count =
                    take_u32::<D::Error>(&mut opts, "version-rolling.min-bit-count")?;

                Ok(Configure {
                    extensions,
                    minimum_difficulty_value,
                    version_rolling_mask,
                    version_rolling_min_bit_count,
                })
            }
        }
    }
}
