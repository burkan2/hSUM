use std::fmt;
use std::str::FromStr;

use thiserror::Error;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SafeSlug(String);

impl SafeSlug {
    pub fn new(value: impl Into<String>) -> Result<Self, SlugError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SlugError::Empty);
        }
        if value.len() > 64 {
            return Err(SlugError::TooLong);
        }
        if let Some((offset, _)) = value
            .bytes()
            .enumerate()
            .find(|(_, byte)| !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
        {
            return Err(SlugError::InvalidByte { offset });
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for SafeSlug {
    type Err = SlugError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl fmt::Display for SafeSlug {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SlugError {
    #[error("slug is empty")]
    Empty,
    #[error("slug is longer than 64 bytes")]
    TooLong,
    #[error("slug contains an invalid byte at offset {offset}")]
    InvalidByte { offset: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn accepts_the_locked_slug_alphabet() {
        for value in ["hsum", "alpha-1", "project_42", "0"] {
            assert_eq!(SafeSlug::new(value).unwrap().as_str(), value);
        }
    }

    #[test]
    fn rejects_empty_long_dot_unicode_uppercase_and_separators() {
        let long = "a".repeat(65);
        for value in ["", ".", "..", "Hsum", "mémoire", "a/b", "a\\b", &long] {
            assert!(SafeSlug::new(value).is_err(), "{value:?} must be rejected");
        }
    }

    proptest! {
        #[test]
        fn accepted_slugs_round_trip(value in "[a-z0-9_-]{1,64}") {
            let slug = SafeSlug::new(value.clone()).unwrap();
            prop_assert_eq!(slug.to_string(), value);
        }
    }
}
