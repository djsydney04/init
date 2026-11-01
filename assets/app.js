"use strict";

const $ = (id) => document.getElementById(id);
let state = null;
let sessionToken = null;
let currentFilter = "all";
let refreshing = false;
let logsService = null;
let logsRequest = 0;
let detailsServiceId = null;
let detailsLogRequest = 0;
let resourceService = null;
let resourceApplying = false;
let previewSignature = null;
let previewRequest = 0;
let toastTimer;
const pending = new Set();

function node(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined) el.textContent = String(text);
  return el;
}
function numeric(value) { return typeof value === "number" && Number.isFinite(value); }
function cores(value) { return numeric(value) ? value.toLocaleString(undefined, { maximumFractionDigits: 2 }) : "—"; }
function memory(value) {
  if (!numeric(value)) return "—";
  return value >= 1024 ? `${(value / 1024).toLocaleString(undefined, { maximumFractionDigits: 1 })} GiB` : `${Math.round(value)} MiB`;
}
function timestamp(value) {
  if (!value) return null;
  const date = new Date(typeof value === "number" ? value * 1000 : value);
  return Number.isNaN(date.getTime()) ? null : date;
}
function relativeTime(value) {
  const date = timestamp(value);
  if (!date) return "—";
  const seconds = Math.round((Date.now() - date.getTime()) / 1000);
  if (seconds < 0) return `in ${Math.max(1, Math.round(-seconds / 60))}m`;
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
async function api(path, options = {}) {
  const response = await fetch(path, { cache: "no-store", ...options });
  let data;
  try { data = await response.json(); }
  catch { throw new Error(`Server returned ${response.status}: ${response.statusText || "invalid response"}`); }
  if (!response.ok) {
    if (response.status === 403) sessionToken = null;
    throw new Error(data.error || `Request failed (${response.status})`);
  }
  return data;
}
async function session() { if (!sessionToken) sessionToken = (await api("/api/session")).token; }
async function post(path, body = {}) {
  await session();
  return api(path, { method: "POST", headers: { "Content-Type": "application/json", "X-Selfhost-Token": sessionToken }, body: JSON.stringify(body) });
}
function showError(message) { $("error-banner").textContent = message; $("error-banner").hidden = false; }
function resourceError(message) { $("resource-error").textContent = message; $("resource-error").hidden = !message; }
function toast(message) {
  clearTimeout(toastTimer);
  $("toast").textContent = message;
  $("toast").hidden = false;
  toastTimer = setTimeout(() => { $("toast").hidden = true; }, 4500);
}
function metric(id, value, unit) {
  $(id).replaceChildren(document.createTextNode(value));
  if (unit) $(id).append(node("small", "", ` ${unit}`));
}
function capacity() {
  const source = state.runtime?.available && (!resourceService || resourceService.runtime === "docker") ? state.runtime : state.capacity;
  return { cpus: numeric(source?.cpus) ? source.cpus : state.budget?.cpus, memory_mb: numeric(source?.memory_mb) ? source.memory_mb : state.budget?.memory_mb };
}
function renderSummary() {
  const services = state.services || [];
  const budget = state.budget || {};
  const runtime = state.runtime || {};
  const reserved = services.filter((service) => !service.observe_only && service.enabled !== false && (service.kind !== "job" || service.status === "running"));
  const running = services.filter((service) => service.status === "running").length;
  const allocatedCpu = reserved.reduce((sum, service) => sum + ((service.effective_limits || service.limits)?.cpus || 0), 0);
  const allocatedMemory = reserved.reduce((sum, service) => sum + ((service.effective_limits || service.limits)?.memory_mb || 0), 0);
  const workspace = typeof state.workspace === "string" ? state.workspace : state.workspace?.name || "Local machine";
  $("workspace-name").textContent = workspace;
  $("workspace-name").title = workspace;
  $("workspace-label").textContent = workspace.split(/[\\/]/).filter(Boolean).pop() || "Local machine";
  metric("service-count", String(services.length));
  $("service-detail").textContent = `${running} running · ${services.filter(service => service.observe_only).length} monitor-only`;
  metric("cpu-allocation", cores(allocatedCpu), `/ ${cores(budget.cpus)} cores`);
  metric("memory-allocation", memory(allocatedMemory), `/ ${memory(budget.memory_mb)}`);
  $("cpu-detail").textContent = `${Math.max(0, budget.cpus - allocatedCpu).toLocaleString(undefined, { maximumFractionDigits: 2 })} cores unreserved`;
  $("memory-detail").textContent = `${memory(Math.max(0, budget.memory_mb - allocatedMemory))} unreserved`;
  $("runtime-state").textContent = runtime.available ? "Connected" : "Unavailable";
  $("runtime-state").style.color = runtime.available ? "var(--success)" : "var(--amber)";
  $("runtime-detail").textContent = runtime.available ? `${cores(runtime.cpus)} cores · ${memory(runtime.memory_mb)} capacity` : "Container actions need a running engine";
  $("runtime-banner").hidden = !runtime.error;
  $("runtime-banner").textContent = runtime.error ? `Container engine: ${runtime.error}` : "";
  $("budget-summary").textContent = `${cores(budget.cpus)} cores · ${memory(budget.memory_mb)}`;
  $("allocation-summary").textContent = state.allocation?.enabled ? "Automatic allocation · weighted sharing within service limits" : "Manual allocation · each service uses its configured limits";
}
function actionButton(service, action, label) {
  const button = node("button", `button secondary${action === "stop" ? " danger" : ""}`, label);
  button.type = "button";
  button.dataset.service = service.id;
  button.dataset.action = action;
  const canManage = !service.observe_only && (service.enabled !== false || action === "stop") && service.capabilities?.lifecycle !== false;
  button.disabled = pending.has(service.id) || (action !== "logs" && !canManage);
  button.title = action === "logs" ? `View logs for ${service.name}` : service.observe_only ? "Managed by the host supervisor" : service.enabled === false ? "Enable this service in the manifest first" : service.capabilities?.lifecycle === false ? "This integration does not support this action" : `${label} ${service.name}`;
  button.setAttribute("aria-label", `${label} ${service.name}`);
  return button;
}
function renderServices() {
  if (!state) return;
  const services = state.services || [];
  const query = $("search").value.trim().toLowerCase();
  const shown = services.filter((service) => {
    const filterMatches = currentFilter === "all" || (currentFilter === "job" ? ["job", "scheduled", "cron"].includes(service.kind) : currentFilter === "stopped" ? ["stopped", "exited", "created", "missing", "disabled", "not created", "inactive"].includes(service.status) : service.status === currentFilter);
    return filterMatches && `${service.name} ${service.id} ${service.description || ""}`.toLowerCase().includes(query);
  });
  const fragment = document.createDocumentFragment();
  for (const service of shown) {
    const row = node("tr");
    const identity = node("td");
    const cell = node("div", "service-cell");
    const name = node("button", "service-name", service.name || service.id);
    name.type = "button";
    name.dataset.service = service.id;
    name.dataset.action = "details";
    name.setAttribute("aria-label", `View details for ${service.name || service.id}`);
    name.title = `${service.name || service.id}${service.description ? ` — ${service.description}` : ""}`;
    const meta = node("div", "service-meta", `${service.runtime || "container"} · ${service.observe_only ? "monitor only" : service.enabled === false ? "disabled" : service.kind || "service"}`);
    meta.title = meta.textContent;
    cell.append(name, meta);
    identity.append(cell);
    const statusCell = node("td");
    const status = service.status || "unknown";
    statusCell.append(node("span", `status-badge ${["running", "healthy", "failed", "unhealthy", "error", "starting", "queued", "pending"].includes(status) ? status : ""}`, status));
    const detail = service.health && !["none", "unknown", "no healthcheck"].includes(service.health) ? service.health : service.paused ? "Paused by operator" : service.next_run ? `Next ${relativeTime(service.next_run)}` : service.last_run ? `Ran ${relativeTime(service.last_run)}` : service.observe_only ? "Host supervised" : "";
    if (detail) { const el = node("span", "cell-sub", detail); el.title = detail; statusCell.append(el); }
    const cpu = node("td", "cell-value", numeric(service.cpu_percent) ? `${service.cpu_percent.toFixed(1)}%` : "—");
    const ram = node("td", "cell-value", memory(service.memory_mb));
    const limitsCell = node("td");
    const limits = node("button", "limit-button");
    const effective = service.effective_limits || service.limits;
    limits.type = "button";
    limits.dataset.service = service.id;
    limits.dataset.action = "resources";
    limits.disabled = service.observe_only || service.enabled === false || service.capabilities?.scale === false || pending.has(service.id);
    limits.title = limits.disabled ? service.observe_only ? "Monitor-only service; limits are managed by the host supervisor" : service.enabled === false ? "Enable this service in the manifest first" : "This runtime does not support resource scaling" : `Edit ${service.name} limits${state.allocation?.enabled ? `; maximum ${cores(service.limits?.cpus)} cores and ${memory(service.limits?.memory_mb)}` : ""}`;
    limits.setAttribute("aria-label", limits.title);
    limits.append(node("span", "", `${cores(effective?.cpus)} CPU`), node("span", "", memory(effective?.memory_mb)));
    limitsCell.append(limits);
    const actionCell = node("td");
    const actions = node("div", "actions");
    const isJob = ["job", "scheduled", "cron"].includes(service.kind);
    if (service.status === "running") actions.append(actionButton(service, "restart", "Restart"), actionButton(service, "stop", "Stop"));
    else actions.append(actionButton(service, isJob ? "run" : "start", isJob ? "Run now" : "Start"));
    actions.append(actionButton(service, "logs", "Logs"));
    actionCell.append(actions);
    row.append(identity, statusCell, cpu, ram, limitsCell, actionCell);
    fragment.append(row);
  }
  $("service-rows").replaceChildren(fragment);
  $("table-count").textContent = services.length;
  $("shown-count").textContent = `${shown.length} of ${services.length} services`;
  $("empty-state").hidden = shown.length > 0;
  $("empty-state").querySelector("h2").textContent = services.length ? "No matching services" : "No services";
  $("empty-state").querySelector("p").textContent = services.length ? "Try another search or filter." : "Add a service to your workspace manifest, then refresh.";
}
function eventList(events) {
  const fragment = document.createDocumentFragment();
  for (const event of events) {
    const item = node("li", `event-item${event.ok === false || event.level === "error" ? " error" : ""}`);
    const time = event.timestamp || event.time || event.at;
    const body = node("div", "event-body");
    const service = state.services.find(service => service.id === event.service);
    body.append(node("span", "event-context", service?.name || event.service || "Workspace"), node("span", "event-message", event.message || event.action || "Updated"));
    const timeEl = node("time", "event-time", relativeTime(time));
    const date = timestamp(time);
    if (date) { timeEl.dateTime = date.toISOString(); timeEl.title = date.toLocaleString(); }
    item.append(body, timeEl);
    fragment.append(item);
  }
  return fragment;
}
function renderEvents() {
  const filter = $("activity-filter");
  const signature = state.services.map(service => `${service.id}:${service.name}`).join("|");
  if (filter.dataset.services !== signature) {
    const previous = filter.value;
    const options = [new Option("All services", ""), ...state.services.map(service => new Option(service.name || service.id, service.id))];
    filter.replaceChildren(...options);
    filter.value = previous;
    if (filter.selectedIndex < 0) filter.value = "";
    filter.dataset.services = signature;
  }
  const events = (state.events || []).filter(event => !filter.value || event.service === filter.value);
  $("event-list").replaceChildren(eventList(events));
  $("no-events").hidden = events.length > 0;
  $("no-events").textContent = filter.value ? "No recent activity for this service." : "No activity recorded yet.";
}
function resourcePair(limits) {
  const cell = node("td", "cell-value", `${cores(limits?.cpus)} cores`);
  cell.append(node("span", "cell-sub", memory(limits?.memory_mb)));
  return cell;
}
function renderResources() {
  const host = state.capacity || {};
  const runtime = state.runtime || {};
  const budget = state.budget || {};
  $("host-capacity").textContent = `${cores(host.cpus)} cores · ${memory(host.memory_mb)}`;
  $("engine-capacity").textContent = runtime.available ? `${cores(runtime.cpus)} cores · ${memory(runtime.memory_mb)}` : "Engine unavailable";
  $("engine-capacity-note").textContent = runtime.available ? "Available to containers; may differ from host capacity." : "Start the container engine to manage Docker services.";
  const services = state.services.filter(service => !service.observe_only && service.enabled !== false);
  const reserved = services.filter(service => service.kind !== "job" || service.status === "running");
  const cpu = reserved.reduce((sum, service) => sum + ((service.effective_limits || service.limits)?.cpus || 0), 0);
  const ram = reserved.reduce((sum, service) => sum + ((service.effective_limits || service.limits)?.memory_mb || 0), 0);
  $("cpu-pool").max = budget.cpus || 1; $("cpu-pool").value = cpu;
  $("memory-pool").max = budget.memory_mb || 1; $("memory-pool").value = ram;
  $("cpu-pool-label").textContent = `${cores(Math.max(0, budget.cpus - cpu))} cores free in budget`;
  $("memory-pool-label").textContent = `${memory(Math.max(0, budget.memory_mb - ram))} free in budget`;
  const observers = state.services.filter(service => service.observe_only);
  const observedRam = observers.reduce((sum, service) => sum + (numeric(service.memory_mb) ? service.memory_mb : 0), 0);
  $("outside-summary").textContent = `${observers.length} monitor-only ${observers.length === 1 ? "service runs" : "services run"} outside this budget${observedRam ? ` (${memory(observedRam)} observed memory)` : ""}. Other host processes also use machine capacity. ${budget.max_parallel_jobs ?? 1} ${budget.max_parallel_jobs === 1 ? "job" : "jobs"} can run in parallel; idle jobs reserve capacity when they run.`;
  const fragment = document.createDocumentFragment();
  for (const service of services) {
    const row = node("tr");
    const identity = node("td");
    const name = node("button", "service-link", service.name || service.id);
    name.type = "button"; name.dataset.service = service.id; name.dataset.action = "details";
    name.title = service.name || service.id;
    identity.append(name, node("span", "cell-sub", service.kind || "service"));
    row.append(identity, resourcePair({ cpus: service.allocation?.min_cpus, memory_mb: service.allocation?.min_memory_mb }), resourcePair(service.limits), resourcePair(service.effective_limits || service.limits), node("td", "cell-value", service.allocation?.weight ?? 1));
    fragment.append(row);
  }
  $("resource-allocation-rows").replaceChildren(fragment);
  $("no-allocations").hidden = services.length > 0;
  $("allocation-table-note").textContent = `${services.length} enabled managed · ${state.allocation?.enabled ? "automatic sharing" : "manual limits"}`;
}
function detailField(grid, label, value, wide = false) {
  const item = node("div", `detail-item${wide ? " wide" : ""}`);
  item.append(node("dt", "", label), node("dd", "", value ?? "—"));
  grid.append(item);
}
function fullTime(value) { const date = timestamp(value); return date ? date.toLocaleString() : "—"; }
function selectedService() { return state?.services.find(service => service.id === detailsServiceId); }
function renderDetails() {
  const service = selectedService();
  if (!service || !$("details-dialog").open) return;
  $("details-title").textContent = service.name || service.id;
  $("details-subtitle").textContent = service.id;
  const overview = node("dl", "details-grid");
  detailField(overview, "State", service.status || "Unknown");
  detailField(overview, "Runtime", service.runtime);
  detailField(overview, "Control", service.observe_only ? "Monitor only" : service.enabled === false ? "Disabled in manifest" : "Managed");
  detailField(overview, "Type", service.kind || "daemon");
  detailField(overview, "CPU usage", numeric(service.cpu_percent) ? `${service.cpu_percent.toFixed(1)}%` : "Not reported");
  detailField(overview, "Memory usage", numeric(service.memory_mb) ? memory(service.memory_mb) : "Not reported");
  detailField(overview, "Health", service.health && !["none", "unknown", "no healthcheck"].includes(service.health) ? service.health : "No health check reported");
  detailField(overview, "Enabled", service.enabled === false ? "No" : "Yes");
  detailField(overview, "Scheduling", service.paused ? "Paused by operator" : service.kind === "job" ? "Interval schedule" : "Continuous service");
  if (service.kind === "job") {
    detailField(overview, "Last run", fullTime(service.last_run));
    detailField(overview, "Next run", fullTime(service.next_run));
    detailField(overview, "Last exit code", service.last_exit ?? "No completed run");
  }
  if (service.description) detailField(overview, "Description", service.description, true);
  const actions = node("div", "detail-actions");
  if (service.status === "running") actions.append(actionButton(service, "restart", "Restart"), actionButton(service, "stop", "Stop"));
  else actions.append(actionButton(service, service.kind === "job" ? "run" : "start", service.kind === "job" ? "Run now" : "Start"));
  $("detail-overview").replaceChildren(overview, actions);
  const resources = node("div");
  resources.append(node("p", "detail-resource-note", service.observe_only ? "This service is monitored through its host supervisor. Workspace allocation does not control its resource limits." : state.allocation?.enabled ? "Automatic allocation shares the workspace budget between service minimums and ceilings. Runtime limits show the latest reported enforcement." : "Manual mode uses the declared ceiling as the effective limit. Runtime limits show the latest reported enforcement."));
  const scroll = node("div", "table-scroll"); scroll.tabIndex = 0; scroll.setAttribute("aria-label", "Service resource limits");
  const table = node("table", "detail-table");
  const head = node("thead"); const headings = node("tr");
  for (const label of ["Resource", "Minimum", "Ceiling", "Effective", "Runtime"]) { const th = node("th", "", label); th.scope = "col"; headings.append(th); }
  head.append(headings); const body = node("tbody");
  for (const key of ["cpus", "memory_mb"]) {
    const format = value => key === "cpus" ? `${cores(value)} cores` : memory(value);
    const minimum = service.observe_only ? null : service.allocation?.[key === "cpus" ? "min_cpus" : "min_memory_mb"];
    const actual = service.actual_limits?.[key];
    const row = node("tr");
    row.append(node("td", "", key === "cpus" ? "CPU" : "Memory"), node("td", "", numeric(minimum) ? format(minimum) : "—"), node("td", "", numeric(service.limits?.[key]) ? format(service.limits[key]) : "—"), node("td", "", numeric((service.effective_limits || service.limits)?.[key]) ? format((service.effective_limits || service.limits)[key]) : "—"), node("td", "", actual === 0 ? "Unlimited" : numeric(actual) ? format(actual) : "Not reported"));
    body.append(row);
  }
  table.append(head, body); scroll.append(table); resources.append(scroll);
  if (!service.observe_only) resources.append(node("p", "detail-weight", `Allocation weight: ${service.allocation?.weight ?? 1}. Change weights and minimums in the workspace manifest.`));
  const edit = node("button", "button secondary", "Edit limits");
  edit.type = "button"; edit.dataset.service = service.id; edit.dataset.action = "resources";
  edit.disabled = service.observe_only || service.enabled === false || service.capabilities?.scale === false || pending.has(service.id);
  resources.append(edit); $("detail-resources").replaceChildren(resources);
  const events = (state.events || []).filter(event => event.service === service.id);
  if (events.length) { const list = node("ol", "event-list"); list.append(eventList(events)); $("detail-activity").replaceChildren(list); }
  else $("detail-activity").replaceChildren(node("p", "quiet-message", "No recent activity recorded for this service."));
}
async function loadDetailLogs() {
  const id = detailsServiceId;
  if (!id) return;
  const request = ++detailsLogRequest;
  $("detail-refresh-logs").disabled = true;
  $("detail-logs-output").textContent = "Loading…";
  try {
    const result = await api(`/api/services/${encodeURIComponent(id)}/logs?tail=200`);
    if (request === detailsLogRequest && $("details-dialog").open && detailsServiceId === id) $("detail-logs-output").textContent = result.logs || "No output recorded for this service.";
  } catch (error) { if (request === detailsLogRequest) $("detail-logs-output").textContent = `Unable to load logs: ${error.message}`; }
  finally { if (request === detailsLogRequest) $("detail-refresh-logs").disabled = false; }
}
function selectDetailTab(name, focus = false) {
  document.querySelectorAll("[data-detail-tab]").forEach(button => {
    const active = button.dataset.detailTab === name;
    button.setAttribute("aria-selected", String(active)); button.tabIndex = active ? 0 : -1;
    $(`detail-${button.dataset.detailTab}`).hidden = !active;
    if (active && focus) button.focus();
  });
  if (name === "logs") loadDetailLogs();
}
function openDetails(service) {
  detailsServiceId = service.id;
  $("details-dialog").showModal();
  selectDetailTab("overview");
  renderDetails();
}
async function refresh() {
  if (refreshing) return;
  refreshing = true;
  $("refresh").disabled = true;
  try {
    await session();
    state = await api("/api/state");
    renderSummary(); renderServices(); renderResources(); renderEvents(); renderDetails();
    $("error-banner").hidden = true;
    if (state.manifest_error) showError(`Manifest: ${state.manifest_error}. Showing the last valid configuration.`);
    $("refresh-status").textContent = "Updates every 5 seconds";
    $("updated-at").textContent = `Updated ${new Date().toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`;
    $("budget-edit").disabled = false;
    $("connection-label").textContent = "Controller connected";
  } catch (error) {
    showError(error.message);
    $("refresh-status").textContent = "Connection interrupted";
    $("connection-label").textContent = "Connection interrupted";
  } finally { refreshing = false; $("refresh").disabled = false; }
}
async function loadLogs() {
  if (!logsService) return;
  const service = logsService;
  const request = ++logsRequest;
  $("refresh-logs").disabled = true;
  try {
    const result = await api(`/api/services/${encodeURIComponent(service.id)}/logs?tail=200`);
    if (request !== logsRequest || !$("logs-dialog").open) return;
    $("logs-output").textContent = result.logs || "No output recorded for this service.";
  } catch (error) { if (request === logsRequest && $("logs-dialog").open) $("logs-output").textContent = `Unable to load logs: ${error.message}`; }
  finally { if (request === logsRequest) $("refresh-logs").disabled = false; }
}
function limitsInput() { return { cpus: Number($("resource-cpu").value), memory_mb: Number($("resource-memory").value) }; }
function automaticInput() { return document.querySelector('input[name="allocation-mode"]:checked').value === "automatic"; }
function inputSignature() { return JSON.stringify({ budget: limitsInput(), enabled: automaticInput() }); }
function validateResources() {
  const limits = limitsInput();
  const cpu = $("resource-cpu");
  const ram = $("resource-memory");
  if (!numeric(limits.cpus) || limits.cpus < Number(cpu.min) || limits.cpus > Number(cpu.max)) return `CPU must be between ${cores(Number(cpu.min))} and ${cores(Number(cpu.max))} cores.`;
  if (!Number.isInteger(limits.memory_mb) || limits.memory_mb < Number(ram.min) || limits.memory_mb > Number(ram.max)) return `Memory must be a whole number between ${ram.min} and ${ram.max} MiB.`;
  return "";
}
function updateResourceWarning() {
  const limits = limitsInput();
  const available = capacity();
  const warnings = [];
  if (limits.cpus > available.cpus * 0.9 || limits.memory_mb > available.memory_mb * 0.9) warnings.push("This leaves less than 10% of the available capacity for the operating system and other work.");
  if (resourceService && numeric(resourceService.memory_mb) && limits.memory_mb < resourceService.memory_mb) warnings.push("The memory limit is below current usage. Applying it may stop the service.");
  $("resource-warning").textContent = warnings.join(" ");
  $("resource-warning").hidden = !warnings.length;
}
function invalidatePreview() {
  previewSignature = null;
  previewRequest++;
  $("allocation-preview").hidden = true;
  $("preview-note").textContent = "Values changed. Preview the allocation to see the proposed limits.";
  $("apply-resources").disabled = !resourceService && automaticInput();
  resourceError("");
  updateResourceWarning();
}
function configureSlider(key, value, min, max, format) {
  const number = $(`resource-${key}`);
  const range = $(`resource-${key}-range`);
  number.min = range.min = min;
  number.max = range.max = max;
  number.value = value;
  range.value = value;
  range.setAttribute("aria-valuetext", format(Number(value)));
  $(`${key === "cpu" ? "cpu" : "memory"}-bounds`).replaceChildren(node("span", "", format(min)), node("span", "", format(max)));
}
async function previewAllocation() {
  if (resourceApplying || resourceService) return;
  const validation = validateResources();
  if (validation) { resourceError(validation); return; }
  const signature = inputSignature();
  const request = ++previewRequest;
  $("preview-allocation").disabled = true;
  $("preview-note").textContent = "Calculating…";
  resourceError("");
  try {
    const plan = await post("/api/allocation", { enabled: automaticInput(), apply: false, budget: limitsInput() });
    if (request !== previewRequest || !$("resources-dialog").open) return;
    const fragment = document.createDocumentFragment();
    for (const allocation of plan.allocations || []) {
      const service = state.services.find((item) => item.id === allocation.id);
      const row = node("tr");
      const name = node("td", "", service?.name || allocation.id);
      name.title = name.textContent;
      name.append(node("span", "cell-sub", `Min ${cores(allocation.min_cpus)} CPU · ${memory(allocation.min_memory_mb)}`));
      row.append(name, node("td", "", cores(allocation.cpus)), node("td", "", memory(allocation.memory_mb)), node("td", "", allocation.weight));
      fragment.append(row);
    }
    $("allocation-rows").replaceChildren(fragment);
    $("allocation-preview").hidden = false;
    $("preview-note").textContent = (plan.warnings || []).length ? plan.warnings.join(" ") : plan.allocations?.length ? "Proposed limits. The Rust allocator checks minimums, weights, ceilings, and current memory usage." : "No managed services are eligible for allocation.";
    previewSignature = signature;
    $("apply-resources").disabled = false;
  } catch (error) {
    if (request !== previewRequest) return;
    resourceError(error.message);
    $("preview-note").textContent = "The allocation could not be calculated. Adjust the budget or service limits.";
    $("apply-resources").disabled = automaticInput();
  } finally { $("preview-allocation").disabled = false; }
}
function openResources(service) {
  if (resourceApplying) return;
  resourceService = service;
  const limits = service ? service.limits : state.budget;
  const available = capacity();
  const cpuMax = Math.max(0.01, available.cpus || limits.cpus);
  const memoryMax = Math.max(6, Math.floor(available.memory_mb || limits.memory_mb));
  $("resources-title").textContent = service ? service.name : "Resource budget";
  $("resource-note").textContent = service ? state.allocation?.enabled ? "Set this service’s maximum limits. Automatic allocation shares the budget below these ceilings." : "Set this service’s CPU and memory limits. The workspace budget must cover the allocation." : "Set the total CPU and memory available to managed services. Monitor-only services and other host processes are outside this budget.";
  const engineCapacity = state.runtime?.available && (!service || service.runtime === "docker");
  $("resource-capacity").textContent = `${engineCapacity ? "Container engine" : "Host"} capacity: ${cores(available.cpus)} CPU cores · ${memory(available.memory_mb)}. Limits are enforced where the service runtime supports them.`;
  const cpuMin = service && state.allocation?.enabled ? Math.max(0.01, service.allocation?.min_cpus ?? 0.1) : 0.01;
  const memoryMin = service && state.allocation?.enabled ? Math.max(6, service.allocation?.min_memory_mb ?? 64) : 6;
  configureSlider("cpu", limits.cpus, cpuMin, cpuMax, (value) => `${cores(value)} cores`);
  configureSlider("memory", limits.memory_mb, memoryMin, memoryMax, memory);
  $("allocation-controls").hidden = Boolean(service);
  document.querySelector(`input[name="allocation-mode"][value="${state.allocation?.enabled ? "automatic" : "manual"}"]`).checked = true;
  $("service-policy").hidden = !service || !state.allocation?.enabled;
  if (service && state.allocation?.enabled) $("service-policy").textContent = `Minimum: ${cores(service.allocation?.min_cpus ?? 0.1)} CPU cores · ${memory(service.allocation?.min_memory_mb ?? 64)}. Weight: ${service.allocation?.weight ?? 1}. Edit minimums and weights in the workspace manifest.`;
  $("apply-resources").textContent = service ? "Apply limits" : "Apply budget";
  invalidatePreview();
  $("resources-dialog").showModal();
  if (!service) previewAllocation();
}
document.addEventListener("click", async (event) => {
  const button = event.target.closest("button[data-service]");
  if (!button || button.disabled) return;
  const service = state.services.find((item) => item.id === button.dataset.service);
  if (!service) return;
  const action = button.dataset.action;
  if (action === "details") { openDetails(service); return; }
  if (action === "resources") { if ($("details-dialog").open) $("details-dialog").close(); openResources(service); return; }
  if (action === "logs") {
    logsService = service;
    $("logs-title").textContent = `${service.name} logs`;
    $("logs-output").textContent = "Loading…";
    $("logs-dialog").showModal();
    await loadLogs();
    return;
  }
  pending.add(service.id); renderServices(); renderDetails();
  try {
    await post(`/api/services/${encodeURIComponent(service.id)}/${action}`);
    toast(`${service.name}: ${action} completed`);
    await refresh();
  } catch (error) { showError(error.message); }
  finally { pending.delete(service.id); renderServices(); renderDetails(); }
});
$("resource-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (resourceApplying) return;
  const validation = validateResources();
  if (validation) { resourceError(validation); return; }
  if (!resourceService && automaticInput() && previewSignature !== inputSignature()) { resourceError("Preview this allocation before applying it."); return; }
  const limits = limitsInput();
  const service = resourceService;
  const enabled = automaticInput();
  resourceApplying = true;
  $("apply-resources").disabled = true;
  $("apply-resources").textContent = "Applying…";
  $("resources-dialog").querySelectorAll("input, .close-dialog, #preview-allocation").forEach((el) => { el.disabled = true; });
  resourceError("");
  try {
    if (service) await post(`/api/services/${encodeURIComponent(service.id)}/apply`, { limits });
    else await post("/api/allocation", { enabled, apply: true, budget: limits });
    $("resources-dialog").close();
    toast(service ? `${service.name}: resource limits applied` : "Resource budget and allocation updated");
    await refresh();
  } catch (error) { resourceError(error.message); }
  finally {
    resourceApplying = false;
    $("resources-dialog").querySelectorAll("input, .close-dialog, #preview-allocation").forEach((el) => { el.disabled = false; });
    $("apply-resources").disabled = false;
    $("apply-resources").textContent = service ? "Apply limits" : "Apply budget";
  }
});
for (const key of ["cpu", "memory"]) {
  const number = $(`resource-${key}`);
  const range = $(`resource-${key}-range`);
  number.addEventListener("input", () => { range.value = number.value; range.setAttribute("aria-valuetext", key === "cpu" ? `${cores(Number(number.value))} cores` : memory(Number(number.value))); invalidatePreview(); });
  range.addEventListener("input", () => { number.value = range.value; range.setAttribute("aria-valuetext", key === "cpu" ? `${cores(Number(range.value))} cores` : memory(Number(range.value))); invalidatePreview(); });
}
document.querySelectorAll('input[name="allocation-mode"]').forEach((input) => input.addEventListener("change", invalidatePreview));
$("preview-allocation").addEventListener("click", previewAllocation);
document.querySelectorAll(".close-dialog").forEach((button) => button.addEventListener("click", () => { if (!resourceApplying) button.closest("dialog").close(); }));
document.querySelectorAll("dialog").forEach((dialog) => {
  dialog.addEventListener("cancel", (event) => { if (resourceApplying) event.preventDefault(); });
  dialog.addEventListener("click", (event) => { if (event.target === dialog && !resourceApplying) { const rect = dialog.getBoundingClientRect(); if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) dialog.close(); } });
  dialog.addEventListener("close", () => { previewRequest++; if (dialog.id === "logs-dialog") logsRequest++; if (dialog.id === "details-dialog") { detailsLogRequest++; detailsServiceId = null; } });
});
document.querySelectorAll("[data-filter]").forEach((button) => button.addEventListener("click", () => {
  currentFilter = button.dataset.filter;
  document.querySelectorAll("[data-filter]").forEach((item) => { item.classList.toggle("active", item === button); item.setAttribute("aria-pressed", String(item === button)); });
  renderServices();
}));
document.querySelectorAll(".nav-item").forEach((link) => link.addEventListener("click", () => {
  document.querySelectorAll(".nav-item").forEach((item) => { const active = item === link; item.classList.toggle("active", active); if (active) item.setAttribute("aria-current", "location"); else item.removeAttribute("aria-current"); });
}));
$("search").addEventListener("input", renderServices);
$("activity-filter").addEventListener("change", renderEvents);
$("detail-refresh-logs").addEventListener("click", loadDetailLogs);
document.querySelectorAll("[data-detail-tab]").forEach(button => {
  button.addEventListener("click", () => selectDetailTab(button.dataset.detailTab));
  button.addEventListener("keydown", event => {
    const names = ["overview", "resources", "logs", "activity"];
    const index = names.indexOf(button.dataset.detailTab);
    const next = event.key === "ArrowRight" ? (index + 1) % names.length : event.key === "ArrowLeft" ? (index + names.length - 1) % names.length : event.key === "Home" ? 0 : event.key === "End" ? names.length - 1 : -1;
    if (next >= 0) { event.preventDefault(); selectDetailTab(names[next], true); }
  });
});
const sectionObserver = new IntersectionObserver(entries => {
  const visible = entries.filter(entry => entry.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
  if (!visible.length) return;
  document.querySelectorAll(".nav-item").forEach(link => { const active = link.hash === `#${visible[0].target.id}`; link.classList.toggle("active", active); if (active) link.setAttribute("aria-current", "location"); else link.removeAttribute("aria-current"); });
}, { rootMargin: "-5% 0px -65% 0px", threshold: 0 });
for (const id of ["services", "resources", "activity"]) sectionObserver.observe($(id));
$("refresh").addEventListener("click", refresh);
$("refresh-logs").addEventListener("click", loadLogs);
$("budget-edit").addEventListener("click", () => { if (state) openResources(null); });
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  $("theme-toggle").textContent = theme === "dark" ? "Light theme" : "Dark theme";
}
let savedTheme;
try { savedTheme = localStorage.getItem("selfhost-theme-v2"); } catch { /* The current session can still change theme. */ }
applyTheme(savedTheme === "light" ? "light" : "dark");
$("theme-toggle").addEventListener("click", () => {
  const theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  applyTheme(theme);
  try { localStorage.setItem("selfhost-theme-v2", theme); } catch { /* The current session can still change theme. */ }
});
refresh();
setInterval(() => { if (!document.hidden) refresh(); }, 5000);
document.addEventListener("visibilitychange", () => { if (!document.hidden) refresh(); });
