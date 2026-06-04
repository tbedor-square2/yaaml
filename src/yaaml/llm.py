"""LLM wrappers for YAAML — memory formulation and consolidation.

Provider is inferred from the model name:
  - "claude-*"            → Anthropic (requires ANTHROPIC_API_KEY)
  - "gpt-*" / "o1-*" / "o3-*" / "o4-*"  → OpenAI (requires OPENAI_API_KEY)

If the model name is ambiguous, ANTHROPIC_API_KEY is tried first.
"""

import json
import logging
import os
from typing import Any

import anthropic
import openai

from .config import Config
from .parsers import ParsedTurn, truncate_tool_content

logger = logging.getLogger(__name__)

_anthropic_client: anthropic.AsyncAnthropic | None = None
_openai_client: openai.AsyncOpenAI | None = None


def _get_anthropic_client() -> anthropic.AsyncAnthropic:
    global _anthropic_client
    if _anthropic_client is None:
        _anthropic_client = anthropic.AsyncAnthropic(api_key=os.environ.get("ANTHROPIC_API_KEY"))
    return _anthropic_client


def _get_openai_client() -> openai.AsyncOpenAI:
    global _openai_client
    if _openai_client is None:
        _openai_client = openai.AsyncOpenAI(api_key=os.environ.get("OPENAI_API_KEY"))
    return _openai_client


def _detect_provider(model: str) -> str:
    """Infer provider from model name; fall back to whichever API key is present."""
    if model.startswith("claude-"):
        return "anthropic"
    if model.startswith(("gpt-", "o1-", "o3-", "o4-")):
        return "openai"
    # Ambiguous name: prefer Anthropic if its key is available
    if os.environ.get("ANTHROPIC_API_KEY"):
        return "anthropic"
    return "openai"


def _turn_to_text(turn: ParsedTurn, truncation_limit: int) -> str:
    """Render a ParsedTurn as readable text for the LLM prompt."""
    parts = []
    if turn.user_content:
        parts.append(f"USER:\n{turn.user_content}")
    for tc in turn.tool_calls:
        name = tc.get("name", "unknown")
        inp = truncate_tool_content(tc.get("input_summary", ""), truncation_limit)
        out = truncate_tool_content(tc.get("output_summary", ""), truncation_limit)
        parts.append(f"TOOL CALL [{name}]:\n  input: {inp}\n  output: {out}")
    if turn.assistant_content:
        parts.append(f"ASSISTANT:\n{turn.assistant_content}")
    return "\n\n".join(parts)


def _build_turns_text(turns: list[ParsedTurn], config: Config) -> str:
    return "\n\n---\n\n".join(_turn_to_text(t, config.tool_call_truncation_chars) for t in turns)


async def _call_llm(prompt: str, model: str) -> str:
    """Single LLM call routed to Anthropic or OpenAI; raises on failure."""
    provider = _detect_provider(model)

    if provider == "openai":
        client = _get_openai_client()
        response = await client.chat.completions.create(
            model=model,
            max_tokens=4096,
            messages=[{"role": "user", "content": prompt}],
        )
        text = response.choices[0].message.content
        if text is None:
            raise ValueError("OpenAI returned empty content")
        return text

    # Anthropic
    ac = _get_anthropic_client()
    a_response = await ac.messages.create(
        model=model,
        max_tokens=4096,
        messages=[{"role": "user", "content": prompt}],
    )
    from anthropic.types import TextBlock

    if not a_response.content:
        raise ValueError("Anthropic returned an empty response")
    block = a_response.content[0]
    if not isinstance(block, TextBlock):
        raise ValueError(f"Unexpected Anthropic content block type: {type(block)}")
    return block.text


def _parse_title_body(text: str, max_length: int) -> tuple[str, str]:
    """Extract title and body from a JSON response block."""
    start = text.find("{")
    end = text.rfind("}") + 1
    if start != -1 and end > start:
        raw = text[start:end]
        try:
            data = json.loads(raw)
            title = str(data.get("title", "Untitled Memory"))
            body = str(data.get("body", text))
            return title, body[:max_length]
        except json.JSONDecodeError:
            pass
    # Fallback: first line as title, rest as body
    lines = text.strip().splitlines()
    title = lines[0][:200] if lines else "Memory"
    body = "\n".join(lines[1:]).strip()[:max_length] if len(lines) > 1 else text[:max_length]
    return title, body


async def summarize_turns(
    turns: list[ParsedTurn],
    prior_summary: str | None,
    config: Config,
) -> tuple[str, str]:
    """Summarize a list of turns into a (title, body) memory.

    If the combined text exceeds max_formulation_tokens, splits into chunks and
    processes recursively, passing each result as prior_summary to the next chunk.

    Args:
        turns: Completed conversation turns to summarize.
        prior_summary: Optional context from a previous chunk pass.
        config: YAAML config.

    Returns:
        (title, body) tuple.
    """
    if not turns:
        return "Empty session", ""

    # Rough token estimate: 4 chars/token
    max_chars = config.max_formulation_tokens * 4
    turns_text = _build_turns_text(turns, config)

    # If too large, chunk and recurse
    if len(turns_text) > max_chars:
        chunk_size = max(1, len(turns) // 2)
        first_half = turns[:chunk_size]
        second_half = turns[chunk_size:]
        try:
            first_title, first_body = await summarize_turns(first_half, prior_summary, config)
            chunk_summary = f"{first_title}\n\n{first_body}"
            return await summarize_turns(second_half, chunk_summary, config)
        except Exception as exc:
            logger.error("Error in chunked summarize_turns: %s", exc)
            raise

    prior_block = ""
    if prior_summary:
        prior_block = f"Prior context summary:\n{prior_summary}\n\n"

    prompt = (
        f"{prior_block}"
        "You are a memory assistant. Summarize the following conversation turns into a "
        "concise, titled memory. Capture facts, decisions, user preferences, and project "
        "state — not raw transcript.\n\n"
        "Respond ONLY with a JSON object in this exact format:\n"
        '{"title": "concise title here", "body": "detailed prose summary here"}\n\n'
        f"Conversation turns:\n\n{turns_text}"
    )

    try:
        raw = await _call_llm(prompt, config.summary_model)
        title, body = _parse_title_body(raw, config.max_memory_length)
        return title, body
    except Exception as exc:
        logger.error("summarize_turns LLM call failed: %s", exc)
        raise


async def merge_memories(
    memories: list[dict[str, Any]],
    config: Config,
) -> tuple[str, str]:
    """Merge a cluster of memories into a single consolidated memory.

    Args:
        memories: List of memory dicts with 'title' and 'body' keys.
        config: YAAML config.

    Returns:
        (title, body) tuple for the merged memory.
    """
    if not memories:
        return "Consolidated Memory", ""

    memories_text = "\n\n---\n\n".join(
        f"## {m.get('title', 'Untitled')}\n\n{m.get('body', '')}" for m in memories
    )

    prompt = (
        "You are a memory consolidation assistant. Merge the following related memories "
        "into a single coherent memory. Preserve all important facts, decisions, and "
        "context. Eliminate redundancy.\n\n"
        "Respond ONLY with a JSON object in this exact format:\n"
        '{"title": "concise title here", "body": "merged prose summary here"}\n\n'
        f"Memories to merge:\n\n{memories_text}"
    )

    try:
        raw = await _call_llm(prompt, config.consolidation_model)
        title, body = _parse_title_body(raw, config.max_memory_length)
        return title, body
    except Exception as exc:
        logger.error("merge_memories LLM call failed: %s", exc)
        raise
