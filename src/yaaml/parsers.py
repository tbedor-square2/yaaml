"""Parsers for Claude Code and Codex JSONL transcript files."""

import json
from dataclasses import dataclass, field
from pathlib import Path


def normalize_project_id(cwd: str) -> str:
    """Normalize a working directory path to a canonical project ID.

    Args:
        cwd: Working directory string.

    Returns:
        Resolved absolute path string without trailing slash.
    """
    return str(Path(cwd).resolve()).rstrip("/")


def truncate_tool_content(text: str, limit: int) -> str:
    """Truncate tool input/output to a character limit.

    Args:
        text: Content to truncate.
        limit: Maximum character count.

    Returns:
        Truncated string with ellipsis appended if truncation occurred.
    """
    if len(text) <= limit:
        return text
    return text[:limit] + "...[truncated]"


@dataclass
class ParsedTurn:
    session_id: str
    agent_type: str  # 'claude-code' | 'codex'
    project_id: str  # cwd normalized
    git_branch: str | None
    user_content: str
    assistant_content: str
    tool_calls: list[dict]  # [{name, input_summary, output_summary}]
    started_at: str  # ISO
    completed_at: str  # ISO
    is_aborted: bool = False


@dataclass
class SessionMeta:
    session_id: str
    agent_type: str
    project_id: str
    git_branch: str | None
    transcript_file_path: str
    started_at: str


