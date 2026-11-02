import assert from "node:assert/strict";
import { ApiError, Client } from "init-sdk";

const client = new Client({ port: Number(process.env.INIT_TEST_PORT) });
assert.deepEqual(await client.health(), { app: "init", version: "0.5.0" });
assert.equal((await client.state()).services[0].id, "worker");
assert.equal(await client.logs("worker", 12), "hello\n");
assert.deepEqual(await client.allocationPlan(), { services: [] });
for (const action of ["start", "stop", "restart", "run"]) assert.equal((await client.action("worker", action)).ok, true);
const limits = { cpus: 1, memory_mb: 256 };
assert.equal((await client.action("worker", "apply", limits)).ok, true);
const service = { id: "worker", image: "example/worker:1", limits };
assert.equal((await client.deploy(service)).applied, false);
assert.equal((await client.deploy(service, { apply: true })).applied, true);
const github = { repository: "https://github.com/example/app", reference: "main", context: "services/web" };
assert.deepEqual((await client.deploy({ id: "worker", limits }, { github })).github, github);
assert.equal((await client.deploy({ id: "worker", limits }, { github, apply: true })).applied, true);
assert.equal((await client.createProject({ id: "dev", name: "Development" })).ok, true);
assert.equal((await client.setBudget(limits)).ok, true);
assert.equal((await client.configureAllocation({ enabled: true })).applied, false);
assert.equal((await client.configureAllocation({ enabled: true, apply: true })).applied, true);
for (const [id, status] of [["blocked", 409], ["expired", 403]]) {
  await assert.rejects(client.action(id, "start"), error => error instanceof ApiError && error.status === status);
}
assert.throws(() => new Client({ port: 0 }));
assert.throws(() => new Client({ timeoutMs: 0 }));
await assert.rejects(client.logs("worker", 0));
await assert.rejects(client.logs("../worker"));
assert.throws(() => client.action("worker", "destroy"));
assert.throws(() => client.action("worker", "start", limits));

await assert.rejects(new Client({ port: client.port, timeoutMs: 50 }).action("slow", "start"),
  error => error.name === "TimeoutError");
