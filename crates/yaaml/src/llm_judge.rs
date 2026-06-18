use std::env;

use serde_json::Value;
use yaaml_core::Config;
use yaaml_llm::anthropic::{AnthropicMessageClient, AnthropicMessageConfig};
use yaaml_llm::openai::{OpenAiMessageClient, OpenAiMessageConfig};
use yaaml_llm::{ProviderError, ReqwestTransport};

pub enum JudgeClient {
    Anthropic(AnthropicMessageClient<ReqwestTransport>),
    OpenAi(OpenAiMessageClient<ReqwestTransport>),
}

impl JudgeClient {
    pub fn from_config(config: &Config, disabled: bool) -> Option<Self> {
        if disabled || env::var(&config.eval_judge_api_key_env).is_err() {
            return None;
        }
        match config.eval_judge_provider.as_str() {
            "anthropic" => Some(Self::Anthropic(AnthropicMessageClient::new(
                AnthropicMessageConfig::judge_from_config(config),
                ReqwestTransport::default(),
            ))),
            "openai" => Some(Self::OpenAi(OpenAiMessageClient::new(
                OpenAiMessageConfig::judge_from_config(config),
                ReqwestTransport::default(),
            ))),
            _ => None,
        }
    }

    pub fn structured_json(&self, system: &str, prompt: &str) -> Result<Value, ProviderError> {
        match self {
            Self::Anthropic(client) => client.structured_json(system, prompt),
            Self::OpenAi(client) => client.structured_json(system, prompt),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use yaaml_core::Config;

    use super::*;

    #[test]
    fn config_selects_anthropic_judge_client() {
        env::set_var("YAAML_TEST_ANTHROPIC_JUDGE_KEY", "test-key");
        let config = Config {
            eval_judge_provider: "anthropic".to_string(),
            eval_judge_api_key_env: "YAAML_TEST_ANTHROPIC_JUDGE_KEY".to_string(),
            ..Config::default()
        };

        let client = JudgeClient::from_config(&config, false).unwrap();

        assert!(matches!(client, JudgeClient::Anthropic(_)));
    }

    #[test]
    fn config_selects_openai_judge_client() {
        env::set_var("YAAML_TEST_OPENAI_JUDGE_KEY", "test-key");
        let config = Config {
            eval_judge_provider: "openai".to_string(),
            eval_judge_api_key_env: "YAAML_TEST_OPENAI_JUDGE_KEY".to_string(),
            ..Config::default()
        };

        let client = JudgeClient::from_config(&config, false).unwrap();

        assert!(matches!(client, JudgeClient::OpenAi(_)));
    }

    #[test]
    fn missing_key_disables_judge_client() {
        let config = Config {
            eval_judge_api_key_env: "YAAML_TEST_MISSING_JUDGE_KEY".to_string(),
            ..Config::default()
        };

        assert!(JudgeClient::from_config(&config, false).is_none());
    }
}
