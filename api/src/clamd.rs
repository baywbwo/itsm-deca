//! Minimal clamd client (`zINSTREAM`) over TCP or a Unix socket. Every path that is not an
//! explicit `OK` verdict is fail-closed: the caller never binds such a file.
//!
//! Outcomes are kept apart on purpose: `Unavailable` (could not reach clamd), `Failed`
//! (clamd reached but no verdict: protocol error, size limit, timeout) and `Infected`.
//! The signature name is never returned or logged.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};

use crate::config::ClamdAddr;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const SCAN_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_BYTES: usize = 64 * 1024;
const MAX_REPLY_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOutcome {
    Clean,
    Infected,
    Failed,
    Unavailable,
}

impl ScanOutcome {
    /// Value stored in `attachments.scan_status`.
    pub fn status_code(self) -> &'static str {
        match self {
            ScanOutcome::Clean => "clean",
            ScanOutcome::Infected => "infected",
            ScanOutcome::Failed => "scan_error",
            ScanOutcome::Unavailable => "scanner_unavailable",
        }
    }
}

/// Scans the file at `path` (already fully written) with clamd.
pub async fn scan_file(addr: &ClamdAddr, path: &Path) -> ScanOutcome {
    let outcome = match addr {
        ClamdAddr::Tcp(hostport) => {
            match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(hostport.as_str())).await
            {
                Ok(Ok(s)) => scan_with_timeout(s, path).await,
                _ => ScanOutcome::Unavailable,
            }
        }
        ClamdAddr::Unix(sock) => {
            match tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(sock)).await {
                Ok(Ok(s)) => scan_with_timeout(s, path).await,
                _ => ScanOutcome::Unavailable,
            }
        }
    };
    tracing::info!(scan.outcome = outcome.status_code(), "attachment scanned");
    outcome
}

async fn scan_with_timeout<S: AsyncRead + AsyncWrite + Unpin>(s: S, path: &Path) -> ScanOutcome {
    match tokio::time::timeout(SCAN_TIMEOUT, instream(s, path)).await {
        Ok(Ok(reply)) => parse_reply(&reply),
        // I/O error after connecting (clamd closed the stream, e.g. StreamMaxLength) or timeout.
        Ok(Err(_)) | Err(_) => ScanOutcome::Failed,
    }
}

async fn instream<S: AsyncRead + AsyncWrite + Unpin>(
    mut s: S,
    path: &Path,
) -> std::io::Result<Vec<u8>> {
    let mut file = tokio::fs::File::open(path).await?;
    s.write_all(b"zINSTREAM\0").await?;
    let mut buf = vec![0u8; CHUNK_BYTES];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let len = u32::try_from(n).map_err(std::io::Error::other)?;
        s.write_all(&len.to_be_bytes()).await?;
        s.write_all(&buf[..n]).await?;
    }
    s.write_all(&0u32.to_be_bytes()).await?;
    s.flush().await?;

    let mut reply = Vec::with_capacity(64);
    let mut byte = [0u8; 256];
    loop {
        let n = s.read(&mut byte).await?;
        if n == 0 {
            break;
        }
        reply.extend_from_slice(&byte[..n]);
        if reply.contains(&0) || reply.len() > MAX_REPLY_BYTES {
            break;
        }
    }
    Ok(reply)
}

/// `stream: OK` → clean; `stream: <sig> FOUND` → infected; anything else (`… ERROR`,
/// empty, oversized, not UTF-8) → failed.
pub fn parse_reply(reply: &[u8]) -> ScanOutcome {
    if reply.len() > MAX_REPLY_BYTES {
        return ScanOutcome::Failed;
    }
    let end = reply.iter().position(|b| *b == 0).unwrap_or(reply.len());
    let Ok(text) = std::str::from_utf8(&reply[..end]) else {
        return ScanOutcome::Failed;
    };
    let text = text.trim();
    let Some(verdict) = text.strip_prefix("stream:").map(str::trim) else {
        return ScanOutcome::Failed;
    };
    if verdict == "OK" {
        ScanOutcome::Clean
    } else if verdict.ends_with(" FOUND") && !verdict.ends_with("ERROR") {
        ScanOutcome::Infected
    } else {
        ScanOutcome::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_map_to_distinct_fail_closed_outcomes() {
        assert_eq!(parse_reply(b"stream: OK\0"), ScanOutcome::Clean);
        assert_eq!(
            parse_reply(b"stream: Win.Test.EICAR_HDB-1 FOUND\0"),
            ScanOutcome::Infected
        );
        for bad in [
            &b"INSTREAM size limit exceeded. ERROR\0"[..],
            b"stream: Can't allocate memory ERROR\0",
            b"",
            b"OK",
            b"stream: \xff\0",
            b"stream: OKAY\0",
        ] {
            assert_eq!(parse_reply(bad), ScanOutcome::Failed, "{bad:?}");
        }
        assert_eq!(parse_reply(&[b'a'; 2000]), ScanOutcome::Failed);
    }

    #[tokio::test]
    async fn unreachable_scanner_is_unavailable_not_clean() {
        let dir = std::env::temp_dir().join(format!("itsm-clamd-{}", uuid::Uuid::new_v4()));
        let missing = ClamdAddr::Unix(dir.join("no.sock"));
        assert_eq!(
            scan_file(&missing, Path::new("/nonexistent")).await,
            ScanOutcome::Unavailable
        );
    }
}
