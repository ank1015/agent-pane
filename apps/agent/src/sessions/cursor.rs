use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::SessionError;

#[derive(Serialize, Deserialize)]
struct Cursor {
    created_at_us: i64,
    run_id: Uuid,
}

pub(super) struct DecodedCursor {
    pub created_at: DateTime<Utc>,
    pub run_id: Uuid,
}

pub(super) fn encode(created_at: DateTime<Utc>, run_id: Uuid) -> Result<String, SessionError> {
    let bytes = serde_json::to_vec(&Cursor {
        created_at_us: created_at.timestamp_micros(),
        run_id,
    })
    .map_err(|_| SessionError::InvalidCursor)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) fn decode(value: &str) -> Result<DecodedCursor, SessionError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SessionError::InvalidCursor)?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| SessionError::InvalidCursor)?;
    let created_at =
        DateTime::from_timestamp_micros(cursor.created_at_us).ok_or(SessionError::InvalidCursor)?;
    Ok(DecodedCursor {
        created_at,
        run_id: cursor.run_id,
    })
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};
    use uuid::Uuid;

    use super::{decode, encode};

    #[test]
    fn cursor_round_trips() {
        let created_at = Utc
            .with_ymd_and_hms(2026, 8, 24, 12, 30, 45)
            .single()
            .expect("timestamp")
            + chrono::Duration::microseconds(123_456);
        let run_id = Uuid::now_v7();

        let decoded = decode(&encode(created_at, run_id).expect("cursor")).expect("decoded");

        assert_eq!(decoded.created_at, created_at);
        assert_eq!(decoded.run_id, run_id);
    }
}
