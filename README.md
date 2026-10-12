# Argus

Lightweight self-hosted monitoring. Small agents report host metrics and check results, two nodes watch each other,
one dashboard is generated from what the agents report, and alerts reach your phone as encrypted web push
notifications, with no app store app and no relay service in between.

- **Agent**: one static binary for Linux (amd64, arm64) and FreeBSD, a container image and a Home Assistant add-on.
  Collects CPU, memory, load, filesystems, network per interface, temperatures per sensor, mdraid, NVMe SMART, Docker
  containers, failed systemd units and pending reboots, and runs http, tcp, ping, file age and exec checks.
- **Node**: stores everything in SQLite, evaluates alert rules, serves the dashboard and sends web push notifications.
  Two nodes sync every 15 seconds and take over for each other.
- **Dashboard**: Leptos and uPlot, built into the node binary. Installed on the home screen it is also the app that
  receives the notifications.

## How it fits together

```
agents ──HTTPS──▶ home node ◀──sync, started by home──▶ vps node ◀──HTTPS── agents
                     │                                     │
                     └──▶ push service (Apple, Google) ◀───┘
                                     │
                                     ▼
                         Argus on your phone's home screen
```

- Every host belongs to one node (its `site`). That node evaluates the host's rules and sends its alerts.
- Agents at home report to both nodes, so the vps node can take over when the home node is gone. After 60 seconds
  without sync each node takes over the hosts it still receives reports from. An alert that was already sent is
  never sent again.
- The node that raises an alert encrypts it for each phone and hands it to the phone's push service directly. The
  text travels inside the encrypted message, so the phone never has to reach a node to read it.
- Nothing ever connects into the home network: the home node starts every sync.

## Node

```sh
argus-node serve --config /data/argus.json
argus-node check --config /data/argus.json   # validates the config without starting
argus-node token       # prints a new random token
argus-node password    # reads a password from stdin and prints its argon2 hash
```

Everything is configured either in the dashboard under Settings or in the JSON file, whichever you prefer. Both
stay in sync: the dashboard writes the file, and changes to the file apply within a few seconds. If the file does
not exist the node creates it with defaults and a random dashboard password, printed once in the log. Passwords and agent tokens may be written in clear text, the node
replaces them with their hashes on the next load. The dashboard never shows secrets, only whether one is stored.

`crates/argus-node/node.example.json` shows every option. The important parts:

- `name`: this node's name, which is also the `site` of the hosts it owns.
- `peer`: the other node. The node that sets `url` starts the sync, both use the same `token`.
- `zerobyte`: webhook token and optional API access for backup monitoring.
- `dashboard`: optional `password` that turns on the dashboard login.
- `shared`: settings both nodes keep identical. The node with the newer `revision` wins, on equal revisions the node
  that starts the sync wins.
  - `agents`: host id to that host's token or its sha256.
  - `checks`: http, tcp and ping checks per host id. The node hands them to that agent with the reply to its next
    report. Commands and file checks only exist in the agent's own config.
  - `rules`: metric thresholds. `metric` is `group|item|field` with `*` as wildcard, `hosts` accepts globs and
    `!exclusions`, `for` is how long the condition must hold. Host unreachable, failing checks, degraded RAID, SMART
    warnings, failed systemd units and pending reboots always alert.
  - `push`: the contact address the push services ask for, the key pair (created on the first start), the
    addresses where phones confirm receipt, an optional daily status message and the registered devices.
  - `backup_max_age` and `retention`.

Removing a host from `agents` also removes it from the dashboard. Its history stays until retention removes it.

Run the node behind a reverse proxy that terminates TLS. Example with Caddy for a node whose dashboard stays private:

```
monitor-vps.example.net {
	@public path /api/v1/ingest /api/v1/sync /api/v1/push/receipt/*
	handle @public {
		reverse_proxy argus-node:8090
	}
	respond 404
}
```

## Agent

```sh
argus-agent run --config /etc/argus/agent.toml
argus-agent once --config /etc/argus/agent.toml   # print one report as JSON, sends nothing
```

