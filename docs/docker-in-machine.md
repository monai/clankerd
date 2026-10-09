# Docker inside the machine (M2 hand-off for ticket 10)

Everything here was tested on Linux against stubs only (`crates/vmctl/tests/{image_changes,smoke_script,host_gateway}.rs`). Nothing has booted real Docker yet; the smoke test is how you find out.

## 1. Change the `slim` image (you apply these)

The files to copy are in `contrib/image/slim/`:

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

COPY contrib/image/slim/daemon.json /etc/docker/daemon.json
COPY contrib/image/slim/clankerd-dockerd /usr/local/bin/clankerd-dockerd
```

Entrypoint: call the helper before running the command. It does nothing unless `/.clankerdenv` exists (guestd writes it in a clankerd machine), so the same image keeps working under plain `docker run`. If your entrypoint is a shell script, add near the top:

```sh
clankerd-dockerd            # starts dockerd and waits for its socket, or exits 1 with dockerd's log
exec "$@"
```

If the image's user is not root (the `agent` user), the helper runs `sudo -n dockerd`, so that user needs passwordless sudo for `dockerd` (`agent ALL=(root) NOPASSWD: /usr/bin/dockerd`), or run the entrypoint as root and drop privileges afterwards. Docker needs root either way. Also add the `agent` user to the `docker` group so the CLI works over `/var/run/docker.sock`.

Push the result as `ghcr.io/monai/clankers:slim` (linux/arm64).

## 2. Run the smoke test

```sh
make rust
scripts/m2-smoke-test.sh
```

It boots the image with a 20G volume, publishes guest port 8080 on `127.0.0.1:8080` and exposes a host loopback port (18081) to the guest, then walks: interactive exec, dockerd in nftables mode, `docker run -p 8080:80 nginx` fetched from the Mac, internet from a container, a compose project with a user-defined network, the host loopback port from the guest, stop/start with the volume and Docker's images intact, a changed image (`ALT_IMAGE`, default `debian:bookworm-slim`) on the same volume, and a second machine from the cached image. It cleans up after itself. Variables are listed at the top of the script. A fully interactive `vmctl run -it` / `exec -it` session is the one thing it cannot do without a terminal: try that by hand.

## Validated on the Mac (2026-10-09)

`scripts/m2-smoke-dind.sh` passed end to end with Docker 29.9.0 on the stock
libkrunfw kernel. The image was `127.0.0.1:5050/clankerd-dind:smoke`, derived
from the official `docker:29-dind` image with nftables, a clankerd entrypoint and
Docker storage on the named volume. Preparation, build and publishing are in
`scripts/m2-prepare-dind-image.sh`, `scripts/m2-build-dind-image.sh` and
`scripts/m2-publish-dind-image.sh`; publishing reuses the existing port-5050
registry.

Verified: interactive stdin, nftables mode, nginx fetched from the Mac,
container internet access, Compose on a user-defined network, a host loopback
service reached from the guest, Docker data and volume persistence across
restart, image changes preserving the volume, forced removal, and cached
creation in 0 seconds. This verifies the runtime with the derived DinD image;
`ghcr.io/monai/clankers:slim` still lacks the Compose plugin observed in testing.

## 3. Implementation checks and remaining verification

1. **nftables mode on the stock libkrunfw kernel.** The check greps `docker info` for `nftables` and runs `nft list tables` as root to find a Docker table. The Docker CLI still runs as the image's user. Docker 29 marks the backend experimental, and the kernel needs nf_tables, nft NAT/masquerade, bridge netfilter and conntrack. On failure the script prints the dockerd log (`/var/log/dockerd.log`).
2. **Storage driver.** Docker picks overlayfs on the ext4 volume if the kernel has `overlay`; otherwise it falls back to `vfs` (slow, large). `docker info | grep -i storage` shows which.
3. **cgroup v2 delegation.** guestd delegates controllers; dockerd needs `cpu`, `memory`, `pids` at least.
4. **Published port chain.** Mac 127.0.0.1:8080 -> vsock tunnel -> guest :8080 -> docker-proxy/nftables DNAT -> container :80. The vmctl `-p` publication is fixed for the machine's life and is re-bound on `start`.
5. **Host-gateway port.** The guest reaches the host's loopback port on its own `127.0.0.1:PORT` (a tunnel, not the gvproxy alias, which stays blocked).
6. **Name resolution inside containers.** Docker's embedded DNS forwards to the guest's `/etc/resolv.conf` from gvproxy.

## 4. If nftables mode fails

Record the result (dockerd log, `docker info`, `zcat /proc/config.gz | grep -E 'NF_TABLES|NFT_|NETFILTER_XT|BRIDGE_NETFILTER'` if present) in this file under a new "Result" heading, then evaluate the fallback in the spec: boot the Kata Containers kernel through `krun_set_kernel` instead of libkrunfw (a kernel path option in clankerd-vmspawn; the Kata arm64 kernel ships the netfilter and overlay options Docker wants). Do not add the fallback speculatively.

## Ticket 10 checklist for you

- [ ] Image changes applied and `ghcr.io/monai/clankers:slim` rebuilt
- [x] dockerd starts in nftables mode on the stock kernel
- [x] `docker run -p` and a compose project with a user-defined network work; containers reach the internet
- [x] nginx is reachable from the Mac through the published port
- [x] `scripts/m2-smoke-dind.sh` passes end to end with the derived DinD image
- [ ] The default `ghcr.io/monai/clankers:slim` image passes the Compose smoke after its image changes
- Not needed for this validation: nftables worked on the stock kernel; the Kata fallback was not evaluated.
