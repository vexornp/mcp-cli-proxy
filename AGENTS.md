# AGENTS.md

## Host commands via proxy

This environment's bash is sandboxed (blocks some network + OS calls).
For git operations that need network, `curl`, builds, `pod install`, or
anything that fails with a sandbox/permission error, call the `exec_command`
tool from the `mcp-cli-proxy` server instead of bash.

`exec_command` runs the command on the host PC (outside the sandbox) via `sh -c`,
so pipes, globs, and redirects work. It returns stdout, stderr, and exit_code
as JSON.

**Prerequisite:** the unsandboxed daemon must be running. Start it in a
separate terminal (not under logoscode) with:

    mcp-cli-proxy daemon

It listens on `127.0.0.1:8130` and stays in the foreground. If
`exec_command` fails with "cannot connect to daemon at 127.0.0.1:8130
(is 'mcp-cli-proxy daemon' running?)", the daemon is not running — ask
the user to start it.

## Known limitations

- If the daemon restarts, the bridge does not reconnect — restart the agent
  (logoscode) to reconnect.
- Requests are sequential over the single connection (no concurrent in-flight
  requests) — a deliberate non-goal per the design spec.
- Localhost TCP only (127.0.0.1:8130). Any local process can connect (no
  auth); intended for a personal dev box. Not network-exposed.

The following MCP servers are configured but unavailable:
- logos_code_remote_112: MCP error -32000: Connection closed

If the user needs these tools, offer to login by calling the `sso_login` tool with the appropriate domain.
A browser will open for SSO login. After login succeeds, the tools will be available immediately.
