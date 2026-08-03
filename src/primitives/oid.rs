//! Object ids, fixed at SHA-1 because the store pins `--object-format=sha1` and a
//! source repository whose format differs is a hard per-repo error.

use std::fmt;

const BYTES: usize = 20;
const HEX: usize = BYTES * 2;
const SHORT: usize = 7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Oid([u8; BYTES]);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum OidError {
    #[error("expected {expected} hex characters, got {actual}")]
    Length { expected: usize, actual: usize },
    #[error("byte {at} of an object id is not hex")]
    NotHex { at: usize },
}

impl Oid {
    /// Parses the 40-character lowercase or uppercase hex form.
    pub fn parse(text: &str) -> Result<Self, OidError> {
        let raw = text.as_bytes();
        if raw.len() != HEX {
            return Err(OidError::Length {
                expected: HEX,
                actual: raw.len(),
            });
        }
        let mut bytes = [0u8; BYTES];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let high = nibble(raw[index * 2], index * 2)?;
            let low = nibble(raw[index * 2 + 1], index * 2 + 1)?;
            *byte = (high << 4) | low;
        }
        Ok(Self(bytes))
    }

    #[must_use]
    pub const fn from_bytes(bytes: [u8; BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }

    /// The abbreviated form used in rendered output.
    #[must_use]
    pub fn short(&self) -> String {
        let mut text = self.to_string();
        text.truncate(SHORT);
        text
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn nibble(byte: u8, at: usize) -> Result<u8, OidError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(OidError::NotHex { at }),
    }
}

#[cfg(test)]
mod tests {
    use super::{HEX, Oid, OidError};

    const SAMPLE: &str = "8f2a10c1930b99aef686f41c8ee24e10f92aa7d4";

    #[test]
    fn parses_and_renders_round_trip() {
        let oid = Oid::parse(SAMPLE).expect("sample is valid hex");
        assert_eq!(oid.to_string(), SAMPLE);
        assert_eq!(oid.short(), "8f2a10c");
    }

    #[test]
    fn uppercase_parses_to_the_same_value() {
        let lower = Oid::parse(SAMPLE).expect("valid");
        let upper = Oid::parse(&SAMPLE.to_uppercase()).expect("valid");
        assert_eq!(lower, upper);
        assert_eq!(upper.to_string(), SAMPLE);
    }

    #[test]
    fn every_byte_value_round_trips() {
        for value in 0..=u8::MAX {
            let oid = Oid::from_bytes([value; 20]);
            assert_eq!(Oid::parse(&oid.to_string()), Ok(oid));
        }
    }

    #[test]
    fn wrong_length_is_rejected() {
        for text in ["", &SAMPLE[..39], &format!("{SAMPLE}0")] {
            assert_eq!(
                Oid::parse(text),
                Err(OidError::Length {
                    expected: HEX,
                    actual: text.len()
                })
            );
        }
    }

    #[test]
    fn non_hex_is_rejected_with_its_position() {
        let text = format!("{}z{}", &SAMPLE[..5], &SAMPLE[6..]);
        assert_eq!(Oid::parse(&text), Err(OidError::NotHex { at: 5 }));
    }
}
