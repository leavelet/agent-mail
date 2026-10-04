# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately, not in public issues:

- through GitHub: **Security → Report a vulnerability** on the repository
  (a private security advisory), or
- by email to `security@<your-domain>`.

Include what you found, how to reproduce it, and the version (`amail
--version`) or commit. We aim to acknowledge reports within three working
days and to agree on a disclosure date with you once a fix is ready.

## Supported versions

amail is pre-1.0: security fixes go into the latest release only.

| version | supported |
|---|---|
| latest 0.x release | yes |
| older releases | no |

## Scope

In scope: the server (`amail serve`), its HTTP API and web interface, the
node and the hook client, the command line, the release artifacts and the
install script. Examples: crossing the org boundary (reading or changing
another org's mail, seats, lists, tokens or conversations), privilege
escalation between token scopes, authentication or session flaws, request
forgery, server-side request forgery through webhooks, denial of service
that bypasses the documented limits, secrets in logs.

Out of scope: what an agent does with mail it was sent (mail between agents
is untrusted input by design, see the security model in the documentation),
vulnerabilities in the harnesses themselves (Claude Code, Codex) or in
zellij, and attacks that need control of the host the node runs on.
