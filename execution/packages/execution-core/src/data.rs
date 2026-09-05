use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Unix timestamp in milliseconds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct TimestampMs(pub u64);

impl TimestampMs {
    #[must_use]
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Self(u64::try_from(millis).unwrap_or(u64::MAX))
    }
}

/// Arbitrary bytes serialized with the standard Base64 alphabet.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BinaryData(Vec<u8>);

impl BinaryData {
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for BinaryData {
    fn from(value: Vec<u8>) -> Self {
        Self(value)
    }
}

impl From<&[u8]> for BinaryData {
    fn from(value: &[u8]) -> Self {
        Self(value.to_vec())
    }
}

impl Serialize for BinaryData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for BinaryData {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        STANDARD
            .decode(value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_data_round_trips_as_base64() {
        let value = BinaryData::new([0, 1, 2, 255]);
        let json = serde_json::to_string(&value).expect("serialize bytes");
        assert_eq!(json, "\"AAEC/w==\"");
        assert_eq!(
            serde_json::from_str::<BinaryData>(&json).expect("deserialize bytes"),
            value
        );
    }

    #[test]
    fn binary_data_rejects_invalid_base64() {
        serde_json::from_str::<BinaryData>("\"not base64!\"")
            .expect_err("invalid base64 should fail");
    }
}
