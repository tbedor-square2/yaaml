use std::collections::BTreeMap;
use std::env;

use serde::Deserialize;
use serde_json::{json, Value};
use yaaml_core::Config;

use crate::error::ProviderError;
use crate::transport::{HttpRequest, HttpTransport};

const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct AnthropicMessageConfig {
    pub model: String,
    pub api_key_env: String,
    pub base_url: String,
    pub max_tokens: u64,
}

impl AnthropicMessageConfig {
    pub fn summary_from_config(config: &Config) -> Self {
        Self {
            model: config.summary_model.clone(),
            api_key_env: config.summary_api_key_env.clone(),
            base_url: config
                .summary_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string()),
            max_tokens: 4096,
        }
    }

    pub fn consolidation_from_config(config: &Config) -> Self {
        Self {
            model: config.consolidation_model.clone(),
            api_key_env: config.consolidation_api_key_env.clone(),
            base_url: config
                .consolidation_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string()),
            max_tokens: 4096,
        }
    }

    pub fn judge_from_config(config: &Config) -> Self {
        Self {
            model: config.eval_judge_model.clone(),
            api_key_env: config.eval_judge_api_key_env.clone(),
            base_url: config
                .eval_judge_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string()),
            max_tokens: 1024,
        }
    }
}

pub struct AnthropicMessageClient<T> {
    config: AnthropicMessageConfig,
    transport: T,
}

impl<T> AnthropicMessageClient<T>
where
    T: HttpTransport,
{
    pub fn new(config: AnthropicMessageConfig, transport: T) -> Self {
        Self { config, transport }
    }

    pub fn structured_json(&self, system: &str, prompt: &str) -> Result<Value, ProviderError> {
        let text = self.message_text(system, prompt)?;
        parse_json_from_text(&text)
    }

    pub fn message_text(&self, system: &str, prompt: &str) -> Result<String, ProviderError> {
        let api_key =
            env::var(&self.config.api_key_env).map_err(|_| ProviderError::MissingApiKey {
                env_var: self.config.api_key_env.clone(),
            })?;
        let mut headers = BTreeMap::new();
        headers.insert("x-api-key".to_string(), api_key);
        headers.insert(
            "anthropic-version".to_string(),
            ANTHROPIC_VERSION.to_string(),
        );
        headers.insert("Content-Type".to_string(), "application/json".to_string());

        let request = HttpRequest {
            url: format!("{}/v1/messages", self.config.base_url.trim_end_matches('/')),
            headers,
            body: json!({
                "model": self.config.model,
                "max_tokens": self.config.max_tokens,
                "system": system,
                "messages": [
                    {"role": "user", "content": prompt}
                ],
            }),
        };
        let response = self.transport.post_json(request)?;
        if !(200..300).contains(&response.status) {
            return Err(ProviderError::Http {
                status: response.status,
                body: response.body,
            });
        }
        parse_message_text(&response.body)
    }
}

#[derive(Debug, Deserialize)]
struct MessageResponse {
    content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

pub fn parse_message_text(body: &str) -> Result<String, ProviderError> {
    let parsed: MessageResponse =
        serde_json::from_str(body).map_err(|error| ProviderError::Parse(error.to_string()))?;
    let text = parsed
        .content
        .into_iter()
        .filter(|block| block.kind == "text")
        .filter_map(|block| block.text)
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        Err(ProviderError::Parse(
            "missing text content block".to_string(),
        ))
    } else {
        Ok(text)
    }
}

