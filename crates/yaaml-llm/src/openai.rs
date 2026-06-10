use std::collections::BTreeMap;
use std::env;

use serde::Deserialize;
use serde_json::json;
use yaaml_core::Config;

use crate::error::ProviderError;
use crate::transport::{HttpRequest, HttpTransport};

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com";

#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingConfig {
    pub model: String,
    pub api_key_env: String,
    pub base_url: String,
}

impl OpenAiEmbeddingConfig {
    pub fn from_config(config: &Config) -> Self {
        Self {
            model: config.embedding_model.clone(),
            api_key_env: config.embedding_api_key_env.clone(),
            base_url: config
                .embedding_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string()),
        }
    }
}

pub struct OpenAiEmbeddingClient<T> {
    config: OpenAiEmbeddingConfig,
    transport: T,
}

impl<T> OpenAiEmbeddingClient<T>
where
    T: HttpTransport,
{
    pub fn new(config: OpenAiEmbeddingConfig, transport: T) -> Self {
        Self { config, transport }
    }

    pub fn embed(&self, input: &str) -> Result<Vec<f32>, ProviderError> {
        let api_key =
            env::var(&self.config.api_key_env).map_err(|_| ProviderError::MissingApiKey {
                env_var: self.config.api_key_env.clone(),
            })?;
        let mut headers = BTreeMap::new();
        headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
        headers.insert("Content-Type".to_string(), "application/json".to_string());

        let request = HttpRequest {
            url: format!(
                "{}/v1/embeddings",
                self.config.base_url.trim_end_matches('/')
            ),
            headers,
            body: json!({
                "model": self.config.model,
                "input": input,
            }),
        };
        let response = self.transport.post_json(request)?;
        if !(200..300).contains(&response.status) {
            return Err(ProviderError::Http {
                status: response.status,
                body: response.body,
            });
        }
        parse_embedding_response(&response.body)
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
}

pub fn parse_embedding_response(body: &str) -> Result<Vec<f32>, ProviderError> {
    let parsed: EmbeddingsResponse =
        serde_json::from_str(body).map_err(|error| ProviderError::Parse(error.to_string()))?;
    parsed
        .data
        .into_iter()
        .next()
        .map(|datum| datum.embedding)
        .filter(|embedding| !embedding.is_empty())
        .ok_or_else(|| ProviderError::Parse("missing embedding vector".to_string()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::rc::Rc;
    use std::thread;

    use crate::transport::{HttpResponse, HttpTransport, ReqwestTransport};

    use super::*;

    struct MockTransport {
        request: Rc<RefCell<Option<HttpRequest>>>,
        response: HttpResponse,
    }

    impl Default for MockTransport {
        fn default() -> Self {
            Self {
                request: Rc::new(RefCell::new(None)),
                response: HttpResponse {
                    status: 200,
                    body: r#"{"data":[{"embedding":[0.1,0.2,0.3]}]}"#.to_string(),
                },
            }
        }
    }

    impl HttpTransport for MockTransport {
        fn post_json(&self, request: HttpRequest) -> Result<HttpResponse, ProviderError> {
            *self.request.borrow_mut() = Some(request);
            Ok(self.response.clone())
        }
    }

    #[test]
    fn parses_openai_embedding_response() {
        let embedding =
            parse_embedding_response(r#"{"object":"list","data":[{"embedding":[1.0,2.0]}]}"#)
                .unwrap();

        assert_eq!(embedding, vec![1.0, 2.0]);
    }

    #[test]
    fn missing_api_key_is_non_retryable() {
        let config = OpenAiEmbeddingConfig {
            model: "text-embedding-3-small".to_string(),
            api_key_env: "YAAML_TEST_MISSING_OPENAI_KEY".to_string(),
            base_url: "https://example.test".to_string(),
        };
        let client = OpenAiEmbeddingClient::new(config, MockTransport::default());

        let error = client.embed("hello").unwrap_err();

        assert!(matches!(error, ProviderError::MissingApiKey { .. }));
        assert_eq!(error.retry_class(), crate::RetryClass::NonRetryable);
    }

    #[test]
    fn sends_embedding_request() {
        env::set_var("YAAML_TEST_OPENAI_KEY", "test-key");
        let transport = MockTransport::default();
        let request = transport.request.clone();
        let config = OpenAiEmbeddingConfig {
            model: "text-embedding-3-small".to_string(),
            api_key_env: "YAAML_TEST_OPENAI_KEY".to_string(),
            base_url: "https://example.test".to_string(),
        };
        let client = OpenAiEmbeddingClient::new(config, transport);

        let embedding = client.embed("hello").unwrap();

        assert_eq!(embedding, vec![0.1, 0.2, 0.3]);
        let request = request.borrow();
        let request = request.as_ref().unwrap();
        assert_eq!(request.url, "https://example.test/v1/embeddings");
        assert_eq!(request.body["model"], "text-embedding-3-small");
        assert_eq!(request.body["input"], "hello");
    }

    #[test]
    fn default_config_sets_embedding_model() {
        let config = OpenAiEmbeddingConfig::from_config(&Config::default());

        assert_eq!(config.model, "text-embedding-3-small");
    }

    #[test]
    fn reqwest_transport_sends_model_parameter() {
        env::set_var("YAAML_TEST_OPENAI_KEY", "test-key");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 8192];
            let bytes = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..bytes]).to_string();
            let body = r#"{"data":[{"embedding":[0.1,0.2,0.3]}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            request
        });
        let config = OpenAiEmbeddingConfig {
            model: "text-embedding-3-small".to_string(),
            api_key_env: "YAAML_TEST_OPENAI_KEY".to_string(),
            base_url,
        };
        let client = OpenAiEmbeddingClient::new(config, ReqwestTransport::default());

        let embedding = client.embed("hello").unwrap();
        let request = handle.join().unwrap();

        assert_eq!(embedding, vec![0.1, 0.2, 0.3]);
        assert!(
            request.contains(r#""model":"text-embedding-3-small""#),
            "{request}"
        );
        assert!(request.contains(r#""input":"hello""#), "{request}");
    }
}
