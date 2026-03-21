---
name: skill-memory
description: Store, retrieve, list, and delete key-value memories with optional tags. Use when the user asks to remember something, recall a fact, or manage stored notes.
metadata:
  transport: mcp
  credential_type: none
---

## Tools

### memory_store

Store a key-value memory with optional tags.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `key` | string | yes | Unique key for the memory |
| `value` | string | yes | Content to store |
| `tags` | string | no | Comma-separated tags |

### memory_retrieve

Retrieve a memory by key.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `key` | string | yes | Key to look up |

### memory_list

List all memories, optionally filtered by tag.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `tag` | string | no | Filter by tag |

### memory_delete

Delete a memory by key. **Destructive.**

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `key` | string | yes | Key to delete |