class ClaudeCodeParser:
    """Stateful parser for Claude Code JSONL transcript files.

    A turn is complete when an assistant message arrives with no tool_use blocks
    in its content array.
    """

    def __init__(self, file_path: str) -> None:
        self.file_path = file_path
        self.session_id = Path(file_path).stem
        self.session_meta: SessionMeta | None = None

        # Accumulation state
        self._project_id: str | None = None
        self._git_branch: str | None = None
        self._current_user_content: str = ""
        self._current_tool_calls: list[dict] = []
        self._pending_tool_inputs: dict[str, dict] = {}  # id -> {name, input}
        self._turn_started_at: str | None = None
        self._in_turn: bool = False
        self._first_line_seen: bool = False

    def feed(self, line: str) -> ParsedTurn | None:
        """Parse a single line. Returns a ParsedTurn if a turn is complete.

        Args:
            line: A single JSON line from a Claude Code JSONL file.

        Returns:
            Completed ParsedTurn or None.
        """
        line = line.strip()
        if not line:
            return None

        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            return None

        # Extract cwd from any line that has it
        if self._project_id is None:
            cwd = obj.get("cwd") or obj.get("workingDirectory")
            if cwd:
                self._project_id = normalize_project_id(cwd)

        # Try to extract git branch
        if self._git_branch is None:
            git_info = obj.get("gitBranch") or obj.get("git", {}).get("branch")
            if git_info:
                self._git_branch = git_info

        msg_type = obj.get("type", "")

        # Handle system init line (first line typically)
        if not self._first_line_seen:
            self._first_line_seen = True
            # Extract session info if available
            if self._project_id is None and obj.get("cwd"):
                self._project_id = normalize_project_id(obj["cwd"])
            # Build session meta once we have a project_id
            if self._project_id and self.session_meta is None:
                started_at = obj.get("timestamp", "")
                self.session_meta = SessionMeta(
                    session_id=self.session_id,
                    agent_type="claude-code",
                    project_id=self._project_id,
                    git_branch=self._git_branch,
                    transcript_file_path=self.file_path,
                    started_at=started_at,
                )

        # Handle user message
        if msg_type == "user":
            content = obj.get("message", {})
            if isinstance(content, dict):
                role = content.get("role", "")
                if role == "user":
                    # Extract text from content array or direct string
                    raw_content = content.get("content", "")
                    user_text = self._extract_text(raw_content)
                    self._current_user_content = user_text
                    self._turn_started_at = obj.get("timestamp", "")
                    self._in_turn = True
                    self._current_tool_calls = []
                    self._pending_tool_inputs = {}

                    # Build session meta if not done yet
                    if self._project_id and self.session_meta is None:
                        self.session_meta = SessionMeta(
                            session_id=self.session_id,
                            agent_type="claude-code",
                            project_id=self._project_id,
                            git_branch=self._git_branch,
                            transcript_file_path=self.file_path,
                            started_at=self._turn_started_at or "",
                        )

        # Handle assistant message
        elif msg_type == "assistant":
            if not self._in_turn:
                return None
            message = obj.get("message", {})
            if not isinstance(message, dict):
                return None

            content_array = message.get("content", [])
            if not isinstance(content_array, list):
                content_array = []

            # Collect text and tool_use blocks
            assistant_text_parts = []
            tool_use_blocks = []
            tool_result_blocks = []

            for block in content_array:
                if not isinstance(block, dict):
                    continue
                btype = block.get("type", "")
                if btype == "text":
                    assistant_text_parts.append(block.get("text", ""))
                elif btype == "tool_use":
                    tool_use_blocks.append(block)
                    # Record pending tool call
                    tool_id = block.get("id", "")
                    self._pending_tool_inputs[tool_id] = {
                        "name": block.get("name", ""),
                        "input": json.dumps(block.get("input", {})),
                    }
                elif btype == "tool_result":
                    tool_result_blocks.append(block)

            assistant_text = "\n".join(assistant_text_parts)

            # If no tool_use blocks: this is a completed turn
            if not tool_use_blocks:
                completed_at = obj.get("timestamp", "")
                project_id = self._project_id or ""

                turn = ParsedTurn(
                    session_id=self.session_id,
                    agent_type="claude-code",
                    project_id=project_id,
                    git_branch=self._git_branch,
                    user_content=self._current_user_content,
                    assistant_content=assistant_text,
                    tool_calls=list(self._current_tool_calls),
                    started_at=self._turn_started_at or "",
                    completed_at=completed_at,
                    is_aborted=False,
                )
                # Reset state
                self._current_user_content = ""
                self._current_tool_calls = []
                self._pending_tool_inputs = {}
                self._turn_started_at = None
                self._in_turn = False
                return turn

        return None

    def _extract_text(self, content) -> str:
        """Extract text from various content formats."""
        if isinstance(content, str):
            return content
        if isinstance(content, list):
            parts = []
            for item in content:
                if isinstance(item, dict):
                    if item.get("type") == "text":
                        parts.append(item.get("text", ""))
                    elif item.get("type") == "tool_result":
                        # Capture tool result to associate with pending tool input
                        tool_use_id = item.get("tool_use_id", "")
                        result_content = item.get("content", "")
                        if isinstance(result_content, list):
                            result_text = " ".join(
                                r.get("text", "") for r in result_content
                                if isinstance(r, dict) and r.get("type") == "text"
                            )
                        else:
                            result_text = str(result_content)

                        if tool_use_id and tool_use_id in self._pending_tool_inputs:
                            pending = self._pending_tool_inputs.pop(tool_use_id)
                            self._current_tool_calls.append({
                                "name": pending["name"],
                                "input_summary": truncate_tool_content(pending["input"], 500),
                                "output_summary": truncate_tool_content(result_text, 500),
                            })
                elif isinstance(item, str):
                    parts.append(item)
            return "\n".join(p for p in parts if p)
        return ""


