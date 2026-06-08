use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

use crate::error::ProviderError;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub trait HttpTransport {
    fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError>;
}

#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    timeout: Duration,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
        }
    }
}

impl ReqwestTransport {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl HttpTransport for ReqwestTransport {
    fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|error| ProviderError::Transport(error.to_string()))?;
        let mut builder = client.post(&request.url).body(request.body.to_string());
        for (key, value) in &request.headers {
            builder = builder.header(key, value);
        }
        let response = builder
            .send()
            .map_err(|error| ProviderError::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .map_err(|error| ProviderError::Transport(error.to_string()))?;

        Ok(HttpResponse { status, body })
    }
}
