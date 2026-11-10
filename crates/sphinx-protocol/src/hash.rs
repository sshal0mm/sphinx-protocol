//! A 32-byte SHA-256 digest with one canonical text form: 64 lowercase hex
//! characters. Uppercase is rejected so the same value always signs the same.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
#[cfg_attr(all(feature = "borsh", feature = "abi"), derive(borsh::BorshSchema))]
pub struct Hash32([u8; 32]);

/// Why a string is not a valid [`Hash32`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashParseError {
    /// Not exactly 64 characters.
    Length(usize),
    /// Contains a character other than `0-9a-f`.
    NotLowercaseHex,
}

impl fmt::Display for HashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length(n) => write!(f, "expected 64 lowercase hex characters, got {n}"),
            Self::NotLowercaseHex => f.write_str("expected only lowercase hex characters 0-9a-f"),
        }
    }
}

impl std::error::Error for HashParseError {}

impl Hash32 {
    /// SHA-256 of `bytes`.
    pub fn digest(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl FromStr for Hash32 {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(HashParseError::Length(s.len()));
        }
        if !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(HashParseError::NotLowercaseHex);
        }
        let mut out = [0u8; 32];
        hex::decode_to_slice(s, &mut out).map_err(|_| HashParseError::NotLowercaseHex)?;
        Ok(Self(out))
    }
}

impl fmt::Display for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl fmt::Debug for Hash32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash32({self})")
    }
}

impl Serialize for Hash32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hash32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(de::Error::custom)
    }
}

#[cfg(feature = "abi")]
impl schemars::JsonSchema for Hash32 {
    fn schema_name() -> String {
        "Hash32".to_owned()
    }

    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            string: Some(Box::new(schemars::schema::StringValidation {
                max_length: Some(64),
                min_length: Some(64),
                pattern: Some("^[0-9a-f]{64}$".to_owned()),
            })),
            ..Default::default()
        }
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_known_vector() {
        assert_eq!(
            Hash32::digest(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn parse_is_strict() {
        let h = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(h.parse::<Hash32>().map(|x| x.to_string()), Ok(h.to_owned()));
        assert_eq!("ab".parse::<Hash32>(), Err(HashParseError::Length(2)));
        assert_eq!(h.to_uppercase().parse::<Hash32>(), Err(HashParseError::NotLowercaseHex));
    }

    #[test]
    fn serde_round_trip() {
        let h = Hash32::digest(b"x");
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<Hash32>(&json).unwrap(), h);
    }
}
