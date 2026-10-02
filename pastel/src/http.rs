//! Pastel's outbound HTTP conventions: per-use timeouts and its User-Agent.

use crate::{Result, USER_AGENT};
use reqwest::blocking::{Client, Response};
use std::io::Read;
use std::time::Duration;

pub fn client(timeout: Duration) -> Result<Client> {
    Ok(Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()?)
}

/// Sends a GET and fails unless the server answers 200 OK.
pub fn get(client: &Client, url: &str) -> Result<Response> {
    let response = client.get(url).send()?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(format!("GET {url}: {}", response.status()).into());
    }
    Ok(response)
}

/// Reads a response body, failing once it grows past `limit` bytes.
pub fn read_limited(response: Response, limit: u64, what: &str) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(format!("{what} is too large (limit {limit} bytes)").into());
    }
    let mut body = Vec::new();
    response.take(limit + 1).read_to_end(&mut body)?;
    if body.len() as u64 > limit {
        return Err(format!("{what} is too large (limit {limit} bytes)").into());
    }
    Ok(body)
}
