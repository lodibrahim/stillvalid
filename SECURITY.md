# Security Policy

## Reporting a vulnerability

Please **do not** open a public issue for security problems.

Report privately through GitHub: **[Report a vulnerability](https://github.com/lodibrahim/stillvalid/security/advisories/new)**.

Include what you found, how to reproduce it, and what an attacker could do with it. You should get a reply within 7 days. Once a fix is released, the advisory is published with credit to you unless you prefer otherwise.

## Supported versions

stillvalid is pre-release. Only the latest commit on `main` (and, once releases exist, the latest release) receives security fixes.

## Scope

stillvalid reads a GitHub token (`GITHUB_TOKEN` or `gh auth token`) and, in `pro-ai` mode, an LLM API key. Issues that could leak these, write to a repository when outputs are disabled, or let issue content inject commands are in scope.
