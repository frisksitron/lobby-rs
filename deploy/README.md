# Deploying a Lobby

This directory runs a Lobby on one Linux machine with Docker Compose. It is written for a small VPS
or a home server that a friend group shares.

## What you get

- **Caddy** takes ports 80 and 443 (tcp), gets and renews a Let's Encrypt
  certificate for your domain, and proxies to the server.
- **lobby-server** listens on `127.0.0.1:8080` for HTTP and WebSocket, and on `50000` (udp)
  for Voice and Screen Share. It also listens on `50000` (tcp) for Members whose network blocks
  UDP, such as some office and hotel Wi-Fi. Their clients fall back to it; everyone else stays on
  UDP. Caddy does not proxy this port.
- Both containers use the **host network**. On a Docker bridge, `docker-proxy` hides the address
  of every IPv6 client. Caddy would then rate-limit all of them as one address, and the SFU would
  see the proxy instead of the client. The host network keeps the real addresses and needs no NAT.
- State lives in `/opt/lobby/data` (the database and `blobs/`) and in two Caddy volumes.

## A fresh VPS

Ubuntu or Debian, with a public IPv4 address and ideally an IPv6 address. 1 vCPU and 1 GB of RAM
run a small Lobby, but the image needs more than that to build (see [Build paths](#build-paths)).

1. **Install Docker with the Compose and Buildx plugins.**

   ```sh
   curl -fsSL https://get.docker.com | sh
   ```

   On Ubuntu you can use the distribution packages instead. Without `docker-buildx`,
   `docker build` fails with "BuildKit is enabled but the buildx component is missing".

   ```sh
   apt install docker.io docker-compose-v2 docker-buildx
   ```

2. **Open the firewall and harden the host.** Open 80 and 443 over tcp, and 50000 over udp and
   tcp for the SFU (tcp is for Members whose network blocks UDP), for IPv4 and IPv6, and at the
   cloud provider's firewall too. Beyond that, the usual care for a public machine:

   - SSH with keys only, no passwords, and rate-limited (`ufw limit 22/tcp`).
   - Docker's logs capped (`max-size` and `max-file` in `/etc/docker/daemon.json`).
   - Automatic security updates (`unattended-upgrades`).
   - Optionally, a per-network cap on new connections to 80, 443 and 50000 (iptables
     `connlimit` and `hashlimit`, an IPv6 /48 counted as one network), so one attacker cannot
     hold every connection slot.

3. **Add DNS records** for your domain: an `A` record to the public IPv4 address, and an `AAAA`
   record to the IPv6 address if the machine has one. Caddy cannot get a certificate until they
   resolve.

4. **Create the data directory** for the container's user (uid 10001), readable by nobody else:
   the database holds every Member's email.

   ```sh
   install -d -m 0700 -o 10001 -g 10001 /opt/lobby/data
   ```

   Uploads go to `/opt/lobby/data/blobs`. Mount a size-capped filesystem there (a loop file in
   `/etc/fstab` works, so it is mounted before Docker starts), so uploads that fill it cannot
   stop the database. Give the mounted directory to uid 10001 as well.

5. **Store the SMTP password** (the API key for Magic Code mail) in `/opt/lobby/smtp_password`,
   readable only by the container's user. Compose mounts it as a secret with its host owner, and
   the server reads it through `LOBBY_SMTP__PASSWORD_FILE`. It never goes in `lobby.toml` or in
   the environment.

   ```sh
   install -m 0400 -o 10001 -g 10001 /dev/stdin /opt/lobby/smtp_password
   ```

   Paste the key, then press Ctrl+D. A deployed Lobby needs a mail provider: everyone signs in or
   joins with a Magic Code mailed to them, the first Member included, so without working mail
   nobody gets in. The server refuses `smtp.log_codes` (codes in the log, for local development)
   in a release build and behind a proxy.

6. **Copy this `deploy/` directory** to the machine (for example to `/opt/lobby/deploy`). Copy
   `.env.example` to `.env`, and set `LOBBY_DOMAIN` to your domain.

7. **Copy `lobby.compose.toml` to `lobby.toml`** and fill in the blanks.
   - `auth.bootstrap_email` is the email of the first Member, who becomes the owner.
   - `smtp.from` is the sender address your mail provider has verified.
   - `sfu.advertise_ips` lists the addresses clients send Voice and Screen Share to. Put the
     **public IPv4 first**, then the IPv6 address. The client tries the addresses in this order
     and uses the first that routes. A host with a broken IPv6 route would otherwise pick IPv6
     and never fall back.

8. **Get the image** with one of the [build paths](#build-paths), then start the stack.

   ```sh
   cd /opt/lobby/deploy
   docker compose up -d
   curl https://lobby.example.com/health
   ```

   The first start takes a minute: Caddy asks for the certificate on its first start.

9. **Create the first Member.** Open the client and join with the email in `bootstrap_email`. The
   server sends a Magic Code to that address. Every later Member joins by asking, and a Member
   Vouches for them.

## Build paths

**(a) Pull the published image.** Each GitHub Release publishes
`ghcr.io/frisksitron/lobby-server:<version>` (and `:latest`). Set `LOBBY_IMAGE` and `LOBBY_VERSION`
in `deploy/.env`, then run `docker compose pull` before `docker compose up -d`. Use the version of
the Lobby app your Members run: the app and the server must speak the same protocol.

To build the image yourself instead: the image builds only `lobby-server`. Run both commands from the repository root, because the
build context is the whole workspace (`.dockerignore` keeps `target/`, `.git/` and the desktop
assets out).

**(b) Build on your own machine, then ship the image.**

```sh
docker build -f deploy/Dockerfile -t lobby-server:<version> .
docker save lobby-server:<version> | gzip | ssh root@your-vps 'gunzip | docker load'
```

`<version>` is the `version` in the workspace `Cargo.toml`. Set `LOBBY_VERSION=<version>` in
`deploy/.env`. Compose then runs `lobby-server:<version>`.

**(c) Build on the VPS.** A 1 GB machine runs out of memory while it compiles, so add swap first.

```sh
fallocate -l 2G /swapfile && chmod 600 /swapfile && mkswap /swapfile && swapon /swapfile
echo '/swapfile none swap sw 0 0' >> /etc/fstab
```

Copy the source to the machine without `target/` (for example `git archive HEAD | ssh root@your-vps
'tar -x -C /opt/lobby/src'`), then run `docker build -f deploy/Dockerfile -t lobby-server:<version> .`
there. Expect tens of minutes on one vCPU.

The builder image is `rust:<channel>-bookworm`, and `cargo xtask check-toolchain` fails when its
tag drifts from `rust-toolchain.toml`.

## Home server

Forward ports 80 and 443 (tcp) and 50000 (udp and tcp) from your router to the machine, and give
the machine a fixed LAN address. In `advertise_ips`, list the public IP first and the **LAN IP
last**. Members on the same network then reach Voice directly and need no NAT hairpin. If your
public IP changes, update `advertise_ips` and restart.

## Operating it

The server has no command line besides `--config`. Members are managed in the app.

```sh
curl http://127.0.0.1:8080/health
docker compose logs -f lobby
```

Logs are JSON lines.

## Backup and restore

The database runs in WAL mode: recent writes sit in `lobby.db-wal`, and `lobby.db` alone can lack
all of them. Stop the server before copying the database and blobs together, so uploads and
deletions cannot change them during the backup.

1. `docker compose stop lobby`
2. Copy `/opt/lobby/data` (the database and `blobs/`) to the backup location.
3. `docker compose up -d lobby`

To restore, stop the container, replace `/opt/lobby/data` with the copy, run
`chown -R 10001:10001 /opt/lobby/data`, and start the container again. If you restore only
`lobby.db`, delete the old `lobby.db-wal` and `lobby.db-shm` first: they belong to the database
you replace.

## Upgrading

1. Back up (see [Backup and restore](#backup-and-restore)).
2. Build or load the new image, and set `LOBBY_VERSION` in `.env`.
3. `docker compose up -d lobby`

The new server runs its migrations on start. Docker stops the old one with SIGTERM and waits up to
20 seconds. The server drains, ends Voice, closes connections and writes a `ShutdownReport` to the
log, so `docker compose logs lobby` shows whether the stop was clean. Members reconnect on their
own, unless the protocol changed: a client whose `PROTOCOL_VERSION` differs stops at the
protocol check until it runs the matching app. A release that changes the protocol (the number is
`PROTOCOL_VERSION` in `crates/lobby-protocol/src/lib.rs` and `protocolVersion` in `GET
/api/v1/lobby`) moves the server and every Member's app together. Keep the previous image: setting
`LOBBY_VERSION` back and running `docker compose up -d lobby` rolls back the server. If the old
release speaks another protocol, roll the apps back with it: it refuses the newer app, so every
Member who updated needs the older app again.

## The SFU egress budget

Voice and Screen Share go through your machine, so its uplink is the limit.

- **Voice** costs 128 kbps per talking Member per listener. With Opus DTX, a silent unmuted Member
  costs about 1 kbps, and a Muted Member sends nothing. 25 Members in Voice with 3 talking is about
  9.2 Mbps for the talkers plus 0.5 Mbps for the silent ones. The worst case, all 25 talking, is
  about 77 Mbps. A Member with Studio Mode on costs 256 kbps per listener, silent or not.
- **Screen Share** costs the share's bitrate per Viewer, set by the size of the picture it sends:
  6 Mbps up to 1080p, 10 Mbps up to 1440p and 20 Mbps up to 4K. A Viewer watches one share at a
  time, so the SFU sends each Viewer at most 20 Mbps. Nothing adapts: a Viewer whose link cannot
  carry the share falls behind and waits for keyframes.
- A typical evening (one 4K share to 5 Viewers plus Voice) peaks near 102 Mbps. The same share
  at up to 1080p is about 32 Mbps.

On a small uplink, lower `voice.max_participants` and `voice.max_sharers` in `lobby.toml`, and
share at up to 1080p.

## Troubleshooting

- **Voice works over IPv4 but not IPv6.** Check that ufw allows 50000/udp on IPv6 and that
  `sfu.udp_bind` is `[::]:50000`, which is dual-stack on Linux.
- **Voice fails for one Member on one network (work, a hotel) and works elsewhere.** That network
  probably blocks UDP. Check that ufw allows 50000/tcp and that `sfu.tcp_bind` is `[::]:50000`.
  The server log says `media path path=Tcp` for a Member who connected over TCP.
- **Everyone gets rate-limited at once, or Sign In says too many attempts.** Caddy's address is not
  trusted. Check `http.trusted_proxy_cidrs` (`127.0.0.1/32` and `::1/128` with the host network)
  and that `http.bind` is `127.0.0.1:8080`.
- **Caddy cannot get a certificate.** Check that the DNS records resolve to this machine and that
  ports 80 and 443 are open at the provider's firewall as well.
- **The container restarts at once.** `docker compose logs lobby` names the invalid configuration
  key (exit code 78). A secret file the container's user cannot read is one cause.
