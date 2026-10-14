# Security

## Reporting

Please report vulnerabilities privately to antonkonig691@gmail.com instead of opening a public issue.

## Threat model

Argus collects host metrics and check results and sends alerts. It never runs commands on monitored hosts and never
changes anything on them.

### Agents

- Agents only send. They open no port and accept no commands. Exec and file age checks exist only in the agent's local
  config. The reply to a report may carry http, tcp and ping checks configured on the node; the agent drops any other
  check type from a reply and never lets a node check replace a local one. Set `checks = false` on a node entry to
  ignore node checks from that node, for example from the internet-facing node, so a compromised node cannot use the
  agents for requests inside the home network.
- Every host has its own token. Nodes store only the SHA-256 of a token and bind it to one host id, so a stolen token
  can only report data for that one host.
- Reports travel over HTTPS through a reverse proxy in front of the node. The agent warns when a node URL is not HTTPS.
- The container image is a single static binary on `scratch`. Run it with `read_only: true`, `cap_drop: [ALL]` and
  `no-new-privileges`, mount the host root read-only and give Docker access only through a read-only socket proxy that
  allows container listing.
- NVMe SMART health needs the device and `CAP_SYS_ADMIN`. Leave `smart` empty if that is not acceptable, temperatures
  still come from hwmon without it.
- On FreeBSD the agent runs as root so it can read the NVMe log page. It still listens on nothing.

### Nodes

- The node container runs as an unprivileged user on `scratch`.
- Node to node sync uses a shared secret and is always started by one side, so the other side never needs a way in.
  The sync also carries the shared settings (agent token hashes, checks, rules, push keys and devices, retention), so
  a node holding the sync secret can change them on the other node.
- The config file holds the sync secret, the push key and agent token hashes, never the dashboard password. The node
  writes it with mode 600 and replaces clear text agent tokens with their hashes when it loads the file. Writes are
  atomic.
- Notifications are encrypted end to end with the web push standard (RFC 8291, aes128gcm) for each phone. The push
  service (Apple, Google, Mozilla) sees the size and time of a message, never its text. Requests to the push service
  are signed with the node's VAPID key (RFC 8292).
- The VAPID private key lives in the config file and never leaves the API in clear text. Whoever has it plus a
  device's subscription can send notifications to that device, not read them.
- Receipts (`/api/v1/push/receipt/<id>`) need no login. They only mark a known message as delivered, and message ids
  are random and travel inside the encrypted payload.
- Ingest and sync check their token before the body is read, so anonymous requests cannot make a node buffer large
  payloads. Other routes accept at most 1 MiB.
- The ZeroByte webhook is authenticated by a secret in its URL. A ZeroByte API key has the full rights of its ZeroByte
  user, so keep it in the node config with mode 600 and only set it if the mirror check is wanted.

### Dashboard

- The dashboard always needs a login. The password is never stored in the config file, only its argon2 hash in the
  node's database. A node without a password accepts a new one only within 15 minutes after its start or after
  `argus-node reset-password`, which needs access to the server. Sessions use an HttpOnly, SameSite Strict cookie
  (Secure behind HTTPS) for 30 days and live in memory. Failed logins wait one second, password checks run one at a
  time. Changing or resetting the password ends every session.
- The configuration API never returns secrets. They come back as `********`, and a save that keeps `********`
  keeps the stored value. Saves only accept JSON bodies, which a cross-site form cannot send, and fail when the
  configuration changed since it was loaded.
- A node that should not show the dashboard publicly exposes only `/api/v1/ingest`, `/api/v1/sync` and the receipt
  paths through its reverse proxy, the dashboard stays reachable through an SSH tunnel.

### Known limits

- Reports are not rate limited. A leaked agent token can fill the database with samples for its own host until the
  token is removed from the node config.
- Alerts carry host names and check names in clear text inside the nodes and the phone app.
