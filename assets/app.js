"use strict";

const $ = (id) => document.getElementById(id);
let state = null;
let sessionToken = null;
let currentFilter = "all";
let refreshing = false;
let logsService = null;
let resourceService = null;
let resourceApplying = false;
let toastTimer;
const pending = new Set();

function node(tag, className, text) {
  const el = document.createElement(tag);
  if (className) el.className = className;
  if (text !== undefined) el.textContent = String(text);
  return el;
}

function numeric(value) {
  return typeof value === "number" && Number.isFinite(value);
}

function cores(value) {
  return numeric(value) ? value.toLocaleString(undefined, { maximumFractionDigits: 2 }) : "—";
}

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

async function session() {
  if (!sessionToken) sessionToken = (await api("/api/session")).token;
}

async function post(path, body = {}) {
  await session();
  return api(path, { method: "POST", headers: { "Content-Type": "application/json", "X-Selfhost-Token": sessionToken }, body: JSON.stringify(body) });
}

function showError(message) {
  $("error-banner").textContent = message;
  $("error-banner").hidden = false;
}

function toast(message) {
  clearTimeout(toastTimer);
  $("toast").textContent = message;
  $("toast").hidden = false;
  toastTimer = setTimeout(() => { $("toast").hidden = true; }, 4500);
}

function metric(id, value, unit) {
  const el = $(id);
  el.replaceChildren(document.createTextNode(value));
  if (unit) el.append(node("small", "", ` ${unit}`));
}

function allocationBar(id, allocated, total) {
  const bar = $(id);
  const percent = total > 0 ? allocated / total * 100 : 0;
  bar.style.width = `${Math.min(100, Math.max(0, percent))}%`;
  bar.parentElement.classList.toggle("over", percent > 100);
}

function renderSummary() {
  const services = state.services || [];
  const budget = state.budget || {};
  const runtime = state.runtime || {};
  const managed = services.filter((service) => !service.observe_only && service.enabled !== false);
  const running = services.filter((service) => service.status === "running").length;
  const reserved = managed.filter((service) => service.kind !== "job" || service.status === "running");
  const allocatedCpu = reserved.reduce((sum, service) => sum + (service.limits?.cpus || 0), 0);
  const allocatedMemory = reserved.reduce((sum, service) => sum + (service.limits?.memory_mb || 0), 0);
  $("workspace-name").textContent = typeof state.workspace === "string" ? state.workspace : state.workspace?.name || "Local machine";
  $("nav-count").textContent = services.length;
  metric("service-count", services.length);
  $("service-detail").replaceChildren(node("span", "running-count", `${running} running`), document.createTextNode(` · ${services.length - running} other`));
  metric("cpu-allocation", cores(allocatedCpu), `/ ${cores(budget.cpus)} cores`);
  metric("memory-allocation", memory(allocatedMemory), `/ ${memory(budget.memory_mb)}`);
  allocationBar("cpu-bar", allocatedCpu, budget.cpus);
  allocationBar("memory-bar", allocatedMemory, budget.memory_mb);
  $("cpu-detail").textContent = `${reserved.length} reservations · ${budget.max_parallel_jobs ?? "—"} parallel jobs`;
  $("memory-detail").textContent = "Daemon reservations + running job limits";
  $("runtime-state").textContent = runtime.available ? "Connected" : "Unavailable";
  $("runtime-state").style.color = runtime.available ? "var(--accent)" : "var(--amber)";
  $("runtime-detail").textContent = runtime.available ? `${cores(runtime.cpus)} cores · ${memory(runtime.memory_mb)} available` : "Check the container engine connection";
  $("runtime-banner").hidden = !runtime.error;
  $("runtime-banner").textContent = runtime.error ? `Container runtime: ${runtime.error}` : "";
}

