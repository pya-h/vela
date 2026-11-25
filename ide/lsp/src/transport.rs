use std::io::{self, BufRead, Write};
use serde::Serialize;
use serde_json::Value;

use crate::protocol::RawMessage;

pub struct Transport<R, W> {
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> Transport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Transport { reader, writer }
    }

    pub fn read_message(&mut self) -> io::Result<Option<RawMessage>> {
        let mut content_length = None;
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = self.reader.read_line(&mut line)?;
            if bytes_read == 0 {
                return Ok(None);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }

            if let Some((name, value)) = trimmed.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                let Ok(len) = value.trim().parse::<usize>() else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid Content-Length header",
                    ));
                };
                content_length = Some(len);
            }
        }

        const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
        let Some(len) = content_length else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing Content-Length header",
            ));
        };
        if len > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Content-Length {} exceeds limit of {} bytes", len, MAX_MESSAGE_BYTES),
            ));
        }

        let mut buffer = vec![0u8; len];
        self.reader.read_exact(&mut buffer)?;

        let raw: RawMessage = serde_json::from_slice(&buffer).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
        })?;

        Ok(Some(raw))
    }

    pub fn write_response<T: Serialize>(&mut self, value: &T) -> io::Result<()> {
        let body = serde_json::to_string(value).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
        })?;
        write!(self.writer, "Content-Length: {}\r\n\r\n{}", body.len(), body)?;
        self.writer.flush()
    }

    pub fn write_notification(&mut self, method: &'static str, params: Value) -> io::Result<()> {
        let notif = crate::protocol::Notification {
            jsonrpc: "2.0",
            method,
            params,
        };
        self.write_response(&notif)
    }
}
