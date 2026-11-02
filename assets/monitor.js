"use strict";

// Host counters belong to the selected controller, independent of workspace filters.
const hostHistory = new Map();
let hostChartWindow = 300;
function hostBytes(value) {
  if (!numeric(value)) return "—";
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let unit = 0;
  while (Math.abs(value) >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
  return `${value.toLocaleString(undefined, {maximumFractionDigits: unit ? 1 : 0})} ${units[unit]}`;
}
function hostPercent(value) { return numeric(value) ? `${value.toFixed(1)}%` : "Collecting…"; }
function rememberHost(key, at, value) {
  if (!numeric(at)) return;
  const history = hostHistory.get(key) || [];
  const sample = {at, value: numeric(value) ? value : null};
  if (history.length && at < history[history.length - 1].at) return;
  if (history[history.length - 1]?.at === at) history[history.length - 1] = sample;
  else history.push(sample);
  while (history.length > 720 || (history.length > 1 && history[0].at < at - 3600)) history.shift();
  hostHistory.set(key, history);
}
function drawHostChart(canvas) {
  const box = canvas.getBoundingClientRect();
  if (!box.width || !box.height) return;
  const density = window.devicePixelRatio || 1;
  canvas.width = Math.round(box.width * density); canvas.height = Math.round(box.height * density);
  const ctx = canvas.getContext("2d"); if (!ctx) return;
  ctx.scale(density, density);
  const tokens = getComputedStyle(document.documentElement);
  const colors = [tokens.getPropertyValue("--graph-cpu").trim(), tokens.getPropertyValue("--graph-mem").trim()];
  const keys = canvas.dataset.hostHistory.split("|");
  const histories = keys.map(key => hostHistory.get(key) || []);
  const newest = Math.max(0, ...histories.map(h => h[h.length - 1]?.at || 0));
  const samples = histories.map(history => history.filter(sample => sample.at >= newest - hostChartWindow));
  const values = samples.flat().filter(sample => numeric(sample.value));
  const max = canvas.dataset.percent === "true" ? 100 : Math.max(1, ...values.map(sample => sample.value)) * 1.15;
  const left = 48, width = Math.max(1, box.width - left - 12), height = Math.max(1, box.height - 30);
  const earliest = Math.min(newest, ...samples.flat().map(sample => sample.at));
  const start = Math.max(newest - hostChartWindow, Math.min(earliest, newest - 30));
  const x = at => left + (at - start) / Math.max(1, newest - start) * width;
  const y = value => 8 + height * (1 - value / max);
  ctx.strokeStyle = tokens.getPropertyValue("--line").trim(); ctx.lineWidth = 1;
  ctx.fillStyle = tokens.getPropertyValue("--muted").trim(); ctx.font = "11px -apple-system, sans-serif";
  for (const fraction of [0, .5, 1]) {
    ctx.beginPath(); ctx.moveTo(left, y(max * fraction)); ctx.lineTo(left + width, y(max * fraction)); ctx.stroke();
    ctx.textAlign = "right";
    ctx.fillText(canvas.dataset.percent === "true" ? `${Math.round(max * fraction)}%` : hostBytes(max * fraction).replace(" ", ""), left - 6, y(max * fraction) + 4);
  }
  samples.forEach((history, index) => {
    ctx.strokeStyle = colors[index % colors.length]; ctx.fillStyle = ctx.strokeStyle; ctx.lineWidth = 2;
    ctx.beginPath(); let previous = null;
    for (const sample of history) {
      if (!numeric(sample.value)) { previous = null; continue; }
      if (!previous || sample.at - previous.at > pollIntervalMs / 1000 * 2.5) ctx.moveTo(x(sample.at), y(sample.value));
      else ctx.lineTo(x(sample.at), y(sample.value));
      previous = sample;
    }
    ctx.stroke();
    const last = history[history.length - 1];
    if (numeric(last?.value)) { ctx.beginPath(); ctx.arc(x(last.at), y(last.value), 2.5, 0, Math.PI * 2); ctx.fill(); }
  });
  ctx.fillStyle = tokens.getPropertyValue("--muted").trim(); ctx.textAlign = "left";
  ctx.fillText(values.length ? new Date(start * 1000).toLocaleTimeString() : "Waiting for measured samples…", left, box.height - 3);
  if (values.length) { ctx.textAlign = "right"; ctx.fillText(new Date(newest * 1000).toLocaleTimeString(), left + width, box.height - 3); }
}
function hostChart(keys, label, percent = false) {
  const canvas = node("canvas", "host-chart");
  canvas.dataset.hostHistory = keys.join("|"); canvas.dataset.percent = String(percent);
  canvas.setAttribute("role", "img"); canvas.setAttribute("aria-label", label);
  return canvas;
}
function hostMeter(value, label) {
  const meter = node("meter", "host-meter"); meter.min = 0; meter.max = 100;
  meter.value = numeric(value) ? Math.max(0, Math.min(100, value)) : 0;
  meter.setAttribute("aria-label", `${label}: ${hostPercent(value)}`);
  meter.hidden = !numeric(value);
  return meter;
}
function renderHostMonitor(metrics, machineName) {
  const root = $("host-monitor-content");
  if (!metrics) {
    root.replaceChildren(node("p", "quiet-message", "Host monitoring is unavailable. Update this machine's Init controller to a version with host metrics."));
    return;
  }
  const cpu = metrics.cpu || {}, ram = metrics.memory || {};
  const usedPercent = numeric(ram.used_bytes) && ram.total_bytes > 0 ? ram.used_bytes / ram.total_bytes * 100 : null;
  rememberHost("host-cpu", metrics.sampled_at, cpu.percent);
  rememberHost("host-memory", metrics.sampled_at, usedPercent);
  for (const net of metrics.network?.interfaces || []) {
    rememberHost(`net-rx:${net.name}`, metrics.sampled_at, net.rx_bytes_per_second);
    rememberHost(`net-tx:${net.name}`, metrics.sampled_at, net.tx_bytes_per_second);
  }
  if (currentRoute !== "monitor") return;
  $("host-monitor-scope").textContent = `${machineName} · ${metrics.os || "OS unknown"} · Controller host, including processes outside Init. Docker VM usage remains in Services. Workspace filters do not change this view.`;
  const grid = node("div", "host-monitor-grid");
  const cpuPanel = node("section", "panel host-panel host-cpu-panel");
  cpuPanel.append(node("h2", "", "CPU"), node("p", "host-value", hostPercent(cpu.percent)), node("p", "muted", `${cpu.logical_cores ?? "Unknown"} logical CPUs · 100% means all busy`));
  cpuPanel.append(hostChart(["host-cpu"], `Host CPU history. Current use ${hostPercent(cpu.percent)}.`, true));
  const coreGrid = node("div", "host-core-grid");
  for (const core of cpu.cores || []) {
    const row = node("div", "host-core");
    row.append(node("span", "", `CPU ${core.id}`), hostMeter(core.percent, `CPU ${core.id}`), node("span", "host-core-value", numeric(core.percent) ? hostPercent(core.percent) : "—")); coreGrid.append(row);
  }
  cpuPanel.append(coreGrid);
  const memoryPanel = node("section", "panel host-panel");
  memoryPanel.append(node("h2", "", "Memory"), node("p", "host-value", hostBytes(ram.used_bytes)), node("p", "muted", `of ${hostBytes(ram.total_bytes)} · ${hostPercent(usedPercent)} in use`));
  memoryPanel.append(hostChart(["host-memory"], `Host memory history. ${hostBytes(ram.used_bytes)} in use.`, true));
  const breakdown = node("dl", "host-memory-details");
  for (const [label, key] of [["Available", "available_bytes"], ["Cache / reclaimable", "cached_bytes"], ["Active", "active_bytes"], ["Inactive", "inactive_bytes"], ["Wired", "wired_bytes"], ["Compressed", "compressed_bytes"], ["Swap used", "swap_used_bytes"], ["Swap total", "swap_total_bytes"]]) {
    if (numeric(ram[key]) || ["available_bytes", "cached_bytes", "swap_used_bytes", "swap_total_bytes"].includes(key)) breakdown.append(node("dt", "", label), node("dd", "", hostBytes(ram[key])));
  }
  memoryPanel.append(breakdown, node("p", "field-help", [ram.available_basis, ram.cached_basis].filter(Boolean).join(". ") || "Unavailable measurements are shown as a dash."));
  grid.append(cpuPanel, memoryPanel);
  const network = node("section", "panel host-panel host-network-panel"); network.append(node("h2", "", "Network interfaces"), node("p", "muted", "Receive and send rates measured between samples. Counters are cumulative since the OS created the interface; tunnels and physical interfaces may count the same traffic."));
  const interfaces = metrics.network?.interfaces || [];
  const cards = node("div", "host-network-grid");
  for (const net of [...interfaces].sort((a,b) => ((b.rx_bytes || 0) + (b.tx_bytes || 0)) - ((a.rx_bytes || 0) + (a.tx_bytes || 0)) || a.name.localeCompare(b.name))) {
    const card = node("article", "host-interface"); card.append(node("h3", "", net.name));
    const rates = node("div", "host-net-rates");
    rates.append(node("span", "host-rx", `↓ ${numeric(net.rx_bytes_per_second) ? hostBytes(net.rx_bytes_per_second) + "/s" : "Collecting…"}`), node("span", "host-tx", `↑ ${numeric(net.tx_bytes_per_second) ? hostBytes(net.tx_bytes_per_second) + "/s" : "Collecting…"}`));
    card.append(rates, hostChart([`net-rx:${net.name}`, `net-tx:${net.name}`], `${net.name} receive and send history. Current rates: ${rates.textContent}.`), node("p", "field-help", `Received ${hostBytes(net.rx_bytes)} · Sent ${hostBytes(net.tx_bytes)}`)); cards.append(card);
  }
  network.append(interfaces.length ? cards : node("p", "quiet-message", "No network counters available.")); grid.append(network);
  const status = node("p", "history-note", `Sampled ${fullTime(metrics.sampled_at)}${numeric(metrics.interval_seconds) ? ` · ${metrics.interval_seconds.toFixed(1)}s interval` : " · Collecting first interval"}. History is kept in this browser for up to one hour. CPU IDs identify logical CPUs, not dedicated service assignments.`);
  root.replaceChildren(grid, status);
  if (metrics.errors?.length) root.append(node("p", "form-warning", metrics.errors.join(" ")));
  root.querySelectorAll("canvas[data-host-history]").forEach(drawHostChart);
}