function actionButton(service, action, label, className = "button secondary") {
  const button = node("button", className, label);
  button.type = "button";
  button.dataset.service = service.id;
  button.dataset.action = action;
  const canManage = !service.observe_only && (service.enabled !== false || action === "stop") && service.capabilities?.lifecycle !== false;
  button.disabled = pending.has(service.id) || (action !== "logs" && !canManage);
  const actionName = action === "logs" ? "View logs" : action === "stop" ? "Stop" : label;
  button.title = action === "logs" ? "View service logs" : service.observe_only ? "This service is observed; manage it with its host supervisor" : service.enabled === false ? "Enable this service in the workspace manifest" : service.capabilities?.lifecycle === false ? "This integration does not support lifecycle actions" : `${actionName} ${service.name}`;
  button.setAttribute("aria-label", `${actionName} ${service.name}`);
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
    const info = node("div");
    info.append(node("div", "service-name", service.name || service.id));
    const meta = node("div", "service-meta");
    meta.append(node("span", "", service.runtime || "container"), node("span", "separator", "/"), node("span", "", service.observe_only ? "observe only" : service.enabled === false ? "disabled" : service.kind || "service"));
    info.append(meta);
    cell.append(node("span", "service-icon", (service.name || service.id || "s").slice(0, 1).toLowerCase()), info);
    identity.append(cell);
    if (service.description) identity.title = service.description;
    const statusCell = node("td");
    const status = service.status || "unknown";
    const badge = node("span", `status-badge ${["running", "healthy", "failed", "unhealthy", "error", "starting", "queued", "pending"].includes(status) ? status : ""}`);
    badge.append(node("span", "status-dot"), document.createTextNode(status));
    statusCell.append(badge);
    const detail = service.health && !["none", "unknown", "no healthcheck"].includes(service.health) ? service.health : service.paused ? "Paused by operator" : service.next_run ? `Next ${relativeTime(service.next_run)}` : service.last_run ? `Ran ${relativeTime(service.last_run)}` : service.observe_only ? "Host supervised" : "";
    if (detail) statusCell.append(node("span", "cell-sub", detail));
    const cpu = node("td", "cell-value", numeric(service.cpu_percent) ? `${service.cpu_percent.toFixed(1)}%` : "—");
    cpu.append(node("span", "cell-sub", "live usage"));
    const ram = node("td", "cell-value", memory(service.memory_mb));
    ram.append(node("span", "cell-sub", "live usage"));
    const limitsCell = node("td");
    const limits = node("button", "limit-button");
    limits.type = "button";
    limits.dataset.service = service.id;
    limits.dataset.action = "resources";
    limits.disabled = service.observe_only || service.enabled === false || service.capabilities?.scale === false || pending.has(service.id);
    limits.title = limits.disabled ? "This service does not support resource scaling" : `Edit resource limits for ${service.name}`;
    limits.setAttribute("aria-label", limits.title);
    limits.append(node("span", "", `${cores(service.limits?.cpus)} CPU${limits.disabled ? "" : " ↗"}`), node("span", "", memory(service.limits?.memory_mb)));
    limitsCell.append(limits);
    const actionCell = node("td");
    const actions = node("div", "actions");
    const isJob = ["job", "scheduled", "cron"].includes(service.kind);
    if (service.status === "running") actions.append(actionButton(service, "restart", "Restart"), actionButton(service, "stop", "■", "icon-button"));
    else actions.append(actionButton(service, isJob ? "run" : "start", isJob ? "Run now" : "Start"));
    actions.append(actionButton(service, "logs", "≡", "icon-button"));
    actionCell.append(actions);
    row.append(identity, statusCell, cpu, ram, limitsCell, actionCell);
    fragment.append(row);
  }
  $("service-rows").replaceChildren(fragment);
  $("table-count").textContent = services.length;
  $("shown-count").textContent = `${shown.length} of ${services.length} services`;
  $("empty-state").hidden = shown.length > 0;
  $("empty-state").querySelector("h3").textContent = services.length ? "No matching services" : "No services here yet";
  $("empty-state").querySelector("p").textContent = services.length ? "Try another search or filter." : "Add a service to your workspace manifest, then refresh.";
}

function renderEvents() {
  const events = (state.events || []).slice(0, 8);
  const fragment = document.createDocumentFragment();
  for (const event of events) {
    const item = node("li", "event-item");
    const time = event.timestamp || event.time || event.at;
    const label = event.message || `${event.service || event.service_id || "Workspace"}: ${event.action || event.kind || "updated"}`;
    const timeEl = node("time", "event-time", relativeTime(time));
    const date = timestamp(time);
    if (date) { timeEl.dateTime = date.toISOString(); timeEl.title = date.toLocaleString(); }
    item.append(node("span", "event-icon", event.ok === false || event.level === "error" ? "!" : "↳"), node("span", "event-message", label), timeEl);
    fragment.append(item);
  }
  $("event-list").replaceChildren(fragment);
  $("no-events").hidden = events.length > 0;
}

async function refresh() {
  if (refreshing) return;
  refreshing = true;
  $("refresh").disabled = true;
  try {
    await session();
    state = await api("/api/state");
    renderSummary(); renderServices(); renderEvents();
    $("error-banner").hidden = true;
    if (state.manifest_error) showError(`Manifest: ${state.manifest_error}. Showing the last valid configuration.`);
    $("refresh-status").textContent = "Auto-refresh · 5s";
    $("updated-at").textContent = `Updated ${new Date().toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`;
    $("budget-edit").disabled = false;
  } catch (error) {
    showError(error.message);
    $("refresh-status").textContent = "Connection interrupted";
  } finally { refreshing = false; $("refresh").disabled = false; }
}

