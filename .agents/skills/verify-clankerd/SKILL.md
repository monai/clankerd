---
name: verify-clankerd
description: Selects and runs clankerd CLI and library verification in Linux, and prepares stable native VM test handoffs for a macOS operator.
---

# Verify clankerd

**Local checks** run in the Linux development container. **Host journeys** exercise actual Linux VMs on Apple Silicon macOS. This skill is a draft. Native journeys remain unproved until an operator executes them against the matching signed bundle.

## Steps

1. Read [the surface map](references/surface.md). Select groups from the public promises affected by the change. Use historical scripts to discover affected surfaces, then derive assertions from docs, public types and callers.
2. Run local checks first. Read [execution and evidence](references/execution.md) for commands, prerequisites and limitations.
3. For native changes, prepare a handoff for the macOS operator. Keep the coding agent in Linux. Request one or more stable groups even when they cover more than the changed behavior. Keep long recovery, workload and distribution checks separate from ordinary VM journeys.
4. Inspect returned command output, exit statuses and resulting state. Record failures and missing prerequisites separately. A skipped or scaffold-only journey supplies no native proof.
5. Clean up only resources created by the run. Retain evidence independently of scratch state.

## Output format

```text
Change: <public promise affected>
Local evidence: <commands, results, artifact paths>
Host request: <revision and dirty patch identity; artifact build; selected groups>
Prerequisites: <operator environment and fixtures>
Commands: <short stable commands from the checked-in verification interface>
Return: <evidence directory and summary>
Unverified: <remaining promises and why>
```

Example for a bind-permission change.

```text
Change: Complete bind semantics preserve guest ownership across restart.
Local evidence: Workspace tests and lint passed. Native permissions remain unverified.
Host request: Use the same revision and patch as this checkout. Build the signed bundle and run the existing bind-semantics subset.
Prerequisites: Apple Silicon macOS, native build dependencies, writable temporary Mac directory.
Commands: mise exec -- cmake --build --preset default --target test-native
Return: Build output, test output and exit status.
Unverified: The existing native target covers bind semantics only. Named-volume persistence remains unverified. The broader storage group is not implemented.
```

Use exact revision identifiers and actual results in a real handoff. Read the checked-in verification scaffold before requesting its group commands. Do not invent a command for a planned group.
