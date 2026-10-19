FROM docker:29-dind
RUN apk add --no-cache nftables \
 && docker compose version \
 && nft --version
ENV DOCKER_HOST=unix:///var/run/docker.sock
ENV DOCKER_TLS_CERTDIR=""
COPY daemon.json /etc/docker/daemon.json
COPY clankerd-dockerd clankerd-dind-entrypoint /usr/local/bin/
RUN chmod 0755 /usr/local/bin/clankerd-dockerd /usr/local/bin/clankerd-dind-entrypoint
ENTRYPOINT ["clankerd-dind-entrypoint"]
CMD ["sleep", "infinity"]