async function loadLogs() {
  if (!logsService) return;
  $("refresh-logs").disabled = true;
  try {
    const result = await api(`/api/services/${encodeURIComponent(logsService.id)}/logs?tail=200`);
    $("logs-output").textContent = result.logs || "No output recorded for this service.";
  } catch (error) { $("logs-output").textContent = `Unable to load logs: ${error.message}`; }
  finally { $("refresh-logs").disabled = false; }
}

function openResources(service) {
  if (resourceApplying) return;
  resourceService = service;
  const limits = service ? service.limits : state.budget;
  $("resources-title").textContent = service ? service.name : "Workspace budget";
  $("resource-note").textContent = service ? "Set hard service limits. The workspace budget must have room for this allocation." : "Set the global CPU and memory budget. It must cover daemon reservations and leave room for scheduled jobs.";
  $("resource-cpu").value = limits?.cpus ?? "";
  $("resource-memory").value = limits?.memory_mb ?? "";
  $("resource-error").hidden = true;
  $("resources-dialog").showModal();
}

$("service-rows").addEventListener("click", async (event) => {
  const button = event.target.closest("button[data-service]");
  if (!button || button.disabled) return;
  const service = state.services.find((item) => item.id === button.dataset.service);
  if (!service) return;
  const action = button.dataset.action;
  if (action === "resources") { openResources(service); return; }
  if (action === "logs") {
    logsService = service;
    $("logs-title").textContent = service.name;
    $("logs-output").textContent = "Loading…";
    $("logs-dialog").showModal();
    await loadLogs();
    return;
  }
  pending.add(service.id); renderServices();
  try {
    await post(`/api/services/${encodeURIComponent(service.id)}/${action}`);
    toast(`${service.name}: ${action} completed`);
    await refresh();
  } catch (error) { showError(error.message); }
  finally { pending.delete(service.id); renderServices(); }
});

$("resource-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (resourceApplying) return;
  const limits = { cpus: Number($("resource-cpu").value), memory_mb: Number($("resource-memory").value) };
  if (!Number.isFinite(limits.cpus) || limits.cpus <= 0 || !Number.isInteger(limits.memory_mb) || limits.memory_mb <= 0) {
    $("resource-error").textContent = "Enter positive CPU cores and a whole number of MiB.";
    $("resource-error").hidden = false;
    return;
  }
  resourceApplying = true;
  $("apply-resources").disabled = true;
  try {
    const path = resourceService ? `/api/services/${encodeURIComponent(resourceService.id)}/apply` : "/api/budget";
    await post(path, resourceService ? { limits } : limits);
    $("resources-dialog").close();
    toast(resourceService ? `${resourceService.name}: resource limits applied` : "Workspace budget updated");
    await refresh();
  } catch (error) {
    $("resource-error").textContent = error.message;
    $("resource-error").hidden = false;
  } finally { resourceApplying = false; $("apply-resources").disabled = false; }
});

document.querySelectorAll(".close-dialog").forEach((button) => { button.addEventListener("click", () => button.closest("dialog").close()); });
document.querySelectorAll("dialog").forEach((dialog) => { dialog.addEventListener("click", (event) => { if (event.target === dialog) { const r = dialog.getBoundingClientRect(); if (event.clientX < r.left || event.clientX > r.right || event.clientY < r.top || event.clientY > r.bottom) dialog.close(); } }); });
document.querySelectorAll("[data-filter]").forEach((button) => {
  button.addEventListener("click", () => {
    currentFilter = button.dataset.filter;
    document.querySelectorAll("[data-filter]").forEach((item) => { item.classList.toggle("active", item === button); item.setAttribute("aria-pressed", String(item === button)); });
    renderServices();
  });
});
$("search").addEventListener("input", renderServices);
$("refresh").addEventListener("click", refresh);
$("refresh-logs").addEventListener("click", loadLogs);
$("budget-edit").addEventListener("click", () => { if (state) openResources(null); });
$("budget-edit").disabled = true;
let savedTheme;
try { savedTheme = localStorage.getItem("selfhost-theme"); } catch { /* A blocked preference store does not prevent control. */ }
document.documentElement.dataset.theme = savedTheme || (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
$("theme-toggle").addEventListener("click", () => {
  const theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  document.documentElement.dataset.theme = theme;
  try { localStorage.setItem("selfhost-theme", theme); } catch { /* Theme still applies for this session. */ }
});
refresh();
setInterval(() => { if (!document.hidden) refresh(); }, 5000);
document.addEventListener("visibilitychange", () => { if (!document.hidden) refresh(); });
