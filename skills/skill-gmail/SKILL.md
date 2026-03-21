---
name: skill-gmail
description: Read, search, and send Gmail messages via the Gmail API. Use when the user asks about email, inbox, unread messages, or sending mail.
metadata:
  transport: api
  provider: google
  credential_type: service_account
  scopes: >-
    https://www.googleapis.com/auth/gmail.readonly
    https://www.googleapis.com/auth/gmail.send
    https://www.googleapis.com/auth/gmail.modify
  api_base: https://gmail.googleapis.com/gmail/v1
---

## Authentication

Requires a Google service account with domain-wide delegation, or a user OAuth token.
The access token is passed as `Authorization: Bearer {token}` on every API call.

Token lifetime is 1 hour. The system refreshes automatically when within 60 seconds of expiry.
If refresh fails with `invalid_grant`, the user must re-authorise.

## Tools

### gmail_list_messages

List recent messages from the inbox.

- **Endpoint:** `GET /users/me/messages`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `max_results` | integer | no | Maximum messages to return (default 10, max 100) |
| `query` | string | no | Gmail search query (same syntax as the Gmail search box) |
| `label` | string | no | Filter by label (e.g. `INBOX`, `UNREAD`, `STARRED`) |

Returns a list of message summaries (id, threadId, snippet).

### gmail_get_message

Get the full content of a message by ID.

- **Endpoint:** `GET /users/me/messages/{id}`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | string | yes | Message ID |
| `format` | string | no | Response format: `full`, `metadata`, or `minimal` (default `full`) |

### gmail_send

Send an email message. **Destructive — requires approval.**

- **Endpoint:** `POST /users/me/messages/send`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `to` | string | yes | Recipient email address |
| `subject` | string | yes | Email subject |
| `body` | string | yes | Email body (plain text) |
| `cc` | string | no | CC recipients (comma-separated) |
| `bcc` | string | no | BCC recipients (comma-separated) |
| `in_reply_to` | string | no | Message ID to reply to |

The skill constructs an RFC 2822 message and base64url-encodes it for the API.

### gmail_reply

Reply to an existing message. **Destructive — requires approval.**

- **Endpoint:** `POST /users/me/messages/send`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `message_id` | string | yes | ID of the message to reply to |
| `body` | string | yes | Reply body (plain text) |

The skill fetches the original message headers (`To`, `Subject`, `Message-ID`) and constructs a proper reply with `In-Reply-To` and `References` headers.

### gmail_archive

Archive messages by removing the INBOX label. **Destructive — requires approval.**

- **Endpoint:** `POST /users/me/messages/{id}/modify`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | string | yes | Message ID to archive |

## Error Handling

- **401 Unauthorized:** Token expired or revoked. System will attempt refresh.
- **403 Forbidden:** Insufficient scopes or domain delegation not configured.
- **429 Too Many Requests:** Rate limited. Retry after the duration in `Retry-After` header.
