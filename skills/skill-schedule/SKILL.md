---
name: skill-schedule
description: Create, list, update, and delete scheduled tasks. Supports cron expressions for recurring schedules and epoch timestamps for one-shot tasks. Use when the user asks to schedule, remind, or automate something on a timer.
metadata:
  transport: mcp
  credential_type: none
---

## Tools

### schedule_create

Create a scheduled task. Provide either a cron expression (recurring) or run_at timestamp (one-shot).

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message` | string | yes | Prompt message sent to the agent when the schedule fires |
| `cron_expr` | string | no | Crontab expression for recurring schedules (e.g. `0 9 * * 1`) |
| `run_at` | integer | no | Epoch milliseconds for one-shot schedule |
| `tool_params` | object | no | Hint to use a specific tool when the schedule fires (`tool_name`, `tool_input`) |

Exactly one of `cron_expr` or `run_at` must be provided.

### schedule_list

List all schedules.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `enabled_only` | boolean | no | If true, only return enabled schedules |

### schedule_get

Get a schedule by ID.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | integer | yes | Schedule ID |

### schedule_delete

Delete a schedule by ID. **Destructive.**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | integer | yes | Schedule ID |

### schedule_update

Update a schedule. Only provided fields are changed.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | integer | yes | Schedule ID |
| `message` | string | no | New prompt message |
| `cron_expr` | string | no | New cron expression |
| `run_at` | integer | no | New one-shot timestamp |
| `enabled` | boolean | no | Enable or disable |
