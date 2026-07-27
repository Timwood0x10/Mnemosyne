# Security Policy

## Supported Versions

Only the latest released version on the `main` branch receives security
fixes.

| Version | Supported |
|---------|-----------|
| latest  | ✅        |
| older   | ❌        |

## Reporting a Vulnerability

**Please do not open a public GitHub issue for security vulnerabilities.**

Instead, report them privately by emailing the maintainer or by using
GitHub's private vulnerability reporting feature for this repository. Include:

- A description of the vulnerability and its impact.
- Steps to reproduce, or a proof-of-concept.
- Any suggested mitigation if you have one.

You can expect an initial acknowledgement within **5 business days**. Once
triaged, we will work with you on a fix and coordinated disclosure. We will
credit reporters who wish to be acknowledged.

## Scope notes

- `memory_distill` is an MCP server. Treat its `memory_*` tools as an
  untrusted input boundary: tool arguments are validated, but callers should
  still sandbox the process and avoid mounting sensitive files.
- SQLite database files and any configured embedding endpoint credentials
  (e.g. `MEMORY_OPENAI_API_KEY`) live on the host. Guard them with normal
  filesystem and environment permissions; do not commit them.
- The `SecurityFilter` drops obviously secret content (API keys, tokens) from
  distilled memories, but it is a heuristic — do not rely on it as a
  compliance control.
