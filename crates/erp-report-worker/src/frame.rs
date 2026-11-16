// SPDX-License-Identifier: MIT OR Apache-2.0
//! Frames: a four-byte big-endian length, then that many bytes. A message frame holds one CBOR value;
//! the frame after a `Rendered` reply holds the PDF bytes as they are.
use serde::{Serialize, de::DeserializeOwned};
use std::io::{Cursor, Read, Write};

/// Version both sides name in `hello` and `ready`. A worker of another version is not used.
pub const PROTOCOL_VERSION: u32 = 2; // 2: block spacing, fill, edge rules and table look in the style frame
/// Cap of the `hello` frame, the only frame read before the limits are known.
pub const HELLO_FRAME_BYTES: usize = 64 * 1024;
/// Cap of a reply message frame (not of the PDF frame, which the `pdf_bytes` limit bounds).
pub const REPLY_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Why a frame was not read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    /// The length prefix (or the encoded message) is over the cap; the body was not read.
    #[error("frame of {actual} bytes is over the cap of {max} bytes")]
    TooLarge { actual: usize, max: usize },
    /// The stream ended inside a frame.
    #[error("frame ended after {got} of {expected} bytes")]
    Truncated { got: usize, expected: usize },
    /// The body is not one known message. `reason` is the decoder's text and can quote the frame.
    #[error("frame body is not a known message")]
    Malformed { reason: String },
    #[error("frame I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// Write one frame and flush it.
pub fn write_frame(output: &mut impl Write, body: &[u8]) -> Result<(), FrameError> {
    let length = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge {
        actual: body.len(),
        max: u32::MAX as usize,
    })?;
    output.write_all(&length.to_be_bytes())?;
    output.write_all(body)?;
    output.flush()?;
    Ok(())
}

/// Read one frame of at most `max` bytes. `None` is a stream that ended between two frames.
pub fn read_frame(input: &mut impl Read, max: usize) -> Result<Option<Vec<u8>>, FrameError> {
    let mut prefix = [0u8; 4];
    let mut got = 0;
    while got < prefix.len() {
        match input.read(&mut prefix[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => {
                return Err(FrameError::Truncated {
                    got,
                    expected: prefix.len(),
                });
            }
            Ok(read) => got += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    let expected = u32::from_be_bytes(prefix) as usize;
    if expected > max {
        return Err(FrameError::TooLarge {
            actual: expected,
            max,
        });
    }
    // The buffer grows with the bytes that arrive: a length prefix alone reserves nothing.
    let mut body = Vec::new();
    input.take(expected as u64).read_to_end(&mut body)?;
    if body.len() != expected {
        return Err(FrameError::Truncated {
            got: body.len(),
            expected,
        });
    }
    Ok(Some(body))
}

/// Encode one message; refuse it when it is over `max` bytes.
pub fn encode<T: Serialize>(message: &T, max: usize) -> Result<Vec<u8>, FrameError> {
    let mut body = Vec::new();
    ciborium::ser::into_writer(message, &mut body).map_err(|error| FrameError::Malformed {
        reason: format!("{error:?}"),
    })?;
    if body.len() > max {
        return Err(FrameError::TooLarge {
            actual: body.len(),
            max,
        });
    }
    Ok(body)
}

/// Decode exactly one message; bytes after it are refused.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, FrameError> {
    let mut cursor = Cursor::new(body);
    let message =
        ciborium::de::from_reader(&mut cursor).map_err(|error| FrameError::Malformed {
            reason: format!("{error:?}"),
        })?;
    if cursor.position() != body.len() as u64 {
        return Err(FrameError::Malformed {
            reason: "bytes after the message".into(),
        });
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "test assertions")]
    use super::*;

    #[test]
    fn a_frame_round_trips_and_a_clean_end_is_not_an_error() {
        let mut pipe = Vec::new();
        write_frame(&mut pipe, b"abc").unwrap();
        write_frame(&mut pipe, b"").unwrap();
        let mut input = pipe.as_slice();
        assert_eq!(read_frame(&mut input, 8).unwrap().unwrap(), b"abc");
        assert_eq!(read_frame(&mut input, 8).unwrap().unwrap(), b"");
        assert!(read_frame(&mut input, 8).unwrap().is_none());
    }

    #[test]
    fn a_length_over_the_cap_is_refused_before_the_body_is_read() {
        let mut input: &[u8] = &[0xff, 0xff, 0xff, 0xff, 1, 2];
        let error = read_frame(&mut input, 1024).unwrap_err();
        assert!(matches!(
            error,
            FrameError::TooLarge {
                actual: 0xffff_ffff,
                max: 1024
            }
        ));
        assert_eq!(
            error.to_string(),
            "frame of 4294967295 bytes is over the cap of 1024 bytes"
        );
        assert_eq!(input, [1, 2]);
    }

    #[test]
    fn a_stream_that_ends_inside_a_frame_is_truncated() {
        let mut input: &[u8] = &[0, 0];
        assert!(matches!(
            read_frame(&mut input, 8).unwrap_err(),
            FrameError::Truncated {
                got: 2,
                expected: 4
            }
        ));
        let mut input: &[u8] = &[0, 0, 0, 5, 1, 2];
        let error = read_frame(&mut input, 8).unwrap_err();
        assert!(matches!(
            error,
            FrameError::Truncated {
                got: 2,
                expected: 5
            }
        ));
        assert_eq!(error.to_string(), "frame ended after 2 of 5 bytes");
    }

    #[test]
    fn decoding_refuses_trailing_bytes_and_oversized_messages() {
        let mut body = encode(&7u32, 16).unwrap();
        assert_eq!(decode::<u32>(&body).unwrap(), 7);
        body.push(0);
        assert!(matches!(
            decode::<u32>(&body).unwrap_err(),
            FrameError::Malformed { reason } if reason == "bytes after the message"
        ));
        assert!(matches!(
            encode(&"long text", 4).unwrap_err(),
            FrameError::TooLarge { actual: 10, max: 4 }
        ));
    }
}
