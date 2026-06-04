"""Configuration management for YAAML."""

import logging
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)


@dataclass
class Config:
    turns_between_memory: int = 10
    consolidation_dark_period_seconds: int = 300
    recall_result_limit: int = 5
    recall_candidate_pool: int = 20
    recall_distance_threshold: float = 0.8
    recall_project_boost: float = 1.3
    db_path: Path = field(default_factory=lambda: Path("~/.yaaml/yaaml.db").expanduser())
    chroma_path: Path = field(default_factory=lambda: Path("~/.yaaml/chroma").expanduser())
    embedding_model: str = "text-embedding-3-small"
    summary_model: str = "claude-haiku-4-5-20251001"
    consolidation_model: str = "claude-haiku-4-5-20251001"
    max_memory_length: int = 12000
    max_formulation_tokens: int = 32000
    tool_call_truncation_chars: int = 500
    recall_classifier_enabled: bool = True


def _load_toml_file(path: Path) -> dict[str, Any]:
    """Load a TOML file, returning empty dict if it doesn't exist."""
    if not path.exists():
        return {}
    with open(path, "rb") as f:
        return tomllib.load(f)


def _apply_dict_to_config(config: Config, data: dict[str, Any]) -> None:
    """Apply a dict of config values to a Config instance."""
    for key, value in data.items():
        if hasattr(config, key):
            # Handle Path fields
            if key in ("db_path", "chroma_path"):
                value = Path(str(value)).expanduser()
            setattr(config, key, value)
        else:
            logger.warning("Unknown config key %r — ignoring.", key)


def load_config(project_dir: Path | None) -> Config:
    """Load config from ~/.yaaml/config.toml, overlaid with project-level config.

    Args:
        project_dir: Optional project directory to look for .yaaml/config.toml.

    Returns:
        Merged Config instance.
    """
    config = Config()

    # Load user-level config
    user_config_path = Path("~/.yaaml/config.toml").expanduser()
    user_data = _load_toml_file(user_config_path)
    _apply_dict_to_config(config, user_data)

    # Overlay project-level config if present
    if project_dir is not None:
        project_config_path = project_dir / ".yaaml" / "config.toml"
        project_data = _load_toml_file(project_config_path)
        _apply_dict_to_config(config, project_data)

    # Expand ~ in path fields
    config.db_path = Path(str(config.db_path)).expanduser()
    config.chroma_path = Path(str(config.chroma_path)).expanduser()

    return config
