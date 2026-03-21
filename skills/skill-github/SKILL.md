---
name: skill-github
description: Interact with GitHub repositories, issues, and pull requests via the GitHub API. Use when the user asks about repos, issues, PRs, commits, or code on GitHub.
metadata:
  transport: api
  provider: github
  credential_type: oauth
  scopes: repo user:email read:org
  api_base: https://api.github.com
---

## Authentication

Requires a GitHub OAuth token obtained via the PKCE flow.
The access token is passed as `Authorization: Bearer {token}` on every API call.

GitHub OAuth App tokens do not expire. GitHub App tokens expire after 8 hours.
All requests must include `Accept: application/vnd.github+json` and `X-GitHub-Api-Version: 2022-11-28`.

## Tools

### github_list_repos

List repositories for the authenticated user.

- **Endpoint:** `GET /user/repos`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `sort` | string | no | Sort by: `created`, `updated`, `pushed`, `full_name` (default `updated`) |
| `per_page` | integer | no | Results per page (default 30, max 100) |
| `visibility` | string | no | Filter: `all`, `public`, `private` (default `all`) |

### github_list_issues

List issues for a repository.

- **Endpoint:** `GET /repos/{owner}/{repo}/issues`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `owner` | string | yes | Repository owner |
| `repo` | string | yes | Repository name |
| `state` | string | no | Filter: `open`, `closed`, `all` (default `open`) |
| `labels` | string | no | Comma-separated label names |
| `per_page` | integer | no | Results per page (default 30) |

### github_get_issue

Get a single issue by number.

- **Endpoint:** `GET /repos/{owner}/{repo}/issues/{number}`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `owner` | string | yes | Repository owner |
| `repo` | string | yes | Repository name |
| `number` | integer | yes | Issue number |

### github_create_issue

Create a new issue. **Destructive — requires approval.**

- **Endpoint:** `POST /repos/{owner}/{repo}/issues`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `owner` | string | yes | Repository owner |
| `repo` | string | yes | Repository name |
| `title` | string | yes | Issue title |
| `body` | string | no | Issue body (Markdown) |
| `labels` | array of strings | no | Labels to apply |
| `assignees` | array of strings | no | Usernames to assign |

### github_list_prs

List pull requests for a repository.

- **Endpoint:** `GET /repos/{owner}/{repo}/pulls`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `owner` | string | yes | Repository owner |
| `repo` | string | yes | Repository name |
| `state` | string | no | Filter: `open`, `closed`, `all` (default `open`) |
| `per_page` | integer | no | Results per page (default 30) |

### github_get_pr

Get a single pull request by number.

- **Endpoint:** `GET /repos/{owner}/{repo}/pulls/{number}`
- **Destructive:** No

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `owner` | string | yes | Repository owner |
| `repo` | string | yes | Repository name |
| `number` | integer | yes | PR number |

### github_comment_issue

Add a comment to an issue or PR. **Destructive — requires approval.**

- **Endpoint:** `POST /repos/{owner}/{repo}/issues/{number}/comments`
- **Destructive:** Yes

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `owner` | string | yes | Repository owner |
| `repo` | string | yes | Repository name |
| `number` | integer | yes | Issue or PR number |
| `body` | string | yes | Comment body (Markdown) |

## Error Handling

- **401 Unauthorized:** Token invalid or revoked. User must re-authorise via OAuth.
- **403 Forbidden:** Insufficient scopes or secondary rate limit hit.
- **422 Unprocessable Entity:** Validation error (missing required fields, invalid values).
- **Rate limits:** GitHub returns `X-RateLimit-Remaining` and `X-RateLimit-Reset` headers. Surface remaining quota in tool responses when below 100.
