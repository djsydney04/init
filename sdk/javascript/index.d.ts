export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type JsonObject = { [key: string]: Json };
export interface Limits { cpus: number; memory_mb: number }
export interface Service {
  id: string;
  /** Required for container deployment; optional when a GitHub source is supplied. */
  image?: string;
  limits: Limits;
  name?: string;
  description?: string;
  project?: string | null;
  kind?: "daemon" | "job";
  command?: string[];
  working_dir?: string | null;
  workspace?: boolean;
  ports?: string[];
  env_file?: string | null;
  volumes?: { source: string; target: string; read_only?: boolean; no_copy?: boolean }[];
  interval_seconds?: number | null;
  allocation?: { weight?: number; min_cpus?: number; min_memory_mb?: number };
}
export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message: string);
}
/** Node.js 22+. Only connects to 127.0.0.1. Timeout applies per HTTP request. */
export class Client {
  constructor(options?: { port?: number; timeoutMs?: number });
  health(): Promise<JsonObject>;
  state(): Promise<JsonObject>;
  logs(id: string, tail?: number): Promise<string>;
  action(id: string, action: "start" | "stop" | "restart" | "run" | "apply", limits?: Limits): Promise<JsonObject>;
  /** Preview by default. Set apply to true to deploy. */
  deploy(service: Service, options?: { apply?: boolean; github?: { repository: string; reference?: string | null; context?: string } }): Promise<JsonObject>;
  createProject(project: { id: string; name: string; description?: string }): Promise<JsonObject>;
  setBudget(limits: Limits): Promise<JsonObject>;
  allocationPlan(): Promise<JsonObject>;
  /** Preview by default. Set apply to true to commit allocation changes. */
  configureAllocation(options?: { enabled?: boolean; budget?: Limits; apply?: boolean }): Promise<JsonObject>;
}
