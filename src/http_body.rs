use reqwest::Response;

pub(crate) const JSON_BODY_LIMIT: usize = 4 * 1024 * 1024;

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

pub(crate) fn ensure_https(response: &Response) -> Result<(), String> {
    if response.url().scheme() == "https" {
        Ok(())
    } else {
        Err("response redirected to a non-HTTPS URL".to_string())
    }
}
