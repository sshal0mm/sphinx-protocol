//! JSON encodes `u64` as a decimal string so JavaScript clients never lose
//! precision. Accepts a string or a JSON number on input.

use serde::{de, Deserialize, Deserializer, Serializer};

#[allow(clippy::trivially_copy_pass_by_ref, reason = "signature required by serde(with)")]
pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.to_string())
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Str(String),
        Num(u64),
    }
    match Repr::deserialize(deserializer)? {
        Repr::Str(s) => s.parse().map_err(de::Error::custom),
        Repr::Num(n) => Ok(n),
    }
}
