//! The error type shared by the encoder, decoder, and packet utilities.

use std::fmt;

/// Why an encoder, decoder, or packet operation failed.
///
/// The variants mirror libopus' error codes ([`Error::code`] returns the libopus
/// integer), and each carries a short static description of the specific
/// condition. `Display` prints that description, so code that formatted the
/// former `&'static str` errors produces identical text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Error {
    /// An argument was outside its valid range (libopus `OPUS_BAD_ARG`).
    BadArg(&'static str),
    /// The caller's output buffer cannot hold the result
    /// (libopus `OPUS_BUFFER_TOO_SMALL`).
    BufferTooSmall(&'static str),
    /// The input packet is malformed, truncated, or otherwise not valid Opus
    /// (libopus `OPUS_INVALID_PACKET`).
    InvalidPacket(&'static str),
    /// An internal codec stage failed (libopus `OPUS_INTERNAL_ERROR`).
    Internal(&'static str),
}

impl Error {
    /// The equivalent libopus error code (always negative).
    #[must_use]
    pub const fn code(&self) -> i32 {
        match self {
            Self::BadArg(_) => -1,
            Self::BufferTooSmall(_) => -2,
            Self::Internal(_) => -3,
            Self::InvalidPacket(_) => -4,
        }
    }

    /// The description of the specific condition.
    #[must_use]
    pub const fn message(&self) -> &'static str {
        match self {
            Self::BadArg(m)
            | Self::BufferTooSmall(m)
            | Self::InvalidPacket(m)
            | Self::Internal(m) => m,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for Error {}
