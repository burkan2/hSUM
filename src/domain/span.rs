use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ByteSpan {
    start: u64,
    end: u64,
}

impl ByteSpan {
    pub fn new(start: u64, end: u64) -> Result<Self, SpanError> {
        if start > end {
            return Err(SpanError::Reversed);
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }

    pub fn slice_bytes(self, value: &[u8]) -> Result<&[u8], SpanError> {
        let start = usize::try_from(self.start).map_err(|_| SpanError::InvalidBoundary)?;
        let end = usize::try_from(self.end).map_err(|_| SpanError::InvalidBoundary)?;
        value.get(start..end).ok_or(SpanError::InvalidBoundary)
    }

    pub fn slice_utf8(self, value: &str) -> Result<&str, SpanError> {
        let start = usize::try_from(self.start).map_err(|_| SpanError::InvalidBoundary)?;
        let end = usize::try_from(self.end).map_err(|_| SpanError::InvalidBoundary)?;
        value.get(start..end).ok_or(SpanError::InvalidBoundary)
    }

    pub fn slice_str(self, value: &str) -> Result<&str, SpanError> {
        self.slice_utf8(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LineSpan {
    start: u64,
    end: u64,
}

impl LineSpan {
    pub fn new(start: u64, end: u64) -> Result<Self, SpanError> {
        if start == 0 || end == 0 {
            return Err(SpanError::ZeroLine);
        }
        if start > end {
            return Err(SpanError::Reversed);
        }
        Ok(Self { start, end })
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn end(self) -> u64 {
        self.end
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SpanError {
    #[error("span start exceeds span end")]
    Reversed,
    #[error("line spans are one-based")]
    ZeroLine,
    #[error("span is outside the value or splits a UTF-8 scalar")]
    InvalidBoundary,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn byte_spans_are_zero_based_start_inclusive_end_exclusive() {
        let span = ByteSpan::new(1, 3).unwrap();
        assert_eq!(span.slice_str("abcd").unwrap(), "bc");
        assert_eq!(span.slice_bytes(b"abcd").unwrap(), b"bc");
        assert_eq!(ByteSpan::new(2, 2).unwrap().slice_str("abc").unwrap(), "");
    }

    #[test]
    fn byte_spans_reject_reversal_bounds_and_utf8_splits() {
        assert_eq!(ByteSpan::new(2, 1), Err(SpanError::Reversed));
        assert_eq!(
            ByteSpan::new(0, 4).unwrap().slice_str("abc"),
            Err(SpanError::InvalidBoundary)
        );
        assert_eq!(
            ByteSpan::new(1, 2).unwrap().slice_str("é"),
            Err(SpanError::InvalidBoundary)
        );
    }

    #[test]
    fn line_spans_are_one_based_and_end_inclusive() {
        assert_eq!(LineSpan::new(1, 1).unwrap().start(), 1);
        assert_eq!(LineSpan::new(0, 1), Err(SpanError::ZeroLine));
        assert_eq!(LineSpan::new(3, 2), Err(SpanError::Reversed));
    }

    proptest! {
        #[test]
        fn valid_ascii_spans_slice_without_panicking(
            text in "[ -~]{0,128}",
            start in 0usize..128,
            len in 0usize..128,
        ) {
            let start = start.min(text.len());
            let end = start.saturating_add(len).min(text.len());
            let span = ByteSpan::new(start as u64, end as u64).unwrap();
            prop_assert_eq!(span.slice_str(&text).unwrap(), &text[start..end]);
        }
    }
}
