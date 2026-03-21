---
name: skill-google-calendar
description: Manage Google Calendar events — list, create, update, delete events, and find free time slots. Use when the user asks about their calendar, scheduling meetings, checking availability, or managing events.
metadata:
  transport: api
  provider: google
  credential_type: service_account
  scopes: >-
    https://www.googleapis.com/auth/calendar.readonly
    https://www.googleapis.com/auth/calendar.events
  api_base: https://www.googleapis.com/calendar/v3
---

## Authentication

Requires a Google service account with domain-wide delegation, or a user OAuth token.
The access token is passed as `Authorization: Bearer {token}` on every API call.

Token lifetime is 1 hour. The system refreshes automatically when within 60 seconds of expiry.
If refresh fails with `invalid_grant`, the user must re-authorise.

## Tools

### calendar_list_events

List upcoming events from a calendar.

- **Endpoint:** `GET /calendars/{calendarId}/events`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `calendar_id` | string | no | Calendar ID (default `primary`) |
| `max_results` | integer | no | Maximum events to return (default 10, max 250) |
| `time_min` | string | no | Lower bound (RFC 3339, default now) |
| `time_max` | string | no | Upper bound (RFC 3339) |
| `query` | string | no | Free-text search across event fields |

### calendar_get_event

Get a single event by ID.

- **Endpoint:** `GET /calendars/{calendarId}/events/{eventId}`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `calendar_id` | string | no | Calendar ID (default `primary`) |
| `event_id` | string | yes | Event ID |

### calendar_create_event

Create a new calendar event. **Destructive — requires approval.**

- **Endpoint:** `POST /calendars/{calendarId}/events`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `calendar_id` | string | no | Calendar ID (default `primary`) |
| `summary` | string | yes | Event title |
| `start` | string | yes | Start time (RFC 3339) |
| `end` | string | yes | End time (RFC 3339) |
| `description` | string | no | Event description |
| `location` | string | no | Event location |
| `attendees` | array of strings | no | Email addresses to invite |

### calendar_update_event

Update an existing event. **Destructive — requires approval.**

- **Endpoint:** `PATCH /calendars/{calendarId}/events/{eventId}`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `calendar_id` | string | no | Calendar ID (default `primary`) |
| `event_id` | string | yes | Event ID |
| `summary` | string | no | New title |
| `start` | string | no | New start time |
| `end` | string | no | New end time |
| `description` | string | no | New description |
| `location` | string | no | New location |

### calendar_delete_event

Delete a calendar event. **Destructive — requires approval.**

- **Endpoint:** `DELETE /calendars/{calendarId}/events/{eventId}`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `calendar_id` | string | no | Calendar ID (default `primary`) |
| `event_id` | string | yes | Event ID |

### calendar_find_free_slots

Find available time slots across calendars. This is a computed tool — it calls the Freebusy API and inverts the busy blocks to produce free slots.

- **Endpoint:** `POST /freeBusy` (internally)
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `duration_minutes` | integer | yes | Desired meeting duration in minutes |
| `time_min` | string | yes | Search window start (RFC 3339) |
| `time_max` | string | yes | Search window end (RFC 3339) |
| `calendar_ids` | array of strings | no | Calendars to check (default `["primary"]`) |
| `max_slots` | integer | no | Maximum slots to return (default 5) |

Returns a list of `{ start, end }` time slots where all specified calendars are free.

## Error Handling

- **401 Unauthorized:** Token expired or revoked. System will attempt refresh.
- **403 Forbidden:** Insufficient scopes or domain delegation not configured.
- **404 Not Found:** Calendar or event ID does not exist.
- **409 Conflict:** Event was modified concurrently. Retry with updated event.
