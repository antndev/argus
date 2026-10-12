# Argus agent

Reports CPU, memory, load, the data partition, network and temperatures of the Home Assistant host to one or more
Argus nodes every 30 seconds. The agent only sends, it opens no port and accepts no commands. Protection mode stays
on.

## Options

- `id`: the host name shown in Argus, it must match the entry in the node's `[agents]` section.
- `site`: the node that owns this host, for example `home`.
- `interval`: seconds between reports.
- `nodes`: one entry per node with its URL and this host's token. Add the host on the node under Settings, Hosts
  and agents, which shows the token once. Optional `checks: false` ignores the http, tcp and ping checks that node
  hands out.

## Notes

- The add-on uses the host network so interface traffic is the real traffic of the host.
- Temperatures come from the kernel's hwmon sensors, for example the SoC and the NVMe drive.
