# syntax=docker/dockerfile:1.7
FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release -p lobby-server --locked \
 && install -m 0755 target/release/lobby-server /usr/local/bin/lobby-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 lobby \
 && useradd --system --uid 10001 --gid lobby --home-dir /data --shell /usr/sbin/nologin lobby \
 && install -d -o lobby -g lobby /data /etc/lobby
COPY --from=build /usr/local/bin/lobby-server /usr/local/bin/lobby-server
ENV LOBBY_LOG_FORMAT=json LOBBY_DATABASE__PATH=/data/lobby.db LOBBY_STORAGE__BLOB_ROOT=/data/blobs
WORKDIR /etc/lobby
USER lobby
VOLUME /data
EXPOSE 8080 50000/udp 50000/tcp
STOPSIGNAL SIGTERM
ENTRYPOINT ["lobby-server"]
