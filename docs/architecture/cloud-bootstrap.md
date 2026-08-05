# Bootstrap Delivery

Ployz has one machine-bootstrap primitive. Delivery may happen on the target
machine, over SSH, or through a Cloud-provided command, but delivery does not
choose cluster truth or create a second bootstrap protocol.

Cloud is an ordinary mesh peer and API consumer. It may deliver a verified
bootstrap artifact and a join token, but it is not runtime authority, a
membership database, or a recovery authority.

## Delivery paths

On the target machine:

```sh
curl -fsSL https://ployz.sh | sh
sudo ployz init
```

From an operator laptop:

```sh
ployz init root@<ip>
```

Cloud-assisted delivery uses the same primitive with Cloud-provided input:

```sh
curl -fsSL https://ployz.sh | sh
sudo ployz init --cloud-token <token>
```

The command installs the verified artifact, creates the local machine
identity, initializes the Corrosion store, and establishes the first mesh
relationship. Later machines join through the operator's SSH path or a
revocable join token presented to a machine's join door.

## Boundaries

Bootstrap delivery must not:

- create Founder and Joiner modes;
- use NATS, a central core, or a promoted machine;
- make Cloud the owner of cluster configuration;
- write machine membership without the machine's admission path;
- carry secret values into shared Corrosion rows.

After bootstrap, callers use HTTP/JSON and SSE over the mesh. Cluster
configuration converges through Corrosion rows, while each machine's Keeper
owns only its local substrate convergence and status testimony.

For the current machine-one contract, see
[the init design](../design/ployz-init-machine-one.md). For the consistency and
membership rules, see [the backbone](backbone.md) and
[ADR 0040](../adr/0040-corrosion-replaces-the-core-and-nats.md).
