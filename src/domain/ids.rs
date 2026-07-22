use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub fn new_v4() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let parsed = Uuid::parse_str(value).map_err(IdParseError::InvalidUuid)?;
                if parsed.hyphenated().to_string() != value {
                    return Err(IdParseError::NonCanonical);
                }
                Ok(Self(parsed))
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Debug, Error)]
pub enum IdParseError {
    #[error("UUID is invalid")]
    InvalidUuid(#[source] uuid::Error),
    #[error("UUID must use canonical lowercase hyphenated text")]
    NonCanonical,
}

define_id!(IndexId);
define_id!(SourceId);
define_id!(ProjectId);
define_id!(DocumentId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_identity_round_trips_through_lowercase_text_and_json() {
        let raw = Uuid::parse_str("018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af").unwrap();

        macro_rules! check {
            ($kind:ident) => {{
                let value = $kind::from_uuid(raw);
                let text = value.to_string();
                assert_eq!(text, "018f47f0-9d9a-7a63-b4cc-8d6f2c8a44af");
                assert_eq!(text.parse::<$kind>().unwrap(), value);
                assert_eq!(
                    serde_json::from_str::<$kind>(&serde_json::to_string(&value).unwrap()).unwrap(),
                    value
                );
            }};
        }

        check!(IndexId);
        check!(SourceId);
        check!(ProjectId);
        check!(DocumentId);
    }

    #[test]
    fn identity_input_must_be_canonical_lowercase_hyphenated_text() {
        assert!(
            "018F47F0-9D9A-7A63-B4CC-8D6F2C8A44AF"
                .parse::<IndexId>()
                .is_err()
        );
        assert!(
            "018f47f09d9a7a63b4cc8d6f2c8a44af"
                .parse::<IndexId>()
                .is_err()
        );
        assert!(
            serde_json::from_str::<IndexId>("\"018F47F0-9D9A-7A63-B4CC-8D6F2C8A44AF\"").is_err()
        );
    }
}
