# Docker inside a machine

## 1. Change the `slim` image (you apply these)

The files to copy are in `images/slim/`:

| File | Install as |
| --- | --- |
| `daemon.json` | `/etc/docker/daemon.json` |
| `clankerd-dockerd` | `/usr/local/bin/clankerd-dockerd` (mode 0755) |

`daemon.json` selects the nftables firewall backend and keeps Docker's data on the volume (`/storage/docker`, so `vmctl run -v data:/storage` is required):

```json
{ "firewall-backend": "nftables", "data-root": "/storage/docker" }
```

Dockerfile changes (replace Debian's `docker.io` with Docker CE 29+; Debian bookworm/trixie shown, adjust the codename):

```dockerfile
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl nftables sudo \
 && install -m 0755 -d /etc/apt/keyrings \
 && curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc \
 && . /etc/os-release \
 && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian ${VERSION_CODENAME} stable" \
      > /etc/apt/sources.list.d/docker.list \
 && apt-get update && apt-get install -y --no-install-recommends \
      docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
 && rm -rf /var/lib/apt/lists/* \
 && docker --version   # must be 29.x or newer: the nftables backend is new in 29

COPY images/slim/daemon.json /etc/docker/daemon.json
COPY images/slim/clankerd-dockerd /usr/local/bin/clankerd-dockerd
```

Entrypoint: call the helper before running the command. It does nothing unless `/.clankerdenv` exists (guestd writes it in a clankerd machine), so the same image keeps working under plain `docker run`. If your entrypoint is a shell script, add near the top:

```sh
clankerd-dockerd            # starts dockerd and waits for its socket, or exits 1 with dockerd's log
exec "$@"
```

If the image's user is not root (the `agent` user), the helper runs `sudo -n dockerd`, so that user needs passwordless sudo for `dockerd` (`agent ALL=(root) NOPASSWD: /usr/bin/dockerd`), or run the entrypoint as root and drop privileges afterwards. Docker needs root either way. Also add the `agent` user to the `docker` group so the CLI works over `/var/run/docker.sock`.

Push the result as `ghcr.io/monai/clankers:slim` (linux/arm64).

## Native verification

Docker and Compose run inside a real clankerd VM. Native workload verification remains planned in the [verification skill](../.agents/skills/verify-clankerd/SKILL.md). It must exercise dockerd startup, container Internet access, Compose networking, host reachability through the guest address, and named-volume persistence across restart and reimage. Interactive terminal behavior requires a terminal fixture or operator observation.

The historical derived-image validation does not establish the current default image. Check the selected image's Compose plugin, nftables backend, storage driver and cgroup delegation before interpreting workload failures. Container port publication is a Docker operation inside the VM; the clankerd CLI exposes no machine port-publication flag.
