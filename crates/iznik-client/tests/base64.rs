//! The one base64 codec, against RFC 4648's own test vectors.

use iznik_client::base64::{decode, encode};

/// RFC 4648 section 10: every remainder of a group of three.
const VECTORS: &[(&str, &str)] = &[
    ("", ""),
    ("f", "Zg=="),
    ("fo", "Zm8="),
    ("foo", "Zm9v"),
    ("foob", "Zm9vYg=="),
    ("fooba", "Zm9vYmE="),
    ("foobar", "Zm9vYmFy"),
];

/// # Panics
///
/// When an encoding or decoding differs from the RFC's.
#[test]
fn base64_matches_the_rfc_vectors() {
    for (plain, encoded) in VECTORS {
        assert_eq!(encode(plain.as_bytes()), *encoded, "encoding {plain:?}");
        assert_eq!(
            decode(encoded).as_deref(),
            Some(plain.as_bytes()),
            "decoding {encoded:?}"
        );
    }
}

/// # Panics
///
/// When every byte value does not survive a round trip, or malformed input
/// is accepted.
#[test]
fn base64_round_trips_and_refuses_what_is_not_base64() {
    let every: Vec<u8> = (0..=u8::MAX).collect();
    for length in 0..every.len() {
        let bytes = every.get(..length).expect("a prefix");
        assert_eq!(
            decode(&encode(bytes)).as_deref(),
            Some(bytes),
            "{length} bytes round trip"
        );
    }
    assert_eq!(
        decode("Zm9v\nYmFy").as_deref(),
        Some(&b"foobar"[..]),
        "whitespace is ignored"
    );
    for refused in ["Zg=", "Zg===", "Z===", "Zm9v!", "=Zm9"] {
        assert!(decode(refused).is_none(), "{refused:?} is refused");
    }
}
