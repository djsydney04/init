import { Client, type Service, type JsonObject } from "init-sdk";
const client = new Client({ port: 8787, timeoutMs: 30000 });
const service: Service = { id: "worker", image: "example/worker:1", limits: { cpus: 1, memory_mb: 256 } };
const preview: Promise<JsonObject> = client.deploy(service);
void preview;
void client.deploy({ id: "repo", limits: service.limits }, { github: { repository: "https://github.com/example/app", context: "services/web" } });
void client.action("worker", "apply", service.limits);
// @ts-expect-error unsupported service action
void client.action("worker", "destroy");
// @ts-expect-error memory limits are numeric
const invalid: Service = { ...service, limits: { cpus: 1, memory_mb: "256" } };
void invalid;
