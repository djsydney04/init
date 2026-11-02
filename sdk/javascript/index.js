/** A daemon HTTP error; network errors and timeouts retain their native types. */
export class ApiError extends Error {
  constructor(status, message) {
    super(`HTTP ${status}: ${message}`);
    this.name = "ApiError";
    this.status = status;
  }
}

/** Local Node.js client. Mutations are never retried, including after a timeout. */
export class Client {
  constructor({ port = 8787, timeoutMs = 30000 } = {}) {
    if (!Number.isInteger(port) || port < 1 || port > 65535) throw new RangeError("Invalid port");
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0) throw new RangeError("Invalid timeoutMs");
    this.port = port;
    this.timeoutMs = timeoutMs;
  }

  async #request(method, path, body) {
    const headers = { Accept: "application/json" };
    let payload;
    if (method === "POST") {
      payload = JSON.stringify(body);
      headers["Content-Type"] = "application/json";
      headers["X-Init-Token"] = (await this.#request("GET", "/api/session")).token;
    }
    const response = await fetch(`http://127.0.0.1:${this.port}${path}`, {
      method, headers, body: payload, redirect: "error",
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    let data;
    try { data = await response.json(); }
    catch { throw new ApiError(response.status, "Daemon returned invalid JSON"); }
    if (!response.ok) throw new ApiError(response.status, data?.error || response.statusText);
    if (data === null || Array.isArray(data) || typeof data !== "object") {
      throw new ApiError(response.status, "Expected a JSON object");
    }
    return data;
  }

  #servicePath(id) {
    if (typeof id !== "string" || !/^[a-z][a-z0-9-]{0,47}$/.test(id)) throw new TypeError("Invalid service ID");
    return `/api/services/${id}`;
  }

  health() { return this.#request("GET", "/api/health"); }
  state() { return this.#request("GET", "/api/state"); }
  async logs(id, tail = 200) {
    if (!Number.isInteger(tail) || tail < 1 || tail > 2000) throw new RangeError("tail must be between 1 and 2000");
    return (await this.#request("GET", `${this.#servicePath(id)}/logs?tail=${tail}`)).logs;
  }
  action(id, action, limits) {
    if (!["start", "stop", "restart", "run", "apply"].includes(action)) throw new TypeError("Unknown service action");
    if (limits !== undefined && action !== "apply") throw new TypeError("limits are only supported by apply");
    return this.#request("POST", `${this.#servicePath(id)}/${action}`, limits === undefined ? {} : { limits });
  }
  deploy(service, { apply = false, github } = {}) { return this.#request("POST", "/api/services", { service, apply, ...(github ? { github } : {}) }); }
  createProject(project) { return this.#request("POST", "/api/projects", project); }
  setBudget(limits) { return this.#request("POST", "/api/budget", limits); }
  allocationPlan() { return this.#request("GET", "/api/allocation"); }
  configureAllocation({ enabled, budget, apply = false } = {}) {
    return this.#request("POST", "/api/allocation", { enabled, budget, apply });
  }
}