`crates/argus-agent/agent.example.toml` documents every option. Besides the checks in its own config the agent runs the http, tcp and ping
checks the node hands out for it. Set `checks = false` on a `[[nodes]]` entry to ignore them, for example on the
node that is reachable from the internet.

### Linux in Docker

```yaml
services:
  argus-agent:
    image: ghcr.io/antndev/argus-agent:latest
    pid: host
    network_mode: host
    read_only: true
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    restart: unless-stopped
    volumes:
      - /:/host:ro,rslave
      - ./agent.toml:/etc/argus/agent.toml:ro

  socket-proxy:
    image: tecnativa/docker-socket-proxy:0.3.0
    environment:
      CONTAINERS: 1
    ports: ["127.0.0.1:2375:2375"]
    volumes:
      - /var/run/docker.sock:/var/run/docker.sock:ro
```

Set `root = "/host"` and, because the agent uses the host network, `docker = "http://127.0.0.1:2375"` in the agent
config. `rslave` makes filesystems mounted after the container started (a late RAID, a remount after maintenance)
visible to the agent; Docker Desktop does not support it, drop it there. For NVMe SMART add `devices: ["/dev/nvme0"]` and `cap_add: [SYS_ADMIN]`.

### FreeBSD and OPNsense

Download `argus-agent-freebsd-amd64` from the release, then as root:

```sh
install -m 755 argus-agent-freebsd-amd64 /usr/local/bin/argus-agent
mkdir -p /usr/local/etc/argus && vi /usr/local/etc/argus/agent.toml
argus-agent install --config /usr/local/etc/argus/agent.toml
sysrc argus_agent_enable=YES && service argus_agent start
```

On OPNsense the interface descriptions come from `/conf/config.xml` automatically.

### Home Assistant

Add this repository in the add-on store (`https://github.com/antndev/argus`) and install "Argus agent". Protection
mode stays on.

## Phone notifications

Argus uses the web push standard. The node encrypts every notification for each registered phone (RFC 8291) and posts
it to that phone's push service, Apple's on an iPhone. The push service only sees encrypted bytes. No account, app
store app or third-party relay is needed.

1. Turn on Settings, Notifications and save. The node creates its push key pair.
2. On the iPhone (iOS 16.4 or later) open the dashboard in Safari over HTTPS, tap Share, Add to Home Screen, and open
   Argus from the home screen.
3. In Settings, Notifications tap Enable and allow notifications.

The phone confirms every notification to the addresses in `push.receipt_urls`, so list the dashboard and the node
that is reachable from outside. Settings, Notifications shows which messages were confirmed. With `status_every`
set, the node sends a short all clear at that interval so a broken path shows up within a day. A phone that ends its
subscription is removed automatically and logged.

## ZeroByte

- **Webhook**: in ZeroByte add a notification destination of type Generic Webhook with method POST, JSON enabled and
  the keys `title` and `message`, URL `https://<node>/api/v1/hooks/zerobyte/<webhook_token>`. Enable start, success,
  warning and failure on every backup schedule. Add the node's origin to ZeroByte's `WEBHOOK_ALLOWED_ORIGINS`.
- **Mirrors**: ZeroByte reports S3 mirror syncs only when they fail. Set `url` and `api_key` in `zerobyte` and the
  node also polls the API for the last mirror sync of every schedule.
- Argus alerts when a backup or mirror fails or when no success arrived within `shared.backup_max_age` (default 25 h).

## Development

```sh
cargo test --workspace --exclude argus-ui
rustup target add wasm32-unknown-unknown && cargo install trunk --locked
trunk serve            # dashboard on :8080, proxies /api to a node on :8090
```

To try alert paths without breaking anything, set `"simulate": true` (Settings, General, Fault simulation). The
dashboard then fakes problems on real hosts with keyboard shortcuts, press `?` for the list. Rules, alerts and
notifications react as if the problem were real.

## License

[PolyForm Noncommercial 1.0.0](LICENSE). Commercial use requires a separate license. Third-party notices are in
[NOTICE.md](NOTICE.md), the threat model in [SECURITY.md](SECURITY.md).
