use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryClass {
    Retryable,
    NonRetryable,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("missing API key in environment variable {env_var}")]
    MissingApiKey { env_var: String },
    #[error("transport error: {0}")]
    Transport(String),
    #[error("provider returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("failed to parse provider response: {0}")]
    Parse(String),
}

impl ProviderError {
    pub fn retry_class(&self) -> RetryClass {
        match self {
            Self::MissingApiKey { .. } | Self::Parse(_) => RetryClass::NonRetryable,
            Self::Transport(_) => RetryClass::Retryable,
            Self::Http { status, .. } => classify_http_status(*status),
        }
    }
}

pub fn classify_http_status(status: u16) -> RetryClass {
    match status {
        408 | 409 | 425 | 429 => RetryClass::Retryable,
        500..=599 => RetryClass::Retryable,
        _ => RetryClass::NonRetryable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_retryable_statuses() {
        for status in [408, 409, 425, 429, 500, 502, 503, 599] {
            assert_eq!(classify_http_status(status), RetryClass::Retryable);
        }
    }

    #[test]
    fn classifies_non_retryable_statuses() {
        for status in [400, 401, 403, 404, 422] {
            assert_eq!(classify_http_status(status), RetryClass::NonRetryable);
        }
    }
}
