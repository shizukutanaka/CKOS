# Multi-stage build for the `ckos` binary (§931/§932).
# Stage 1: build the workspace in release mode.
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ckos-cli

# Stage 2: minimal runtime image with just the binary. No runtime
# dependencies to install — CKOS is std-only, so the binary is the image.
FROM debian:stable-slim
RUN useradd -m ckos && mkdir -p /data && chown ckos:ckos /data
COPY --from=build /src/target/release/ckos /usr/local/bin/ckos
USER ckos
WORKDIR /home/ckos

# Session state (documents + graph.kg) lives here. Declared a volume so it
# survives `docker run --rm` and can be bind-mounted; without it every session
# a container creates dies with the container.
VOLUME ["/data"]
EXPOSE 8080

ENTRYPOINT ["ckos"]

# The gateway, not `help`. An image whose default command exits immediately
# cannot be deployed: a Kubernetes Deployment restarts the container, so
# `args: ["help"]` produced a permanent CrashLoopBackOff (verified locally —
# `ckos help` exits 0). `serve` is the only long-running mode CKOS has.
#
# `--host 0.0.0.0` is required *inside a container*: binding the CLI default
# of 127.0.0.1 would make the server unreachable from outside the container's
# network namespace, including from a published port. This widens the listen
# address, not the trust model — the gateway still has no TLS and no
# authentication (see the `ckos_web` crate docs), so publish this port only on
# a trusted network or behind a reverse proxy that adds them.
#
# Any other subcommand still works: `docker run ckos plan "research X"`.
CMD ["serve", "--host", "0.0.0.0", "--port", "8080", "--session-root", "/data"]
