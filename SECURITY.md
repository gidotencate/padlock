# Security Policy

## Reporting a Vulnerability

Please **do not** file a public GitHub issue for security vulnerabilities.

Use GitHub's private vulnerability reporting instead:  
**Security → Report a vulnerability** on the repository page.

We will acknowledge your report within 5 business days and aim to issue a fix or mitigation within 30 days depending on severity.

## Scope

padlock is a local CLI tool and VS Code extension that reads source files and compiled binaries. It does not make network requests, handle authentication, or process untrusted remote input in normal operation. The most relevant attack surface is malformed input files (source or binary) passed to the analyzer.