pub fn parse_json_from_text(text: &str) -> Result<Value, ProviderError> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }

    let Some(start) = trimmed.find(['{', '[']) else {
        return Err(ProviderError::Parse(
            "message text did not contain JSON".to_string(),
        ));
    };
    let end = trimmed.rfind(['}', ']']).ok_or_else(|| {
        ProviderError::Parse("message text did not contain complete JSON".to_string())
    })?;
    if end <= start {
        return Err(ProviderError::Parse(
            "message text did not contain complete JSON".to_string(),
        ));
    }
    serde_json::from_str::<Value>(&trimmed[start..=end])
        .map_err(|error| ProviderError::Parse(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::transport::{HttpResponse, HttpTransport};

    use super::*;

    struct MockTransport {
        request: Rc<RefCell<Option<HttpRequest>>>,
        result: Result<HttpResponse, ProviderError>,
    }

    impl Default for MockTransport {
        fn default() -> Self {
            Self {
                request: Rc::new(RefCell::new(None)),
                result: Ok(HttpResponse {
                    status: 200,
                    body: r#"{"content":[{"type":"text","text":"{\"title\":\"Memory\"}"}]}"#
                        .to_string(),
                }),
            }
        }
    }

    impl HttpTransport for MockTransport {
        fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            *self.request.borrow_mut() = Some(request);
            self.result
                .as_ref()
                .map(Clone::clone)
                .map_err(|error| ProviderError::Transport(error.to_string()))
        }
    }

    #[test]
    fn parses_message_text_content() {
        let text = parse_message_text(
            r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"hello"}]}"#,
        )
        .unwrap();

        assert_eq!(text, "hello");
    }

    #[test]
    fn parses_json_from_wrapped_text() {
        let value = parse_json_from_text("Here is JSON:\n{\"title\":\"Memory\"}\nThanks").unwrap();

        assert_eq!(value["title"], "Memory");
    }

    #[test]
    fn missing_api_key_is_non_retryable() {
        let config = AnthropicMessageConfig {
            model: "claude-haiku-4-5-20251001".to_string(),
            api_key_env: "YAAML_TEST_MISSING_ANTHROPIC_KEY".to_string(),
            base_url: "https://example.test".to_string(),
            max_tokens: 100,
        };
        let client = AnthropicMessageClient::new(config, MockTransport::default());

        let error = client.structured_json("system", "prompt").unwrap_err();

        assert!(matches!(error, ProviderError::MissingApiKey { .. }));
        assert_eq!(error.retry_class(), crate::RetryClass::NonRetryable);
    }

    #[test]
    fn sends_anthropic_message_request() {
        env::set_var("YAAML_TEST_ANTHROPIC_KEY", "test-key");
        let transport = MockTransport::default();
        let request_cell = Rc::clone(&transport.request);
        let config = AnthropicMessageConfig {
            model: "claude-haiku-4-5-20251001".to_string(),
            api_key_env: "YAAML_TEST_ANTHROPIC_KEY".to_string(),
            base_url: "https://example.test".to_string(),
            max_tokens: 100,
        };
        let client = AnthropicMessageClient::new(config, transport);

        let value = client.structured_json("system", "prompt").unwrap();

        assert_eq!(value["title"], "Memory");
        let request = request_cell.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.url, "https://example.test/v1/messages");
        assert_eq!(request.body["model"], "claude-haiku-4-5-20251001");
        assert_eq!(request.body["max_tokens"], 100);
        assert_eq!(request.body["system"], "system");
        assert_eq!(request.body["messages"][0]["content"], "prompt");
        assert_eq!(
            request.headers.get("anthropic-version").map(String::as_str),
            Some(ANTHROPIC_VERSION)
        );
    }

    #[test]
    fn http_429_is_retryable() {
        env::set_var("YAAML_TEST_ANTHROPIC_429_KEY", "test-key");
        let transport = MockTransport {
            request: Rc::new(RefCell::new(None)),
            result: Ok(HttpResponse {
                status: 429,
                body: "rate limited".to_string(),
            }),
        };
        let config = AnthropicMessageConfig {
            model: "claude-haiku-4-5-20251001".to_string(),
            api_key_env: "YAAML_TEST_ANTHROPIC_429_KEY".to_string(),
            base_url: "https://example.test".to_string(),
            max_tokens: 100,
        };
        let client = AnthropicMessageClient::new(config, transport);

        let error = client.message_text("system", "prompt").unwrap_err();

        assert!(matches!(error, ProviderError::Http { status: 429, .. }));
        assert_eq!(error.retry_class(), crate::RetryClass::Retryable);
    }

    #[test]
    fn http_401_is_non_retryable() {
        env::set_var("YAAML_TEST_ANTHROPIC_401_KEY", "test-key");
        let transport = MockTransport {
            request: Rc::new(RefCell::new(None)),
            result: Ok(HttpResponse {
                status: 401,
                body: "unauthorized".to_string(),
            }),
        };
        let config = AnthropicMessageConfig {
            model: "claude-haiku-4-5-20251001".to_string(),
            api_key_env: "YAAML_TEST_ANTHROPIC_401_KEY".to_string(),
            base_url: "https://example.test".to_string(),
            max_tokens: 100,
        };
        let client = AnthropicMessageClient::new(config, transport);

        let error = client.message_text("system", "prompt").unwrap_err();

        assert!(matches!(error, ProviderError::Http { status: 401, .. }));
        assert_eq!(error.retry_class(), crate::RetryClass::NonRetryable);
    }

    #[test]
    fn truncated_message_response_is_parse_error() {
        let error =
            parse_message_text(r#"{"content":[{"type":"text","text":"hello"}]"#).unwrap_err();

        assert!(matches!(error, ProviderError::Parse(_)));
        assert_eq!(error.retry_class(), crate::RetryClass::NonRetryable);
    }

    #[test]
    fn missing_text_content_is_parse_error() {
        let error =
            parse_message_text(r#"{"content":[{"type":"tool_use","name":"Bash"}]}"#).unwrap_err();

        assert!(matches!(error, ProviderError::Parse(_)));
    }
}
