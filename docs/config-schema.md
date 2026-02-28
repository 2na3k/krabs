# Config schema

Krabs resolves configuration from three sources, applied in order (later overrides earlier):

1. `~/.krabs/config.json` — global defaults
2. `.krabs.json` — project-level overrides
3. Environment variables — highest priority

---

## `~/.krabs/config.json` / `.krabs.json`

```json
{
  "model": "claude-sonnet-4-6",
  "base_url": "https://api.anthropic.com",
  "api_key": "",
  "max_turns": 50,
  "db_path": "~/.krabs/krabs.db",
  "max_context_tokens": 128000,
  "skills": {
    "paths": ["skills/"],
    "enabled": []
  },
  "custom_models": [
    {
      "name": "my-local",
      "provider": "openai",
      "base_url": "http://localhost:11434/v1",
      "api_key": "ollama",
      "model": "llama3.2"
    }
  ]
}
```

### Fields

| Field                | Type             | Default                    | Description                                                                 |
|----------------------|------------------|----------------------------|-----------------------------------------------------------------------------|
| `model`              | string           | `"gpt-4o"`                 | Model identifier passed to the provider                                     |
| `base_url`           | string           | `"https://api.openai.com/v1"` | Provider API base URL                                                    |
| `api_key`            | string           | `""`                       | API key (prefer env vars over storing here)                                 |
| `max_turns`          | integer          | `50`                       | Maximum agent loop iterations before stopping                               |
| `db_path`            | path             | `~/.krabs/krabs.db`        | SQLite database for session persistence                                     |
| `max_context_tokens` | integer          | `128000`                   | Context window limit; messages are trimmed when >80% used                   |
| `skills.paths`       | array of paths   | `["skills/"]`              | Directories to scan for skills                                              |
| `skills.enabled`     | array of strings | `[]` (all)                 | Allowlist of skill names; empty means all discovered skills are loaded      |
| `custom_models`      | array            | `[]`                       | Register additional model endpoints (see below)                             |

### `custom_models` entry

| Field      | Type   | Description                                          |
|------------|--------|------------------------------------------------------|
| `name`     | string | Alias used in `/models <name>` CLI command           |
| `provider` | string | One of `"openai"`, `"anthropic"`, `"gemini"`        |
| `base_url` | string | API endpoint                                         |
| `api_key`  | string | API key for this endpoint                            |
| `model`    | string | Model ID passed to the provider                      |

---

## `~/.krabs/credentials.json`

Managed by `krabs setup`. Stores provider credentials.

```json
{
  "provider": "anthropic",
  "api_key": "sk-ant-...",
  "base_url": "https://api.anthropic.com",
  "model": "claude-sonnet-4-6",
  "is_default": true
}
```

| Field        | Type    | Description                                        |
|--------------|---------|----------------------------------------------------|
| `provider`   | string  | One of `"anthropic"`, `"openai"`, `"gemini"`      |
| `api_key`    | string  | API key                                            |
| `base_url`   | string  | Provider API base URL                              |
| `model`      | string  | Default model for this provider                    |
| `is_default` | boolean | Whether this credential set is the active default  |

---

## `~/.krabs/mcp.json`

MCP server registry. Each server is connected at agent startup and its tools are registered into the tool registry as `mcp__{server}__{tool}`.

```json
{
  "servers": [
    {
      "name": "filesystem",
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "url": "",
      "enabled": true
    },
    {
      "name": "remote-tools",
      "transport": "sse",
      "command": "",
      "args": [],
      "url": "http://localhost:8080/sse",
      "enabled": true
    }
  ]
}
```

### Server fields

| Field       | Type            | Description                                                     |
|-------------|-----------------|-----------------------------------------------------------------|
| `name`      | string          | Server name; used as namespace prefix for tools                 |
| `transport` | string          | `"stdio"` (subprocess) or `"sse"` (HTTP Server-Sent Events)    |
| `command`   | string          | Executable to launch (stdio only)                               |
| `args`      | array of string | Arguments for the subprocess (stdio only)                       |
| `url`       | string          | SSE endpoint URL (sse only)                                     |
| `enabled`   | boolean         | Whether this server is connected at startup                     |

---

## Environment variables

| Variable           | Overrides          | Description                      |
|--------------------|--------------------|----------------------------------|
| `KRABS_MODEL`      | `config.model`     | Model identifier                 |
| `KRABS_BASE_URL`   | `config.base_url`  | Provider API base URL            |
| `KRABS_API_KEY`    | `config.api_key`   | API key                          |
| `ANTHROPIC_API_KEY`| `config.api_key`   | Anthropic API key (auto-detected)|
| `OPENAI_API_KEY`   | `config.api_key`   | OpenAI API key (auto-detected)   |
| `GEMINI_API_KEY`   | `config.api_key`   | Gemini API key (auto-detected)   |

---

## Skills directory layout

```
skills/
└── my-skill/
    ├── SKILL.md          # required: frontmatter + instructions
    └── reference.md      # optional: additional resources
```

### `SKILL.md` format

```markdown
---
name: my-skill
description: A short description of what this skill does
---

# My Skill

Quick-start instructions here.

## Reference
See [reference.md](reference.md) for full details.
```

### Frontmatter constraints

| Field         | Constraints                                         |
|---------------|-----------------------------------------------------|
| `name`        | Required. Max 64 chars. Pattern: `[a-z0-9-]+`      |
| `description` | Required. Max 1024 chars. No XML tags.              |

---

## Agent persona format

Personas live in `./krabs/agents/*.md` and are invoked with `@<name>` in the CLI.

```markdown
---
name: reviewer
description: Senior code reviewer focused on correctness
model: claude-sonnet-4-6
---

You are a senior Rust engineer...
```

### Frontmatter fields

| Field         | Type   | Required | Description                               |
|---------------|--------|----------|-------------------------------------------|
| `name`        | string | Yes      | Identifier used with `@<name>`            |
| `description` | string | No       | Shown in `/agents list`                   |
| `model`       | string | No       | Override model for this persona           |
