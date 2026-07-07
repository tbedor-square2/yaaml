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

/// Aligned per-memory judge instrument (`query_memory_rubric`), adopted after
/// the 2026-07 judge calibration: it judges pre-injection usefulness from the
/// current turn and stored memory only. Shared by the offline eval path and
/// the daemon's online per-memory recall evals so the instrument cannot
/// drift. Abstention judging stays on the after-the-fact prompt because it
/// depends on subsequent conversation.
pub fn candidate_judge_system_prompt() -> &'static str {
    concat!(
        "You are scoring memory recall quality for an AI coding agent before context injection. ",
        "Decide whether the stored memory would be useful context for the current turn. ",
        "Use only the current turn, memory, and rubric. ",
        "Return only JSON with fields score and rationale. score must be a string from \"1\" to \"5\"."
    )
}

pub fn candidate_judge_prompt(turn_text: &str, memory_title: &str, memory_body: &str) -> String {
    format!(
        concat!(
            "Score whether this stored memory would be useful context for answering the current turn.\n\n",
            "Return only JSON with this exact shape:\n",
            "{{\"score\": \"<integer 1-5>\", \"rationale\": \"<one short sentence>\"}}\n\n",
            "Rubric:\n",
            "- 5: directly useful and actionable for the current turn.\n",
            "- 4: useful context with minor gaps or extra filtering needed.\n",
            "- 3: mixed or marginal; some relevance but not clearly worth recall.\n",
            "- 2: weak, stale, or mostly irrelevant.\n",
            "- 1: distracting, wrong-context, or actively harmful.\n\n",
            "Current turn:\n",
            "```text\n",
            "{}\n",
            "```\n\n",
            "Stored memory title:\n",
            "```text\n",
            "{}\n",
            "```\n\n",
            "Stored memory body:\n",
            "```text\n",
            "{}\n",
            "```\n"
        ),
        truncate_judge_text(turn_text, 4_000),
        truncate_judge_text(memory_title, 500),
        truncate_judge_text(memory_body, 4_000)
    )
}

fn truncate_judge_text(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
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
