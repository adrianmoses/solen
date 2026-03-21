---
name: skill-web-search
description: Search the web using the Brave Search API. Use when the user asks to search for information, look something up, or find recent news.
metadata:
  transport: mcp
  credential_type: none
  env_vars: BRAVE_SEARCH_API_KEY
---

## Tools

### web_search

Search the web using Brave Search API. Returns titles, URLs, and descriptions.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `query` | string | yes | Search query |
| `max_results` | integer | no | Maximum results, 1-10 (default 5) |
