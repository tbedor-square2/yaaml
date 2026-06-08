"""Test harness: generate synthetic Claude Code and Codex JSONL transcripts."""

from __future__ import annotations

import json
import random
import uuid
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path

_LOREM_WORDS = [
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "ex",
    "ea",
    "commodo",
    "consequat",
    "duis",
    "aute",
    "irure",
    "dolor",
    "reprehenderit",
    "voluptate",
    "velit",
    "esse",
    "cillum",
    "dolore",
    "fugiat",
    "nulla",
    "pariatur",
    "excepteur",
    "sint",
    "occaecat",
    "cupidatat",
    "non",
    "proident",
    "culpa",
    "qui",
    "officia",
    "deserunt",
    "mollit",
    "anim",
    "id",
    "est",
    "laborum",
]


def _utcnow() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


class LoremGenerator:
    """Simple lorem ipsum text generator with no external dependencies."""

    def __init__(self, seed: int = 42) -> None:
        self._rng = random.Random(seed)

    def sentence(self) -> str:
        """Return a sentence of 8–15 lorem words."""
        n = self._rng.randint(8, 15)
        words = [self._rng.choice(_LOREM_WORDS) for _ in range(n)]
        words[0] = words[0].capitalize()
        return " ".join(words) + "."

    def paragraph(self, sentences: int = 4) -> str:
        """Return N sentences joined into a paragraph."""
        return " ".join(self.sentence() for _ in range(sentences))


@dataclass
class FakeToolCall:
    """A single fake tool call with name, input, and output."""

    name: str
    input: dict[str, str]
    output: str


@dataclass
class FakeTurn:
    """A fake conversation turn with optional tool calls."""

    user: str
    assistant: str
    tool_calls: list[FakeToolCall] = field(default_factory=list)


class ClaudeCodeSessionWriter:
    """Writes synthetic Claude Code–format JSONL transcript files."""

    def __init__(
        self,
        session_dir: Path,
        project_cwd: str,
        git_branch: str = "main",
    ) -> None:
        self._session_dir = session_dir
        self._project_cwd = project_cwd
        self._git_branch = git_branch
        self._session_uuid = str(uuid.uuid4())
        self._file_path = session_dir / f"{self._session_uuid}.jsonl"
        # Ensure the directory exists
        session_dir.mkdir(parents=True, exist_ok=True)

    @property
    def file_path(self) -> Path:
        """Path to the JSONL file."""
        return self._file_path

    def _append(self, obj: dict[str, object]) -> None:
        with open(self._file_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(obj) + "\n")

    def write_turn(self, turn: FakeTurn) -> None:
        """Append all JSONL lines for a complete turn."""
        now = _utcnow()

        # 1. User message line
        self._append(
            {
                "type": "user",
                "message": {"role": "user", "content": turn.user},
                "uuid": str(uuid.uuid4()),
                "timestamp": now,
                "cwd": self._project_cwd,
                "gitBranch": self._git_branch,
            }
        )

        # 2. Tool call / tool result pairs
        for tc in turn.tool_calls:
            tool_id = str(uuid.uuid4())

            # Assistant tool_use line
            self._append(
                {
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "tool_use",
                                "id": tool_id,
                                "name": tc.name,
                                "input": tc.input,
                            }
                        ],
                    },
                    "uuid": str(uuid.uuid4()),
                    "timestamp": _utcnow(),
                }
            )

            # User tool_result line
            self._append(
                {
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": tool_id,
                                "content": [{"type": "text", "text": tc.output}],
                            }
                        ],
                    },
                    "uuid": str(uuid.uuid4()),
                    "timestamp": _utcnow(),
                }
            )

        # 3. Final assistant text line — signals turn boundary
        self._append(
            {
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": turn.assistant}],
                },
                "uuid": str(uuid.uuid4()),
                "timestamp": _utcnow(),
            }
        )


class CodexSessionWriter:
    """Writes synthetic Codex-format JSONL transcript files."""

    def __init__(
        self,
        session_dir: Path,
        project_cwd: str,
        git_branch: str = "main",
    ) -> None:
        self._session_dir = session_dir
        self._project_cwd = project_cwd
        self._git_branch = git_branch
        self._session_uuid = str(uuid.uuid4())
        ts = datetime.now(UTC).strftime("%Y%m%d%H%M%S")
        self._file_path = session_dir / f"rollout-{ts}-{self._session_uuid}.jsonl"
        session_dir.mkdir(parents=True, exist_ok=True)

        # Write the current Codex envelope shape.
        self._append(
            {
                "timestamp": _utcnow(),
                "type": "session_meta",
                "payload": {
                    "id": self._session_uuid,
                    "timestamp": _utcnow(),
                    "cwd": project_cwd,
                    "git": {"branch": git_branch},
                },
            }
        )

    @property
    def file_path(self) -> Path:
        """Path to the JSONL file."""
        return self._file_path

    def _append(self, obj: dict[str, object]) -> None:
        with open(self._file_path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(obj) + "\n")

    def write_turn(self, turn: FakeTurn) -> None:
        """Append all JSONL lines for a complete Codex-format turn."""
        # User message
        self._append(
            {
                "timestamp": _utcnow(),
                "type": "event_msg",
                "payload": {"type": "user_message", "message": turn.user},
            }
        )

        # Tool call / result pairs
        for tc in turn.tool_calls:
            call_id = str(uuid.uuid4())
            self._append(
                {
                    "timestamp": _utcnow(),
                    "type": "response_item",
                    "payload": {
                        "type": "function_call",
                        "name": tc.name,
                        "arguments": json.dumps(tc.input),
                        "call_id": call_id,
                    },
                }
            )
            self._append(
                {
                    "timestamp": _utcnow(),
                    "type": "response_item",
                    "payload": {
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": tc.output,
                    },
                }
            )

        # Assistant message
        self._append(
            {
                "timestamp": _utcnow(),
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": turn.assistant}],
                    "phase": "final_answer",
                },
            }
        )

        # Turn complete
        self._append(
            {
                "timestamp": _utcnow(),
                "type": "event_msg",
                "payload": {
                    "type": "task_complete",
                    "last_agent_message": turn.assistant,
                },
            }
        )


_TOOL_NAMES = ["Read", "Write", "Bash", "Grep"]


def generate_session(
    writer: ClaudeCodeSessionWriter | CodexSessionWriter,
    n_turns: int,
    lorem: LoremGenerator,
) -> None:
    """Write ``n_turns`` synthetic turns to the given writer.

    Each turn has 0–2 fake tool calls chosen from Read, Write, Bash, Grep.

    Args:
        writer: Session writer (ClaudeCode or Codex format).
        n_turns: Number of turns to generate.
        lorem: LoremGenerator instance to source text from.
    """
    rng = lorem._rng
    for _ in range(n_turns):
        n_tools = rng.randint(0, 2)
        tool_calls = [
            FakeToolCall(
                name=rng.choice(_TOOL_NAMES),
                input={"path": f"/tmp/file_{rng.randint(1, 100)}.txt"},
                output=lorem.sentence(),
            )
            for _ in range(n_tools)
        ]
        turn = FakeTurn(
            user=lorem.paragraph(sentences=2),
            assistant=lorem.paragraph(sentences=3),
            tool_calls=tool_calls,
        )
        writer.write_turn(turn)
