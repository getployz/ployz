# @ployz/sdk

TypeScript client for the Ployz operator API.

## Stability

This package is visible plumbing for Ployz Cloud and early operators. It is
0.x alpha software: third parties use it at their own risk, should pin exact
versions, and should expect breaking changes without a support or compatibility
promise.

## Install

```sh
npm install @ployz/sdk
```

## Use

```ts
import { connectPloyzNatsClient } from "@ployz/sdk";

const connection = await connectPloyzNatsClient({
  nats: { servers: "tls://127.0.0.1:4222" },
});

try {
  const operation = await connection.client.deploy({
    idempotencyKey: "deploy-001",
    namespaceId: "default",
    serviceId: "api",
    image: "ghcr.io/acme/api:rev-1",
    replicas: 1,
  });

  console.log(await operation.status());
} finally {
  await connection.close();
}
```

Generated wire types are exported from `@ployz/sdk/generated` and re-exported
from the package root.
