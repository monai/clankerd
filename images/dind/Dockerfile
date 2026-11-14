FROM docker:29-dind
RUN apk add --no-cache nftables
ENV DOCKER_HOST=unix:///var/run/docker.sock
COPY daemon.json /etc/clankerd/docker.json
COPY clankerd-dind-entrypoint /usr/local/bin/
RUN chmod 0755 /usr/local/bin/clankerd-dind-entrypoint
ENTRYPOINT ["clankerd-dind-entrypoint"]
