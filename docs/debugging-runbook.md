# Debugging Runbook

Quick reference for diagnosing issues in a running or crashed Krabs session.
All queries target `~/.krabs/krabs.db` (SQLite, timestamps in Unix seconds).

---

## 1. Find the session you're looking for

```sql
-- Most recent sessions
SELECT id, model, provider, datetime(created_at, 'unixepoch') AS started
FROM sessions
ORDER BY created_at DESC
LIMIT 20;
```

You only need the first 8 chars (matches what the TUI shows in the info bar):

```sql
SELECT id, model, provider, datetime(created_at, 'unixepoch') AS started
FROM sessions
WHERE id LIKE 'ebf6e551%';
```

---

## 2. Full event timeline for a session

The single most useful query. Gives a chronological stream of everything
that happened: messages sent, tokens used, errors hit, checkpoints written.

```sql
SELECT
    datetime(created_at, 'unixepoch') AS time,
    event_type,
    turn,
    detail
FROM (

    SELECT created_at, 'message_' || role AS event_type, turn,
           COALESCE(tool_name, SUBSTR(content, 1, 120)) AS detail
    FROM messages WHERE session_id LIKE '$SESSION%'

    UNION ALL

    SELECT created_at, 'token_usage', turn,
           'in=' || input_tokens || '  out=' || output_tokens AS detail
    FROM token_usage WHERE session_id LIKE '$SESSION%'

    UNION ALL

    SELECT created_at, 'error_' || context, turn,
           'attempt=' || attempt || ' — ' || SUBSTR(message, 1, 120) AS detail
    FROM errors WHERE session_id LIKE '$SESSION%'

    UNION ALL

    SELECT created_at, 'checkpoint', turn,
           'last_msg_id=' || last_msg_id AS detail
    FROM checkpoints WHERE session_id LIKE '$SESSION%'

) ORDER BY created_at ASC, event_type ASC;
```

Replace `$SESSION` with the 8-char prefix from the TUI (e.g. `ebf6e551`).

### What a healthy turn looks like

```
message_user  → message_assistant → token_usage → checkpoint
```

### What a tool-call turn looks like

```
message_user  → message_assistant → message_tool → ... → message_assistant → token_usage → checkpoint
```

### What a failed turn looks like

```
message_user  → error_llm_stream (attempt=0) → error_llm_stream (attempt=1) → ...
```

---

## 3. Diagnose: "I sent a message but got no response"

Run this to see exactly what landed in the DB:

```sql
-- Step 1: confirm the session exists and has a user message
SELECT id, datetime(created_at,'unixepoch') AS started
FROM sessions WHERE id LIKE '$SESSION%';

SELECT role, turn, SUBSTR(content,1,120) AS content,
       datetime(created_at,'unixepoch') AS time
FROM messages WHERE session_id LIKE '$SESSION%'
ORDER BY created_at;

-- Step 2: check for errors
SELECT context, message, attempt, datetime(created_at,'unixepoch') AS time
FROM errors WHERE session_id LIKE '$SESSION%'
ORDER BY created_at;

-- Step 3: check if tokens were counted (means LLM call completed)
SELECT turn, input_tokens, output_tokens
FROM token_usage WHERE session_id LIKE '$SESSION%';
```

**Interpretation:**

| What you see | Likely cause |
|---|---|
| Only `message_user`, no `message_assistant`, no errors | Streaming deadlock — LLM produced 64+ chunks and the channel filled (fixed in current build) |
| `message_user` + `error_llm_stream attempt=0..N` | LLM call failed repeatedly — check `message` column for HTTP status or timeout text |
| `message_user` + `message_assistant` (empty content) + `token_usage` | LLM responded but with empty `content` — check if model uses `reasoning_content` field |
| `message_user` + tool messages but no final `message_assistant` | Tool loop completed but synthesis turn failed or TUI channel was closed (Ctrl+C) |
| Nothing at all | Session was never created — config/DB path issue |

---

## 4. Diagnose: tool call issues

```sql
-- All tool calls and their results for a session
SELECT
    m.turn,
    m.role,
    m.tool_name,
    SUBSTR(m.content, 1, 200)  AS content,
    datetime(m.created_at, 'unixepoch') AS time
FROM messages m
WHERE m.session_id LIKE '$SESSION%'
  AND m.role IN ('assistant', 'tool')
ORDER BY m.created_at;

-- Tool errors specifically
SELECT context, message, attempt, datetime(created_at,'unixepoch') AS time
FROM errors
WHERE session_id LIKE '$SESSION%'
  AND context NOT IN ('llm_stream', 'llm_complete')
ORDER BY created_at;
```

---

## 5. Diagnose: context overflow / truncation

```sql
-- Find sessions where token usage spiked
SELECT
    s.id,
    s.model,
    SUM(t.input_tokens)  AS total_input,
    SUM(t.output_tokens) AS total_output,
    datetime(s.created_at,'unixepoch') AS started
FROM sessions s
JOIN token_usage t ON t.session_id = s.id
GROUP BY s.id
ORDER BY total_input DESC
LIMIT 10;

-- Check if any tool result was truncated in a session
SELECT turn, tool_name, LENGTH(content) AS chars, SUBSTR(content, -60) AS tail
FROM messages
WHERE session_id LIKE '$SESSION%'
  AND role = 'tool'
ORDER BY chars DESC;
```

