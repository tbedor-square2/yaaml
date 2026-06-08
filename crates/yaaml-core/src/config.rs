use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml::Value;

use crate::paths::{expand_tilde, home_dir, PathError};

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to deserialize config: {0}")]
    Deserialize(#[from] toml::de::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub user_config: PathBuf,
    pub project_config: PathBuf,
}

impl ConfigPaths {
    pub fn for_cwd(cwd: &Path) -> Result<Self, ConfigError> {
        Ok(Self {
            user_config: home_dir()?.join(".yaaml").join("config.toml"),
            project_config: cwd.join(".yaaml").join("config.toml"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub turns_between_memory: u64,
    pub session_idle_memory_seconds: u64,
    pub consolidation_dark_period_seconds: u64,
    pub recall_result_limit: usize,
    pub recall_candidate_pool: usize,
    pub recall_live_turn_window: usize,
    pub recall_query_max_chars: usize,
    pub recall_similarity_threshold: f32,
    pub recall_project_tiebreaker: bool,
    pub recall_project_score_bonus: f32,
    pub recall_dir: String,
    pub db_path: String,
    pub vector_index_backend: String,
    pub vector_index_path: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_api_key_env: String,
    pub embedding_base_url: Option<String>,
    pub summary_provider: String,
    pub summary_model: String,
    pub summary_api_key_env: String,
    pub summary_base_url: Option<String>,
    pub consolidation_provider: String,
    pub consolidation_model: String,
    pub consolidation_api_key_env: String,
    pub consolidation_base_url: Option<String>,
    pub memory_cluster_distance_threshold: f32,
    pub memory_cluster_min_size: usize,
    pub memory_cluster_max_size: usize,
    pub max_memory_length: usize,
    pub max_formulation_tokens: usize,
    pub tool_call_truncation_chars: usize,
    pub recall_classifier_enabled: bool,
    pub backlog_max_concurrent_remote_jobs: usize,
    pub backlog_newest_first: bool,
    pub backlog_formulation_turn_window: usize,
    pub eval_judge_provider: String,
    pub eval_judge_model: String,
    pub eval_judge_api_key_env: String,
    pub eval_judge_base_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            turns_between_memory: 10,
            session_idle_memory_seconds: 600,
            consolidation_dark_period_seconds: 300,
            recall_result_limit: 5,
            recall_candidate_pool: 20,
            recall_live_turn_window: 3,
            recall_query_max_chars: 12_000,
            recall_similarity_threshold: 0.3,
            recall_project_tiebreaker: true,
            recall_project_score_bonus: 0.05,
            recall_dir: "~/.yaaml/recall".to_string(),
            db_path: "~/.yaaml/yaaml.db".to_string(),
            vector_index_backend: "sqlite-exact".to_string(),
            vector_index_path: "~/.yaaml/vector-index".to_string(),
            embedding_provider: "openai".to_string(),
            embedding_model: "text-embedding-3-small".to_string(),
            embedding_api_key_env: "OPENAI_API_KEY".to_string(),
            embedding_base_url: None,
            summary_provider: "anthropic".to_string(),
            summary_model: "claude-haiku-4-5-20251001".to_string(),
            summary_api_key_env: "ANTHROPIC_API_KEY".to_string(),
            summary_base_url: None,
            consolidation_provider: "anthropic".to_string(),
            consolidation_model: "claude-haiku-4-5-20251001".to_string(),
            consolidation_api_key_env: "ANTHROPIC_API_KEY".to_string(),
            consolidation_base_url: None,
            memory_cluster_distance_threshold: 0.21125,
            memory_cluster_min_size: 3,
            memory_cluster_max_size: 5,
            max_memory_length: 12_000,
            max_formulation_tokens: 32_000,
            tool_call_truncation_chars: 500,
            recall_classifier_enabled: true,
            backlog_max_concurrent_remote_jobs: 1,
            backlog_newest_first: true,
            backlog_formulation_turn_window: 10,
            eval_judge_provider: "anthropic".to_string(),
            eval_judge_model: "claude-haiku-4-5-20251001".to_string(),
            eval_judge_api_key_env: "ANTHROPIC_API_KEY".to_string(),
            eval_judge_base_url: None,
        }
    }
}

impl Config {
    pub fn load_for_cwd(cwd: &Path) -> Result<Self, ConfigError> {
        Self::load_from_paths(ConfigPaths::for_cwd(cwd)?)
    }

    pub fn load_from_paths(paths: ConfigPaths) -> Result<Self, ConfigError> {
        let mut merged = Value::Table(Default::default());

        if paths.user_config.exists() {
            merge_value(&mut merged, read_toml(&paths.user_config)?);
        }

        if paths.project_config.exists() {
            merge_value(&mut merged, read_toml(&paths.project_config)?);
        }

        Ok(merged.try_into()?)
    }

    pub fn db_path(&self) -> Result<PathBuf, ConfigError> {
        Ok(expand_tilde(&self.db_path)?)
    }

    pub fn recall_dir(&self) -> Result<PathBuf, ConfigError> {
        Ok(expand_tilde(&self.recall_dir)?)
    }
}

fn read_toml(path: &Path) -> Result<Value, ConfigError> {
    let text = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    text.parse::<Value>().map_err(|source| ConfigError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn merge_value(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Table(base), Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(&key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn defaults_match_requirements() {
        let config = Config::default();

        assert_eq!(config.turns_between_memory, 10);
        assert_eq!(config.session_idle_memory_seconds, 600);
        assert_eq!(config.recall_result_limit, 5);
        assert_eq!(config.recall_live_turn_window, 3);
        assert_eq!(config.recall_query_max_chars, 12_000);
        assert_eq!(config.recall_similarity_threshold, 0.3);
        assert_eq!(config.recall_dir, "~/.yaaml/recall");
        assert_eq!(config.vector_index_backend, "sqlite-exact");
        assert_eq!(config.memory_cluster_distance_threshold, 0.21125);
        assert_eq!(config.backlog_formulation_turn_window, 10);
        assert_eq!(config.eval_judge_provider, "anthropic");
        assert_eq!(config.eval_judge_api_key_env, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn project_override_wins_over_user_config() {
        let tmp = TempDir::new().unwrap();
        let user_config = tmp.path().join("user.toml");
        let project_config = tmp.path().join("project.toml");
        fs::write(
            &user_config,
            r#"
turns_between_memory = 20
recall_result_limit = 4
summary_model = "user-model"
"#,
        )
        .unwrap();
        fs::write(
            &project_config,
            r#"
recall_result_limit = 8
"#,
        )
        .unwrap();

        let config = Config::load_from_paths(ConfigPaths {
            user_config,
            project_config,
        })
        .unwrap();

        assert_eq!(config.turns_between_memory, 20);
        assert_eq!(config.recall_result_limit, 8);
        assert_eq!(config.summary_model, "user-model");
    }
}