class CodexParser:
    """Stateful parser for Codex JSONL session files.

    First line is a SessionMeta with id, cwd, git.branch.
    Accumulates lines between EventMsg/UserMessage and EventMsg/TurnComplete (or TurnAborted).
    """

    def __init__(self, file_path: str) -> None:
        self.file_path = file_path
        self.session_meta: SessionMeta | None = None

        self._session_id: str | None = None
        self._project_id: str | None = None
        self._git_branch: str | None = None
        self._in_turn: bool = False
        self._turn_started_at: str | None = None
        self._user_content: str = ""
        self._assistant_parts: list[str] = []
        self._tool_calls: list[dict] = []
        self._pending_tool: dict | None = None
        self._first_line_seen: bool = False

    def feed(self, line: str) -> ParsedTurn | None:
        """Parse a single line. Returns a ParsedTurn if a turn is complete.

        Args:
            line: A single JSON line from a Codex JSONL file.

        Returns:
            Completed ParsedTurn or None.
        """
        line = line.strip()
        if not line:
            return None

        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            return None

        # First line: session metadata
        if not self._first_line_seen:
            self._first_line_seen = True
            self._session_id = obj.get("id", Path(self.file_path).stem)
            cwd = obj.get("cwd", "")
            if cwd:
                self._project_id = normalize_project_id(cwd)
            git_info = obj.get("git", {})
            if isinstance(git_info, dict):
                self._git_branch = git_info.get("branch")
            started_at = obj.get("created_at") or obj.get("startedAt") or ""
            self.session_meta = SessionMeta(
                session_id=self._session_id,
                agent_type="codex",
                project_id=self._project_id or "",
                git_branch=self._git_branch,
                transcript_file_path=self.file_path,
                started_at=started_at,
            )
            return None

        # Subsequent lines are event messages
        msg_type = obj.get("type", "")

        try:
            return self._handle_event(obj, msg_type)
        except Exception:
            # Skip unknown / malformed event types gracefully
            return None

    def _handle_event(self, obj: dict, msg_type: str) -> ParsedTurn | None:
        """Handle a Codex event object."""

        # UserMessage event — starts a new turn
        if msg_type in ("EventMsg/UserMessage", "user"):
            content = obj.get("content", "") or obj.get("message", "")
            if isinstance(content, list):
                content = " ".join(
                    c.get("text", "") for c in content
                    if isinstance(c, dict) and c.get("type") == "text"
                )
            self._user_content = str(content)
            self._turn_started_at = obj.get("timestamp", "")
            self._in_turn = True
            self._assistant_parts = []
            self._tool_calls = []
            self._pending_tool = None
            return None

        if not self._in_turn:
            return None

        # Assistant/model output tokens
        if msg_type in ("EventMsg/AssistantMessage", "assistant", "EventMsg/ModelOutput"):
            content = obj.get("content", "") or obj.get("message", "")
            if isinstance(content, list):
                for item in content:
                    if isinstance(item, dict) and item.get("type") == "text":
                        self._assistant_parts.append(item.get("text", ""))
            elif isinstance(content, str):
                self._assistant_parts.append(content)
            return None

        # Tool call started
        if msg_type in ("EventMsg/ToolCall", "EventMsg/FunctionCall"):
            self._pending_tool = {
                "name": obj.get("name", obj.get("function", "")),
                "input": json.dumps(obj.get("arguments", obj.get("input", {}))),
            }
            return None

        # Tool result
        if msg_type in ("EventMsg/ToolResult", "EventMsg/FunctionResult"):
            result = obj.get("output", obj.get("result", ""))
            if self._pending_tool:
                self._tool_calls.append({
                    "name": self._pending_tool["name"],
                    "input_summary": truncate_tool_content(self._pending_tool["input"], 500),
                    "output_summary": truncate_tool_content(str(result), 500),
                })
                self._pending_tool = None
            return None

        # Turn complete
        if msg_type in ("EventMsg/TurnComplete", "EventMsg/TurnAborted"):
            is_aborted = msg_type == "EventMsg/TurnAborted"
            completed_at = obj.get("timestamp", "")

            turn = ParsedTurn(
                session_id=self._session_id or "",
                agent_type="codex",
                project_id=self._project_id or "",
                git_branch=self._git_branch,
                user_content=self._user_content,
                assistant_content="\n".join(self._assistant_parts),
                tool_calls=list(self._tool_calls),
                started_at=self._turn_started_at or "",
                completed_at=completed_at,
                is_aborted=is_aborted,
            )
            # Reset state
            self._in_turn = False
            self._user_content = ""
            self._assistant_parts = []
            self._tool_calls = []
            self._pending_tool = None
            self._turn_started_at = None
            return turn

        # Unknown types are silently skipped
        return None
