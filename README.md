# ployz

Minimal npm package for the `ployz` CLI entrypoint.

## Usage

```bash
npx ployz
```

## Experimental v2 MVP

The BEAM/Mnesia v2 slice is exposed through Mix while packaging remains on the
existing `ployz` entrypoint:

```bash
mix ployz manifest check path/to/ployz.yml
mix ployz machine add node-a
mix ployz machine remove node-a
mix ployz deploy path/to/ployz.yml
mix ployz cert issue example.com
mix ployz migrate-volume data --to machine-a
mix ployz gateway routes
mix ployz status
```

The native MVP manifest is intentionally tiny:

```yaml
service: web
image: ghcr.io/example/web:sha
instances: 2
env:
  DATABASE_URL: ployz-secret://prod/database-url
routes:
  - host: example.com
    path: /
    port: 4000
volumes:
  - name: data
    mount: /data
```

Use `just test-v2` for the focused Elixir tests and `just test-v2-e2e` for the
Docker-backed deploy path. The Docker e2e is tagged separately and prints a
clear skip message when Docker is not installed, the daemon is unreachable, or
the test image cannot be pulled.
