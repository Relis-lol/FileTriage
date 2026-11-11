# Security

Please report suspected vulnerabilities privately to [kontakt@saveroq.com](mailto:kontakt@saveroq.com). Do not publish exploit details in a GitHub issue before a fix is available. This project does not currently offer a bug bounty.

FileTriage changes files, so its core follows a fail-closed, journal-first design. Every productive move, rename, delete, and rule-driven file operation must first be recorded in a persistent SQLite journal. If that journal is unavailable or recovery is unresolved, mutations are blocked while scanning and diagnosis remain available. Undo and recovery validate the recorded source action and never overwrite a conflicting file.

Deletion uses the Windows Recycle Bin by default. Permanent deletion is an explicitly enabled expert option with an additional per-action confirmation. Cross-volume moves copy to a temporary file, verify the result, revalidate the source, and only then remove the source.

The Community build has no telemetry, analytics, account, cloud upload, or application network calls. Its WebView uses a restrictive content-security policy and loads no remote scripts.

These controls reduce risk but are not a guarantee against data loss. Keep independent backups of important files. Known platform limitations and the detailed design are documented in [ARCHITECTURE.md](ARCHITECTURE.md).
