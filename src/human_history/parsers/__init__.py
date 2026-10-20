"""Agent log parsers. Add a new agent by implementing ``base.Parser`` and registering it here."""

from human_history.models import Agent
from human_history.parsers.base import Parser
from human_history.parsers.claude import ClaudeParser
from human_history.parsers.codex import CodexParser

PARSERS: dict[Agent, Parser] = {
    Agent.CLAUDE: ClaudeParser(),
    Agent.CODEX: CodexParser(),
}
