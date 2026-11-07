//! Bounded HTTP body reading shared by update checks.

use reqwest::Response;

/// Maximum size for a GitHub release metadata response.
pub(crate) const JSON_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Read a response body with a hard size cap, independent of
/// `Content-Length` (chunked responses have none).
pub(crate) async fn read_limited(mut response: Response, max: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|err| err.to_string())? {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err("response body is too large".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// GitHub API requests must stay on HTTPS after redirects.
pub(crate) fn ensure_https(response: &Response) -> Result<(), String> {
    if response.url().scheme() == "https" {
        Ok(())
    } else {
        Err("response redirected to a non-HTTPS URL".to_string())
    }
}
