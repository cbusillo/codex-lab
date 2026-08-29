use std::error::Error;
use std::fmt;
use std::io::Read;
use std::io::Write;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    EmptyFrame,
    FrameTooLarge,
    InvalidJson,
    Io,
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyFrame => "owner-control IPC frame is empty",
            Self::FrameTooLarge => "owner-control IPC frame exceeds the size limit",
            Self::InvalidJson => "owner-control IPC frame contains invalid JSON",
            Self::Io => "owner-control IPC frame I/O failed",
        };
        formatter.write_str(message)
    }
}

impl Error for FrameError {}

pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, FrameError> {
    let mut length_bytes = [0_u8; 4];
    reader
        .read_exact(&mut length_bytes)
        .map_err(|_| FrameError::Io)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 {
        return Err(FrameError::EmptyFrame);
    }
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge);
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|_| FrameError::Io)?;
    serde_json::from_slice(&payload).map_err(|_| FrameError::InvalidJson)
}

pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(value).map_err(|_| FrameError::InvalidJson)?;
    if payload.is_empty() {
        return Err(FrameError::EmptyFrame);
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::FrameTooLarge)?;
    writer
        .write_all(&length.to_be_bytes())
        .map_err(|_| FrameError::Io)?;
    writer.write_all(&payload).map_err(|_| FrameError::Io)?;
    writer.flush().map_err(|_| FrameError::Io)
}
