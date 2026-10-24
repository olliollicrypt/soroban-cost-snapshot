# Security policy

Please report security problems privately through GitHub's
"Report a vulnerability" button (Security tab → Advisories) on this
repository rather than in a public issue.

In scope: the `soroban-cost` CLI, the `cost_snapshot!` macro and the GitHub
Action in `action.yml` (for example: report injection, path traversal,
crashes on crafted snapshot files, workflow-injection in the Action).

This project measures costs in tests only; it never handles keys, signs
transactions or talks to the network.
