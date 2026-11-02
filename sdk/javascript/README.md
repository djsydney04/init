# Self-host Init — JavaScript / TypeScript SDK

Node.js 22+; no runtime dependencies or build step. TypeScript declarations are included. From your JavaScript project:

```sh
npm install /absolute/path/to/self-host-init/sdk/javascript
selfhost serve
```

Save as `example.mjs` (or use the same API in TypeScript):

```js
import { ApiError, Client } from "self-host-init-sdk";

const client = new Client(); // port=8787, timeoutMs=30000 per request
console.log(await client.health());
console.log((await client.state()).services);
const service = {
  id: "worker",
  image: "your-org/worker:1.0", // replace with your image
  limits: { cpus: 1, memory_mb: 256 },
};
console.log(await client.deploy(service)); // preview only
// After reviewing the plan:
// await client.deploy(service, { apply: true });
// console.log(await client.logs("worker", 100));
// await client.action("worker", "stop");

try {
  await client.action("missing-service", "start");
} catch (error) {
  if (error instanceof ApiError) console.error(error.status, error.message);
  else throw error;
}
```

Run with `node example.mjs`. Methods return promises of plain JSON objects; logs return a string. Mutations retrieve session tokens automatically and are never retried. After a timeout, inspect state before repeating an operation. Increase `timeoutMs` for image pulls. This package targets local Node.js automation; it is not a browser/CORS client.

[Complete operations, request fields, and connection behavior](https://github.com/djsydney04/self-host-init/blob/main/sdk/README.md).
