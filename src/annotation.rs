use std::time::{SystemTime, UNIX_EPOCH};

/// Return current time as `"{secs}.{micros:06}"` for annotation log lines.
pub fn timestamp_micros() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let micros = duration.subsec_micros();
    format!("{secs}.{micros:06}")
}
