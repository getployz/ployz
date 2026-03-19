# ployz

Ployz lets you deploy containerized apps across one machine or a small fleet without Swarm or Kubernetes.

It is built for the common case:

- start on one machine
- add more machines when you need them
- deploy apps with simple CLI commands
- keep the operational model small

There is no separate control plane to babysit. Machines share cluster state directly and the CLI can talk to any one of them.

## Quick Start

Install or reconfigure the local daemon:

```bash
ployz daemon install --runtime docker
```

Create and start your first network:

```bash
ployz mesh init my-network
ployz mesh ready --json
```

Deploy an app:

```bash
ployz deploy service whoami \
  --image traefik/whoami:latest \
  -p 8080:80
```

Check what is running:

```bash
ployz status
ployz machine ls
ployz mesh status my-network
```

## Add More Machines

Bootstrap a remote founder:

```bash
ployz machine init root@your-server --network prod
```

Add more machines to the current network:

```bash
ployz machine add root@10.0.0.12 root@10.0.0.13
```

Use a specific SSH key for one add operation:

```bash
ployz machine add --identity ~/.ssh/id_ed25519 root@10.0.0.12
```

## Deploy From A Manifest

Preview:

```bash
ployz deploy preview -f manifest.json
```

Apply:

```bash
ployz deploy -f manifest.json
```

Minimal example:

```json
{
  "namespace": "demo",
  "services": [
    {
      "name": "whoami",
      "placement": { "replicated": { "count": 1 } },
      "template": { "image": "traefik/whoami:latest" },
      "network": "overlay",
      "service_ports": [
        { "name": "http", "container_port": 80, "protocol": "tcp" }
      ],
      "publish": [
        { "service_port": "http", "host_port": 8080 }
      ],
      "restart": "unless-stopped"
    }
  ]
}
```

## Main Commands

```bash
ployz daemon install --help
ployz mesh --help
ployz machine --help
ployz deploy --help
```

Use `--json` when you want machine-readable output.

The root `package.json` exists so `npx ployz` can invoke `ployz.sh`.
