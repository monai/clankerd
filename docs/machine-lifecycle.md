# Machine lifetime

Guestd is the guest init process (PID 1 on native boots). It starts the OCI
image ENTRYPOINT/CMD once per boot as a child process. The command may succeed,
fail, or exit because stdin is closed; guestd keeps serving exec sessions and
network configuration afterward. A command finishing does not shut down or
restart the machine.

| Operation | Behavior |
|---|---|
| `create` | Saves machine configuration without booting. |
| `start` | Boots guestd and starts the configured image command. |
| `run` | Creates and starts a machine, then returns the image command's exit code. The machine stays running. |
| `run -d` | Creates and starts a machine, returning immediately. |
| `exec` | Runs a separate process, including after the image command has finished. |
| `stop` | Requests shutdown; signals a running image command with its configured stop signal, then tears down the guest. Works if the command already ended. |
| `kill` | SIGKILL terminates the VM immediately; other signals target the image command if it is running. |
| `start` after `stop` | Boots the saved machine again and reruns ENTRYPOINT/CMD. |

A shell started without stdin or a terminal can exit immediately. That is a
completed image command, while `ps` still shows the machine running. There is
no need to replace it with `sleep infinity`.

The library exposes separate waits: `Machine::wait_workload` observes the
command's result; `Machine::wait` waits for the machine itself to end. Attached
command streams end when that command ends. Guest networking and host bindings
are retained until the machine stops.

Restart policies use Docker-style names but apply to an ended machine, not to
an ordinary command exiting. Unexpected VMM termination is a machine failure,
even if its image command previously exited successfully. Explicit stop requests
are persisted per VMM PID so another CLI observer does not restart a machine
that was deliberately stopped.

Native validation passed in Mac suite `/tmp/clankerd-network-gates.JSPlld`: command exit retained guest init, exec, Mac TCP/UDP and NAT; explicit stop, restart, cleanup, network recovery and Docker end-to-end checks passed.
