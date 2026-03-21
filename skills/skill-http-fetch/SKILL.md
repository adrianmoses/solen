---
name: skill-http-fetch
description: Fetch a URL and return its text content with HTML stripped and truncated to 100KB. Use when the user asks to read a web page, fetch a URL, or extract content from a site.
metadata:
  transport: mcp
  credential_type: none
---

## Tools

### http_fetch

Fetch a URL and return its text content. HTML is stripped automatically. Response truncated at 100KB.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `url` | string | yes | URL to fetch |
| `allowed_domains` | array of strings | no | Domain allowlist. If empty, all domains allowed. |
