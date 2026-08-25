use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::HarnessError;

#[derive(Serialize, Deserialize)]
struct Cursor {
    created_at_us: i64,
    id: String,
}

pub(super) struct DecodedCursor {
    pub created_at: DateTime<Utc>,
    pub id: String,
}

pub(super) fn encode(created_at: DateTime<Utc>, id: &str) -> Result<String, HarnessError> {
    let bytes = serde_json::to_vec(&Cursor {
        created_at_us: created_at.timestamp_micros(),
        id: id.to_owned(),
    })
    .map_err(|_| HarnessError::InvalidCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) fn decode(value: &str) -> Result<DecodedCursor, HarnessError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| HarnessError::InvalidCursor)?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| HarnessError::InvalidCursor)?;
    let created_at =
        DateTime::from_timestamp_micros(cursor.created_at_us).ok_or(HarnessError::InvalidCursor)?;
    if cursor.id.is_empty() {
        return Err(HarnessError::InvalidCursor);
    }
    Ok(DecodedCursor {
        created_at,
        id: cursor.id,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::{decode, encode};

    #[test]
    fn cursor_preserves_postgres_timestamp_precision() {
        let timestamp = Utc
            .with_ymd_and_hms(2026, 8, 24, 12, 30, 45)
            .single()
            .expect("timestamp")
            + chrono::Duration::microseconds(123_456);
        let encoded = encode(timestamp, "item-1").expect("encoded cursor");
        let decoded = decode(&encoded).expect("decoded cursor");

        assert_eq!(decoded.created_at, timestamp);
        assert_eq!(decoded.id, "item-1");
    }
}
