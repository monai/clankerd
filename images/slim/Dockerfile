FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates \
      iproute2 \
      nftables \
 && rm -rf /var/lib/apt/lists/*

CMD ["sleep", "infinity"]