`tail` ending with `[…output truncated to fit context window…]` confirms truncation fired
(`max_tool_result_chars`, default 8000 chars).

---

## 6. Diagnose: retry storms

```sql
-- Sessions with the most retries
SELECT
    session_id,
    context,
    COUNT(*) AS total_errors,
    MAX(attempt) AS max_attempt,
    MIN(datetime(created_at,'unixepoch')) AS first,
    MAX(datetime(created_at,'unixepoch')) AS last
FROM errors
GROUP BY session_id, context
ORDER BY total_errors DESC
LIMIT 20;

-- Errors in the last hour
SELECT
    datetime(created_at,'unixepoch') AS time,
    session_id,
    context,
    attempt,
    SUBSTR(message,1,120) AS message
FROM errors
WHERE created_at > strftime('%s','now') - 3600
ORDER BY created_at DESC;
```

---

## 7. Diagnose: missing checkpoint (crash mid-turn)

If the agent crashed mid-turn, there will be messages after the last checkpoint's
`last_msg_id`. On next resume, those are rolled back automatically. You can check:

```sql
SELECT
    cp.turn,
    cp.last_msg_id,
    (SELECT COUNT(*) FROM messages m
     WHERE m.session_id = cp.session_id AND m.id > cp.last_msg_id) AS orphaned_messages
FROM checkpoints cp
WHERE cp.session_id LIKE '$SESSION%'
ORDER BY cp.turn DESC
LIMIT 5;
```

`orphaned_messages > 0` means a crash happened mid-turn and resume will roll back those rows.

---

## 8. Grafana metrics queries

### Token usage over time (time-series)

```sql
SELECT
    (created_at / 300) * 300 * 1000  AS time,
    SUM(input_tokens)                 AS input_tokens,
    SUM(output_tokens)                AS output_tokens,
    SUM(input_tokens + output_tokens) AS total_tokens
FROM token_usage
WHERE created_at >= $__unixEpochFrom() AND created_at <= $__unixEpochTo()
GROUP BY (created_at / 300)
ORDER BY time ASC;
```

### Error rate over time (time-series)

```sql
SELECT
    (created_at / 300) * 300 * 1000 AS time,
    COUNT(*)                         AS errors,
    context
FROM errors
WHERE created_at >= $__unixEpochFrom() AND created_at <= $__unixEpochTo()
GROUP BY (created_at / 300), context
ORDER BY time ASC;
```

### Tool call frequency (bar chart)

```sql
SELECT tool_name, COUNT(*) AS calls
FROM messages
WHERE role = 'tool'
  AND created_at >= $__unixEpochFrom()
  AND created_at <= $__unixEpochTo()
GROUP BY tool_name
ORDER BY calls DESC;
```

### Cost by model (stat / table)

```sql
SELECT
    s.model,
    s.provider,
    SUM(t.input_tokens)                   AS total_input,
    SUM(t.output_tokens)                  AS total_output,
    SUM(t.input_tokens + t.output_tokens) AS total_tokens
FROM token_usage t
JOIN sessions s ON s.id = t.session_id
GROUP BY s.model, s.provider
ORDER BY total_tokens DESC;
```

### Recent sessions (table panel)

```sql
SELECT
    s.id                                    AS session_id,
    s.model,
    datetime(s.created_at, 'unixepoch')    AS started,
    COUNT(DISTINCT m.turn)                  AS turns,
    SUM(t.input_tokens + t.output_tokens)   AS total_tokens
FROM sessions s
LEFT JOIN messages   m ON m.session_id = s.id
LEFT JOIN token_usage t ON t.session_id = s.id
GROUP BY s.id
ORDER BY s.created_at DESC
LIMIT 50;
```

---

## 9. Event type reference

| `event_type` | Source | What it means |
|---|---|---|
| `message_system` | `messages` | System prompt injected at turn start |
| `message_user` | `messages` | User submitted input |
| `message_assistant` | `messages` | LLM produced a response (may include tool calls in `tool_args`) |
| `message_tool` | `messages` | Tool result returned to LLM |
| `token_usage` | `token_usage` | LLM call completed; tokens counted for the turn |
| `error_llm_stream` | `errors` | Streaming call failed; `attempt` = retry index |
| `error_llm_complete` | `errors` | Non-streaming call failed |
| `error_bash` | `errors` | `bash` tool hard-error after retries |
| `error_<tool_name>` | `errors` | Any other tool hard-error after retries |
| `error_max_turns` | `errors` | Agent hit `config.max_turns` limit |
| `checkpoint` | `checkpoints` | Turn succeeded; `last_msg_id` is the safe resume boundary |

---

## 10. Quick one-liners

```bash
# Open the DB in sqlite3 CLI
sqlite3 ~/.krabs/krabs.db

# Count messages per role across all sessions
sqlite3 ~/.krabs/krabs.db \
  "SELECT role, COUNT(*) FROM messages GROUP BY role;"

# Last 10 errors
sqlite3 ~/.krabs/krabs.db \
  "SELECT datetime(created_at,'unixepoch'), context, attempt, SUBSTR(message,1,80)
   FROM errors ORDER BY created_at DESC LIMIT 10;"

# Token spend today
sqlite3 ~/.krabs/krabs.db \
  "SELECT SUM(input_tokens), SUM(output_tokens)
   FROM token_usage
   WHERE created_at > strftime('%s','now','start of day');"
```
