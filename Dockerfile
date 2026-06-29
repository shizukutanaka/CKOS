# Multi-stage build for the `ckos` CLI (§932 dev image).
# Stage 1: build the workspace in release mode.
FROM rust:1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p ckos-cli

# Stage 2: minimal runtime image with just the binary.
FROM debian:stable-slim
RUN useradd -m ckos
USER ckos
WORKDIR /home/ckos
COPY --from=build /src/target/release/ckos /usr/local/bin/ckos
ENTRYPOINT ["ckos"]
CMD ["help"]
