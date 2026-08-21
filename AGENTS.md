# AGENTS.md

## Host commands via proxy

This environment's bash is sandboxed (blocks some network + OS calls).
For git operations that need network, `curl`, builds, `pod install`, or
anything that fails with a sandbox/permission error, call the `exec_command`
tool from the `mcp-cli-proxy` server instead of bash.

`exec_command` runs the command on the host PC (outside the sandbox) via
`sh -c`, so pipes, globs, and redirects work. It returns stdout, stderr, and
exit_code as JSON. Prefer it whenever a normal bash command is blocked.
