# Host a Lobby server

One Linux machine with Docker runs it. A small VPS with 1 vCPU and 1 GB of RAM is enough, and so
is a home server. You also need a domain name and an SMTP account: sign-in codes go out by email,
so without working mail nobody gets in, you included.

Run the commands as root.

## Set up

1. **Install Docker.**

   ```sh
   curl -fsSL https://get.docker.com | sh
   ```

2. **Open the ports** in the machine's firewall and in your provider's: 80 and 443 (tcp) for the
   app, and 50000 (udp and tcp) for voice and screen share.

3. **Point your domain at the machine.** Add an `A` record for its public IPv4 address, and an
   `AAAA` record if it has an IPv6 address.

4. **Make the data directory and the SMTP password file.** Both belong to the server's user,
   uid 10001.

   ```sh
   install -d -m 0700 -o 10001 -g 10001 /opt/lobby/data
   install -m 0400 -o 10001 -g 10001 /dev/stdin /opt/lobby/smtp_password
   ```

   The second command waits for input: paste the SMTP password, then press Ctrl+D.

5. **Copy this `deploy/` directory** to `/opt/lobby/deploy`, and make your two config files.

   ```sh
   cd /opt/lobby/deploy
   cp .env.example .env
   cp lobby.compose.toml lobby.toml
   ```

   In `.env`, set your domain and the published image:

   ```sh
   LOBBY_DOMAIN=lobby.example.com
   LOBBY_IMAGE=ghcr.io/frisksitron/lobby-server
   ```

   In `lobby.toml`, fill in three things and leave the rest as it is:

   - `auth.bootstrap_email`: your email. You become the first member and the owner.
   - `[smtp]`: your mail provider's `host`, `port` and `username`, and a `from` address the
     provider has verified. The file comes set up for Resend.
   - `sfu.advertise_ips`: the machine's public IPv4 address first, then its IPv6 address if it
     has one.

6. **Start it.**

   ```sh
   docker compose pull
   docker compose up -d
   curl https://lobby.example.com/health
   ```

   The first start takes about a minute while Caddy gets the HTTPS certificate.

7. **Sign in.** Open the Lobby app, enter your domain and sign in with the bootstrap email. Your
   friends ask to join from the app, and any member can let them in.

## On a home server

Give the machine a fixed LAN address, and forward ports 80 and 443 (tcp) and 50000 (udp and tcp)
to it from your router. In `sfu.advertise_ips`, put your public IP first and the LAN IP last. When
your public IP changes, update DNS and `advertise_ips`, then run `docker compose restart lobby`.

## Back up

Stop the server first, and copy the whole data directory, never the database file alone.

```sh
cd /opt/lobby/deploy
umask 077
docker compose stop lobby
tar -C /opt/lobby -czf /opt/lobby/backup-$(date +%F).tar.gz \
    data smtp_password deploy/.env deploy/lobby.toml
docker compose up -d lobby
```

Keep a copy of the archive on another machine. It holds every member's email and your SMTP
password.

To restore, stop the server, empty `/opt/lobby/data`, and unpack the archive over it:

```sh
docker compose stop lobby
tar -C /opt/lobby -xzf /opt/lobby/backup-2026-01-31.tar.gz
docker compose up -d lobby
```

## Upgrade

Back up, then pull the newest release and restart:

```sh
docker compose pull lobby
docker compose up -d lobby
```

The apps update themselves, and the server does not. Most releases work with an older server. A
release that changes the protocol does not: the apps cannot connect until the server runs it too.
Upgrading with every release avoids that.

To go back, restore the backup and name the old release in `.env`, such as `LOBBY_VERSION=0.1.5`.
The old server cannot open a database the new one has changed.

## Bandwidth

Voice and screen share pass through the server, so its upload speed is the limit. Screen share
needs about 6 Mbps per viewer at 1080p and 20 Mbps at 4K. Voice needs about 0.1 Mbps per talker
per listener. On a slow uplink, share at 1080p, and lower `voice.max_participants` and
`voice.max_sharers` in `lobby.toml`.

## Worth doing on a public machine

- Allow SSH with keys only, and turn on automatic security updates.
- Cap Docker's logs (`max-size` and `max-file` in `/etc/docker/daemon.json`).
- Mount a size-capped filesystem at `/opt/lobby/data/blobs`, owned by uid 10001, so uploads
  cannot fill the disk the database is on.

## When something is wrong

`docker compose logs -f lobby` shows the server's log.

- **No HTTPS certificate.** The domain must resolve to this machine, and ports 80 and 443 must
  be open at your provider too.
- **The server restarts at once.** The log names the cause: a wrong key in `lobby.toml`, or a
  file that uid 10001 cannot read.
- **No sign-in email.** Check the `[smtp]` settings, the password file and that your provider
  has verified the `from` address.
- **Voice fails for everyone.** Check `sfu.advertise_ips` and that 50000 (udp) is open.
- **Voice fails for one person on one network.** That network blocks UDP. Open 50000 (tcp).
- **Everyone is rate-limited at once.** Keep `http.bind` and `http.trusted_proxy_cidrs` as they
  are in `lobby.compose.toml`.

## Build the image yourself

From the repository root, on a machine with more than 1 GB of RAM:

```sh
docker build -f deploy/Dockerfile -t lobby-server .
```

Then leave `LOBBY_IMAGE` out of `.env`, and skip `docker compose pull`.
