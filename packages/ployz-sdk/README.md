# @ployz/sdk

TypeScript contracts for the Ployz operator API.

## Stability

This package is visible plumbing for Ployz Cloud and early operators. It is
0.x alpha software: third parties use it at their own risk, should pin exact
versions, and should expect breaking changes without a support or compatibility
promise.

## Install

```sh
npm install @ployz/sdk
```

Generated wire types are exported from `@ployz/sdk/generated` and re-exported
from the package root. The coreless HTTP/SSE transport lands with the v2 SDK;
this package deliberately exposes no incumbent transport in the meantime.
