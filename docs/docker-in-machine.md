# Docker inside a machine

The current `images/slim/Dockerfile` builds a minimal Debian image with networking
tools. It does not install Docker or Compose.

`images/slim/daemon.json` and `images/slim/clankerd-dockerd` are optional ingredients
for a Docker-enabled image, not integrated into the current Dockerfile. The config
uses the nftables firewall backend and `/storage/docker`; a derived image needs a
compatible Docker installation, permissions to start dockerd, and a persistent
volume mounted at `/storage`.

Native Docker and Compose verification remains separate from machine lifecycle
tests. See the [verification skill](../.agents/skills/verify-clankerd/SKILL.md).
Container port publication belongs to Docker inside the machine; clankerd exposes
no machine port-publication flag.
