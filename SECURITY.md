# Security Policy

## Supported versions

Security fixes are accepted for the current `0.1.x` release line of fetchling.

## Reporting a vulnerability

fetchling is a local CLI that speaks untrusted HTTP(S)/FTP(S) protocols and writes files. Please report security issues that could allow:

- Path traversal or unexpected local filesystem writes from remote content
- Credential leakage (logs, cookies, netrc, argv)
- TLS authentication bypass when certificate checking is enabled
- Other remotely triggerable integrity or confidentiality failures

### GitHub Security Advisory

Open a [GitHub Security Advisory](https://github.com/xychelsea/fetchling/security/advisories/new) on the repository (private disclosure).

If that is unavailable, open a private report via the repository maintainers or a public issue without exploit detail when disclosure must stay limited.

Please include:

- Affected version / commit
- Description of the impact
- Steps to reproduce on a local/test endpoint (no attacks against third-party systems)

## Non-goals / known footguns

Some options intentionally weaken security for compatibility (for example `--no-check-certificate`, `--ftps-fallback-to-ftp`). Those are documented in the README; misuse of explicit insecure flags is outside the vulnerability process unless the default configuration is unsafe.
