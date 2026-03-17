use proptest::prelude::*;
use tender::log::LogLine;
use tender::model::ids::{EpochTimestamp, SessionName, SessionNameError};

// --- LogLine parse/format roundtrip ---

proptest! {
    #[test]
    fn logline_parse_roundtrip(
        secs in 0u64..10_000_000_000,
        micros in 0u64..1_000_000,
        tag in prop_oneof![Just('O'), Just('E')],
        content in "[^\n\r]{0,200}",
    ) {
        let line = format!("{secs}.{micros:06} {tag} {content}");
        let parsed = LogLine::parse(&line).expect("should parse well-formed line");

        prop_assert_eq!(parsed.timestamp_us, secs * 1_000_000 + micros);
        prop_assert_eq!(parsed.tag, tag);
        prop_assert_eq!(&parsed.content, &content);

        // format_prefixed roundtrips back to the original line
        let formatted = parsed.format_prefixed();
        prop_assert_eq!(&formatted, &line);
    }

    #[test]
    fn logline_format_raw_is_content(
        secs in 0u64..10_000_000_000,
        micros in 0u64..1_000_000,
        tag in prop_oneof![Just('O'), Just('E')],
        content in "[^\n\r]{0,200}",
    ) {
        let line = format!("{secs}.{micros:06} {tag} {content}");
        let parsed = LogLine::parse(&line).unwrap();
        prop_assert_eq!(parsed.format_raw(), &content);
    }

    #[test]
    fn logline_rejects_invalid_tag(
        secs in 0u64..10_000_000_000,
        micros in 0u64..1_000_000,
        tag in "[^OE]",
        content in "[^\n\r]{0,50}",
    ) {
        let line = format!("{secs}.{micros:06} {tag} {content}");
        prop_assert!(LogLine::parse(&line).is_none());
    }

    #[test]
    fn logline_rejects_short_micros(
        secs in 0u64..10_000_000_000,
        micros in 0u64..1_000_000,
    ) {
        // Micros formatted with fewer than 6 digits should be rejected
        let short = format!("{secs}.{micros} O test");
        let digits = format!("{micros}").len();
        if digits != 6 {
            prop_assert!(LogLine::parse(&short).is_none());
        }
    }
}

// --- EpochTimestamp serde roundtrip ---

proptest! {
    #[test]
    fn epoch_timestamp_serde_roundtrip(secs in 1u64..10_000_000_000) {
        // Serialize as JSON, deserialize back
        let ts = serde_json::json!(secs.to_string());
        let parsed: EpochTimestamp = serde_json::from_value(ts).unwrap();
        let serialized = serde_json::to_value(&parsed).unwrap();
        // EpochTimestamp serializes as string
        prop_assert_eq!(serialized.as_str().unwrap(), &secs.to_string());
    }

    #[test]
    fn epoch_timestamp_accepts_integer_and_string(secs in 1u64..10_000_000_000) {
        // String form
        let from_str: EpochTimestamp =
            serde_json::from_value(serde_json::json!(secs.to_string())).unwrap();
        // Integer form
        let from_int: EpochTimestamp =
            serde_json::from_value(serde_json::json!(secs)).unwrap();
        // Both should produce the same value
        prop_assert_eq!(from_str, from_int);
    }
}

// --- SessionName validation ---

proptest! {
    #[test]
    fn session_name_valid_names_roundtrip(
        name in "[a-zA-Z][a-zA-Z0-9_-]{0,50}"
    ) {
        let sn = SessionName::new(&name).unwrap();
        prop_assert_eq!(sn.as_str(), &name);

        // Serde roundtrip
        let json = serde_json::to_string(&sn).unwrap();
        let parsed: SessionName = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(parsed.as_str(), &name);
    }

    #[test]
    fn session_name_rejects_slashes(
        prefix in "[a-zA-Z]{1,10}",
        suffix in "[a-zA-Z]{1,10}",
    ) {
        let name = format!("{prefix}/{suffix}");
        prop_assert!(matches!(
            SessionName::new(&name),
            Err(SessionNameError::ContainsSlash)
        ));
    }

    #[test]
    fn session_name_rejects_dots(
        prefix in "[a-zA-Z]{1,10}",
        suffix in "[a-zA-Z]{1,10}",
    ) {
        let name = format!("{prefix}.{suffix}");
        prop_assert!(matches!(
            SessionName::new(&name),
            Err(SessionNameError::ContainsDot)
        ));
    }

    #[test]
    fn session_name_rejects_too_long(extra in 1usize..100) {
        let name = "a".repeat(255 + extra);
        prop_assert!(matches!(
            SessionName::new(&name),
            Err(SessionNameError::TooLong)
        ));
    }
}
