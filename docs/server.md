# krabs-server specs

> Canonical plan lives at `.claude/plans/lazy-tickling-tower.md`. This is a snapshot saved per CLAUDE.md planning loop rules.

## Context

krabs-server is the HTTP API layer for the Krabs agentic framework. It enables any client (custom TUI, web frontend, mobile app, Telegram bot) to create agents, send messages, and stream responses via a documented OpenAPI REST API.

## Key Design Decisions

1. **Axum + utoipa** for HTTP framework and OpenAPI documentation
2. **Core pool + context** — `AgentPool<M>`, `AgentHandle<M>`, `ConversationContext`, `AgentFactory` all live in `krabs-core`, shared by CLI and server
3. **Per-chat agent rebuild** via `AgentFactory` — cheap rebuild, cached provider
4. **SSE streaming** with `SessionEventBus` (broadcast + 512-event circular replay buffer)
5. **ServerHook** bridges agent lifecycle events to SSE (auto-approve tools in server mode)
6. **All config via env vars** — `KRABS_SERVER_BIND`, `KRABS_SERVER_MAX_AGENTS`, etc.
7. **No unwrap()** — all errors as `Result`, mapped to HTTP status codes

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/agents` | Create agent |
| `GET` | `/api/v1/agents` | List agents |
| `GET` | `/api/v1/agents/{id}` | Get agent details |
| `DELETE` | `/api/v1/agents/{id}` | Stop agent |
| `POST` | `/api/v1/agents/{id}/resume` | Resume from session |
| `POST` | `/api/v1/agents/{id}/chat` | Send message (SSE stream) |
| `DELETE` | `/api/v1/agents/{id}/chat` | Cancel in-flight chat |
| `GET` | `/api/v1/agents/{id}/chat/events` | Reconnect SSE |
| `GET` | `/api/v1/agents/{id}/history` | Conversation history |
| `GET` | `/api/v1/sessions` | List sessions |
| `GET` | `/api/v1/sessions/{id}` | Session details |
| `DELETE` | `/api/v1/sessions/{id}` | Delete session |
| `GET` | `/api/v1/tools` | List tools |
| `GET` | `/api/v1/config` | Server config |
| `PATCH` | `/api/v1/config` | Update config |
| `GET` | `/api/v1/health` | Health check |
| `GET` | `/openapi.json` | OpenAPI spec |

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `KRABS_SERVER_BIND` | `127.0.0.1:3001` | Bind address |
| `KRABS_SERVER_SECRET_KEY` | None | Auth secret (None = no auth) |
| `KRABS_SERVER_MAX_AGENTS` | `16` | Max concurrent agents |
| `KRABS_SERVER_CORS_ORIGINS` | permissive | Comma-separated origins |
| `KRABS_SERVER_HEARTBEAT_MS` | `500` | SSE heartbeat interval |
| `KRABS_SERVER_REPLAY_CAPACITY` | `512` | Event replay buffer size |
