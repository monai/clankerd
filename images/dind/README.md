# Docker in Docker

```sh
docker run -d --name registry \
  -p 5001:5000 \
  -v registry-data:/var/lib/registry \
  registry:latest

docker build -t localhost:5001/clankerd-dind images/dind
docker push localhost:5001/clankerd-dind

vmctl=build/artifact/darwin-arm64/vmctl

"$vmctl" --insecure-registry localhost:5001 machine run -d \
  --name dind --memory 2G \
  -v dind-data:/storage \
  localhost:5001/clankerd-dind

"$vmctl" machine exec dind docker run --rm hello-world
"$vmctl" machine exec -it dind sh
```

## Host Docker context

```sh
docker context create dind --docker "host=tcp://$("$vmctl" machine inspect dind | jq -r '.[0].network.addresses[] | select(contains(":") | not)' | head -n 1):2375"
docker context use dind
docker ps
```

## Docker in Docker

```sh
docker --context default run --privileged -d --name nested-dind localhost:5001/clankerd-dind
docker --context default exec nested-dind docker run --rm hello-world
docker --context default rm -fv nested-dind
```
