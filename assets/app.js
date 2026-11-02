"use strict";

const $ = (id) => document.getElementById(id);
let state = null;
let sessionToken = null;
let currentFilter = "all";
let currentProject = "";
let currentTarget = "";
let currentRoute = "overview";
let chartWindow = 86400;
let pollIntervalMs = 5000;
let pollTimer = null;
try { const saved=Number(readStoredValue(localStorage,"init-update-ms"));if([5000,10000,30000].includes(saved))pollIntervalMs=saved; } catch {}
let deployApplying = false;
let deploymentStep = 1;
let projectApplying = false;
let deployPreviewSignature = null;
let deployRequest = 0;
let deployNeedsReuse = false;
let refreshPromise = null;
const graphSelection = new Map();
let detailsServiceId = null;
let detailsLogRequest = 0;
let detailsLogsLoading = null;
let detailsTab = "overview";
let resourceService = null;
let resourceApplying = false;
let previewSignature = null;
let previewRequest = 0;
let toastTimer;
let lastStateAt = null;
const pending = new Set();
const usageHistory = new Map();
const HISTORY_SECONDS = 3600;

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
function rememberUsage(key, sampledAt, cpu, ram) {
  if (!numeric(sampledAt) || sampledAt <= 0) return;
  const history = usageHistory.get(key) || [];
  const sample = { at: sampledAt, cpu: numeric(cpu) ? cpu : null, memory: numeric(ram) ? ram : null };
  const last = history[history.length - 1];
  if (last && sample.at < last.at) return;
  if (last?.at === sample.at) history[history.length - 1] = sample;
  else history.push(sample);
  while (history.length > 720 || (history.length > 1 && history[0].at < sample.at - HISTORY_SECONDS)) history.shift();
  usageHistory.set(key, history);
}
function eligibleService(service) { if(typeof service.telemetry_eligible === "boolean")return service.telemetry_eligible; return !["not created", "stopped", "exited", "disabled", "inactive", "missing"].includes(service.status); }
function servicesForProject(project = currentProject) { return (state?.services || []).filter(service => !project || (project === "__default__" ? !service.project : service.project === project)); }
function projectName(id) { return !id || id === "__default__" ? "Default" : (state?.projects || []).find(project => project.id === id)?.name || id; }
function scopeName() { return currentProject ? projectName(currentProject) : "All workspaces"; }
function targetName(service) { return service.execution?.label || (service.runtime === "docker" ? state?.runtime?.execution?.label : null) || "Location not reported"; }
function scopedTelemetry(services = servicesForProject()) {
  const targets=new Map(),now=Date.now()/1000;
  for(const service of services){
    const key=service.telemetry_target||service.id;let target=targets.get(key);if(!target){target={eligible:false,cpu:null,memory:null,at:null};targets.set(key,target);}
    const eligible=eligibleService(service);target.eligible||=eligible;
    const at=service.sampled_at;const fresh=numeric(at)&&at>0&&at<=now&&now-at<=30;
    const valid=value=>eligible&&fresh&&numeric(value)&&value>=0&&value<=1e15;
    const cpu=valid(service.cpu_percent)?service.cpu_percent:null,memory=valid(service.memory_mb)?service.memory_mb:null;
    if(target.cpu===null&&cpu!==null)target.cpu=cpu;if(target.memory===null&&memory!==null)target.memory=memory;
    if(cpu!==null||memory!==null)target.at=target.at===null?at:Math.min(target.at,at);
  }
  const eligible=[...targets.values()].filter(target=>target.eligible),cpu=eligible.filter(target=>numeric(target.cpu)),ram=eligible.filter(target=>numeric(target.memory));
  const times=eligible.map(target=>target.at).filter(numeric);
  return {eligible:eligible.length,cpu_count:cpu.length,memory_count:ram.length,cpu:cpu.length?cpu.reduce((sum,target)=>sum+target.cpu,0):null,memory:ram.length?ram.reduce((sum,target)=>sum+target.memory,0):null,at:times.length?Math.min(...times):null,cpu_complete:eligible.length>0&&cpu.length===eligible.length,memory_complete:eligible.length>0&&ram.length===eligible.length};
}
function scopeHistoryKey(project = currentProject) { return `__project__${project || "__all__"}`; }
function recordUsage() {
  for (const service of state.services || []) rememberUsage(service.id, service.sampled_at, service.cpu_percent, service.memory_mb);
  for (const project of ["", "__default__", ...(state.projects || []).map(project => project.id)]) {
    const measured = scopedTelemetry(servicesForProject(project));
    rememberUsage(scopeHistoryKey(project), measured.at, measured.cpu_complete ? measured.cpu : null, measured.memory_complete ? measured.memory : null);
  }
}
const historyRequests = new Map();
const recordedHistoryMeta = new Map();
const HISTORY_REFRESH_MS = 15000;
function recordedHistoryKey(service = null, project = currentProject, windowSeconds = chartWindow) {
  return `recorded:${JSON.stringify({service,project:service?null:project,window:windowSeconds})}`;
}
function historyDescriptor(service = null, project = currentProject, windowSeconds = chartWindow) {
  return {service, project:service?null:project, window:windowSeconds, key:recordedHistoryKey(service,project,windowSeconds)};
}
function currentHistoryDescriptor() { return historyDescriptor(null,currentProject,chartWindow); }
function requestRecordedHistory(descriptor, force = false) {
  const previous=historyRequests.get(descriptor.key);
  if(previous?.loading || (!force && previous && Date.now()-previous.requestedAt<HISTORY_REFRESH_MS))return;
  const entry={...previous,loading:true,error:null,requestedAt:Date.now()};historyRequests.set(descriptor.key,entry);
  const params=new URLSearchParams({window_seconds:String(descriptor.window),max_points:"720"});
  if(descriptor.service)params.set("service",descriptor.service);else if(descriptor.project)params.set("project",descriptor.project);
  api(`/api/history?${params}`).then(response=>{
    if(!Array.isArray(response.points))throw new Error("The controller returned invalid history data");
    entry.response=response;entry.loadedAt=Date.now();entry.loading=false;
    const samples=response.points.filter(point=>numeric(point.sampled_at)).map(point=>({
      at:point.sampled_at,cpu:numeric(point.cpu_percent)?point.cpu_percent:null,memory:numeric(point.memory_mb)?point.memory_mb:null,
      first:point.first_sample_at,last:point.last_sample_at,cpu_min:point.cpu_min,cpu_max:point.cpu_max,memory_min:point.memory_min,memory_max:point.memory_max,
      samples:point.samples,eligible_samples:point.eligible_samples,cpu_samples:point.cpu_samples,memory_samples:point.memory_samples,
      cpu_complete:point.cpu_complete,memory_complete:point.memory_complete
    }));
    usageHistory.set(descriptor.key,samples);recordedHistoryMeta.set(descriptor.key,response);
    renderState();
  }).catch(error=>{entry.loading=false;entry.error=error.message;renderRecordedHistoryStatus();});
  if(historyRequests.size>32){const oldest=[...historyRequests].find(([key,value])=>key!==descriptor.key&&!value.loading);if(oldest){historyRequests.delete(oldest[0]);recordedHistoryMeta.delete(oldest[0]);usageHistory.delete(oldest[0]);}}
}
function synchronizeHistory(force = false) {
  if(!state||document.hidden)return;
  if(currentRoute==="overview")requestRecordedHistory(currentHistoryDescriptor(),force);
  if(currentRoute==="workspaces"&&currentProject)requestRecordedHistory(historyDescriptor(null,currentProject,86400),force);
  if($("details-dialog").open&&detailsTab==="usage"&&detailsServiceId)requestRecordedHistory(historyDescriptor(detailsServiceId,null,chartWindow),force);
  renderRecordedHistoryStatus();
}
function historicalStats(key, metric) {
  const samples=usageHistory.get(key)||[];let total=0,weight=0,minimum=null,maximum=null;
  for(const sample of samples){const value=sample[metric];if(!numeric(value))continue;const count=sample[`${metric}_samples`];if(!numeric(count)||count<=0)continue;total+=value*count;weight+=count;const low=sample[`${metric}_min`],high=sample[`${metric}_max`];if(numeric(low))minimum=minimum===null?low:Math.min(minimum,low);if(numeric(high))maximum=maximum===null?high:Math.max(maximum,high);}
  return {average:weight?total/weight:null,minimum,maximum,count:weight};
}
function historyStatsText(key, metric) {
  const stats=historicalStats(key,metric);const format=value=>!numeric(value)?"—":metric==="cpu"?`${cores(value)}%`:memory(value);
  return stats.count?`Window observations · average ${format(stats.average)} · min ${format(stats.minimum)} · max ${format(stats.maximum)}`:"No recorded measurements in this window";
}
function historyStatusText(descriptor) {
  const entry=historyRequests.get(descriptor.key);if(!entry)return "Loading recorded utilization…";
  if(entry.error)return `Recorded history unavailable: ${entry.error}. Retrying automatically.${entry.response?` Showing history loaded at ${fullTime(entry.loadedAt/1000)}.`:""}`;
  if(!entry.response)return "Loading recorded utilization…";
  const response=entry.response;const warning=response.error||response.status==="error"?`History collection error: ${response.error||"collection is unavailable"}. Showing retained records. `:"";const measured=response.points.some(point=>numeric(point.cpu_percent)||numeric(point.memory_mb));
  if(!measured)return `${warning}No recorded utilization for this scope yet. Earlier time is not backfilled.`;
  const interval=response.bucket_seconds>=60?`${response.bucket_seconds/60}-minute`:`${response.bucket_seconds}-second`;
  const coverage=response.coverage;const summary=coverage?` CPU data in ${coverage.cpu_buckets}/${coverage.buckets} buckets; memory in ${coverage.memory_buckets}/${coverage.buckets}.`:"";
  return `${warning}${interval} bucket averages of recorded measurements. Window statistics summarize observed values.${summary} Min/max show observed bounds. Gaps and partial buckets indicate missing measurements.${numeric(response.earliest_at)?` Available records begin ${fullTime(response.earliest_at)}.`:""}${entry.loading?" Updating…":""}`;
}
function renderRecordedHistoryStatus() {
  if($("chart-note"))$("chart-note").textContent=historyStatusText(currentHistoryDescriptor());
  if($("workspace-history-status"))$("workspace-history-status").textContent=historyStatusText(historyDescriptor(null,currentProject,86400));
  for(const output of document.querySelectorAll("[data-history-summary]"))output.textContent=historyStatsText(output.dataset.historySummary,output.dataset.metric);
  const detailStatus=$("detail-history-status");if(detailStatus&&detailsServiceId)detailStatus.textContent=historyStatusText(historyDescriptor(detailsServiceId,null,chartWindow));
}
function setHistoryWindow(seconds) {
  if(![300,900,3600,86400].includes(seconds))return;
  chartWindow=seconds;document.querySelectorAll("[data-window]").forEach(button=>{const active=Number(button.dataset.window)===chartWindow;button.classList.toggle("active",active);button.setAttribute("aria-pressed",String(active));});
  if(state){renderOverview();renderDetails();renderRecordedHistoryStatus();renderUsageCharts();synchronizeHistory(true);}
}
function drawUsageChart(canvas) {
  const history=usageHistory.get(canvas.dataset.history)||[];const metric=canvas.dataset.metric==="memory"?"memory":"cpu";const spark=canvas.dataset.sparkline==="true";
  const meta=recordedHistoryMeta.get(canvas.dataset.history);const historical=Boolean(meta);
  const newest=historical?meta.end:history[history.length-1]?.at||0;const windowSeconds=Number(canvas.dataset.window)||(spark?900:chartWindow);
  const samples=history.filter(sample=>sample.at>=newest-windowSeconds);const measured=samples.filter(sample=>numeric(sample[metric]));
  const box=canvas.getBoundingClientRect();if(box.width<1||box.height<1)return;const density=window.devicePixelRatio||1;canvas.width=Math.round(box.width*density);canvas.height=Math.round(box.height*density);const context=canvas.getContext("2d");if(!context)return;context.scale(density,density);
  const tokens=getComputedStyle(document.documentElement);const color=tokens.getPropertyValue(metric==="cpu"?"--graph-cpu":"--graph-mem").trim()||tokens.getPropertyValue("--accent").trim();const muted=tokens.getPropertyValue("--muted").trim();const line=tokens.getPropertyValue("--line").trim();context.font=`12px ${getComputedStyle(canvas).fontFamily}`;
  const limit=Number(canvas.dataset.limit)||0;const peak=Math.max(0,...measured.map(sample=>sample[metric]),limit);const step=metric==="cpu"?(peak<=5?1:peak<=20?5:peak<=100?25:100):peak>1024?256:64;const maximum=Math.max(step,Math.ceil(peak/step)*step);
  const plot=spark?{x:1,y:2,width:Math.max(1,box.width-3),height:Math.max(1,box.height-4)}:{x:53,y:12,width:Math.max(1,box.width-66),height:Math.max(1,box.height-44)};
  const first=historical?meta.start:samples[0]?.at||0;const last=historical?meta.end:samples[samples.length-1]?.at||first;
  const x=at=>plot.x+(at-first)/Math.max(1,last-first)*plot.width;const y=value=>plot.y+plot.height*(1-value/maximum);const format=value=>!numeric(value)?"—":metric==="cpu"?`${cores(value)}%`:memory(value);
  const interval=historical?meta.bucket_seconds:pollIntervalMs/1000;const complete=sample=>sample[`${metric}_complete`]!==false;
  context.lineWidth=1;context.strokeStyle=line;context.fillStyle=muted;
  if(!spark)for(const fraction of [0,.5,1]){const level=maximum*fraction;context.beginPath();context.moveTo(plot.x,y(level));context.lineTo(plot.x+plot.width,y(level));context.stroke();context.textAlign="right";context.fillText(format(level),plot.x-7,y(level)+4);}
  if(!measured.length){canvas.__chart=null;const card=canvas.closest(".chart-card");const readout=card?.querySelector(".chart-readout");if(readout)readout.textContent=historical?"No recorded measurements in this window.":"Waiting for measured samples.";card?.querySelector(".chart-samples tbody")?.replaceChildren();if(!spark){context.textAlign="left";context.fillText(historical?"No recorded measurements yet":"Waiting for measured samples…",plot.x+8,plot.y+plot.height/2);}canvas.setAttribute("aria-label",`${metric==="cpu"?"CPU":"Memory"} history: no measured data.`);return;}
  let gaps=0,inGap=false;
  for(let i=1;i<samples.length;i++){const a=samples[i-1],b=samples[i];const missing=b.at-a.at>interval*2.5||!numeric(a[metric])||!numeric(b[metric])||!complete(a)||!complete(b);if(missing){if(!inGap)gaps++;inGap=true;if(!spark){context.fillStyle=line;context.globalAlpha=.16;context.fillRect(x(a.at),plot.y,Math.max(1,x(b.at)-x(a.at)),plot.height);context.globalAlpha=1;}}else inGap=false;}
  if(limit>0&&!spark){context.setLineDash([4,4]);context.strokeStyle=muted;context.beginPath();context.moveTo(plot.x,y(limit));context.lineTo(plot.x+plot.width,y(limit));context.stroke();context.setLineDash([]);context.fillStyle=muted;context.textAlign="right";context.fillText(`Limit ${format(limit)}`,plot.x+plot.width,Math.max(11,y(limit)-5));}
  context.strokeStyle=color;context.lineWidth=1.5;context.beginPath();let previous=null;
  for(const sample of samples){if(!numeric(sample[metric])){previous=null;continue;}if(previous&&sample.at-previous.at<=interval*2.5&&complete(previous)&&complete(sample))context.lineTo(x(sample.at),y(sample[metric]));else context.moveTo(x(sample.at),y(sample[metric]));previous=sample;}
  context.stroke();const latest=measured[measured.length-1];context.fillStyle=color;context.beginPath();context.arc(!historical&&measured.length===1?plot.x+plot.width:x(latest.at),y(latest[metric]),spark?1.7:2.5,0,Math.PI*2);context.fill();
  if(spark)return;
  const key=`${canvas.dataset.history}|${metric}`;const selectedAt=graphSelection.get(key);const selected=numeric(selectedAt)?measured.reduce((nearest,sample)=>Math.abs(sample.at-selectedAt)<Math.abs(nearest.at-selectedAt)?sample:nearest,measured[0]):null;
  if(selected){context.strokeStyle=muted;context.setLineDash([2,3]);context.beginPath();context.moveTo(x(selected.at),plot.y);context.lineTo(x(selected.at),plot.y+plot.height);context.stroke();context.setLineDash([]);context.fillStyle=color;context.beginPath();context.arc(x(selected.at),y(selected[metric]),3,0,Math.PI*2);context.fill();}
  context.fillStyle=muted;
  if(!historical&&measured.length<2){context.textAlign="left";context.fillText("Collecting first samples…",plot.x+8,plot.y+plot.height/2);}
  else for(let i=0;i<3;i++){const at=first+(last-first)*i/2;context.textAlign=i===0?"left":i===2?"right":"center";context.fillText(new Date(at*1000).toLocaleTimeString(undefined,{hour:"2-digit",minute:"2-digit",...(windowSeconds<3600?{second:"2-digit"}:{})}),plot.x+plot.width*i/2,box.height-7);}
  const describe=sample=>{const at=numeric(sample.first)?sample.first:sample.at;const time=new Date(at*1000).toLocaleTimeString(undefined,{hour:"2-digit",minute:"2-digit",second:"2-digit"});const end=numeric(sample.last)&&sample.last!==at?`–${new Date(sample.last*1000).toLocaleTimeString(undefined,{hour:"2-digit",minute:"2-digit",second:"2-digit"})}`:"";const bounds=historical?` · min ${format(sample[`${metric}_min`])}, max ${format(sample[`${metric}_max`])}`:"";return `${time}${end} · ${format(sample[metric])}${historical?" bucket average":""}${bounds}${!complete(sample)?" · partial coverage":""}`;};
  const chosen=selected||latest;const card=canvas.closest(".chart-card");const readout=card?.querySelector(".chart-readout");if(readout)readout.textContent=describe(chosen);
  const body=card?.querySelector(".chart-samples tbody");if(body){const fragment=document.createDocumentFragment();for(const sample of measured){const row=node("tr");row.append(node("td","",describe(sample)));fragment.append(row);}body.replaceChildren(fragment);}
  canvas.setAttribute("aria-label",`${metric==="cpu"?"CPU":"Memory"} history, ${measured.length} ${historical?"recorded average buckets":"actual samples"} and ${gaps} missing or partial spans. Latest ${format(latest[metric])}${historical?" average":""}. Left and right arrows inspect data.${limit?" Dashed line is the runtime limit.":""}`);
  canvas.__chart={measured,key,plot,first,last};
  if(!canvas.dataset.bound){canvas.dataset.bound="true";canvas.addEventListener("keydown",event=>{const chart=canvas.__chart;if(!chart)return;let index=chart.measured.findIndex(sample=>sample.at===graphSelection.get(chart.key));if(index<0)index=chart.measured.length-1;const next=event.key==="ArrowLeft"?Math.max(0,index-1):event.key==="ArrowRight"?Math.min(chart.measured.length-1,index+1):event.key==="Home"?0:event.key==="End"?chart.measured.length-1:-1;if(next>=0){event.preventDefault();graphSelection.set(chart.key,chart.measured[next].at);drawUsageChart(canvas);}});canvas.addEventListener("pointermove",event=>{const chart=canvas.__chart;if(!chart)return;const bounds=canvas.getBoundingClientRect();const at=chart.first+(event.clientX-bounds.left-chart.plot.x)/chart.plot.width*(chart.last-chart.first);graphSelection.set(chart.key,at);drawUsageChart(canvas);});canvas.addEventListener("pointerleave",()=>{if(!canvas.isConnected)return;if(document.activeElement!==canvas){graphSelection.delete(canvas.__chart?.key);drawUsageChart(canvas);}});canvas.addEventListener("blur",()=>{if(!canvas.isConnected)return;graphSelection.delete(canvas.__chart?.key);drawUsageChart(canvas);});}
}
function renderUsageCharts() { document.querySelectorAll("canvas[data-history]").forEach(drawUsageChart); if(state)drawStatusChart(); document.querySelectorAll("canvas[data-host-history]").forEach(drawHostChart); }
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
  const target = machineApiPath(path, machineId);
  if (target !== path) {
    if (!activeMachine?.identity) throw new Error("Test this machine connection in Machines before using it");
    options = {...options, headers:{...options.headers, "X-Init-Machine-Revision":activeMachine.revision}};
  }
  const response = await fetch(target, { cache: "no-store", ...options });
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
  return api(path, { method: "POST", headers: { "Content-Type": "application/json", "X-Init-Token": sessionToken }, body: JSON.stringify(body) });
}
function showError(message) { $("error-banner").textContent = message; $("error-banner").hidden = false; }
function resourceError(message) { $("resource-error").textContent = message; $("resource-error").hidden = !message; }
function toast(message) {
  clearTimeout(toastTimer);
  $("toast").textContent = message;
  $("toast").hidden = false;
  toastTimer = setTimeout(() => { $("toast").hidden = true; }, 4500);
}
function capacity() {
  const source = state.runtime?.available && (!resourceService || resourceService.runtime === "docker") ? state.runtime : state.capacity;
  return { cpus: numeric(source?.cpus) ? source.cpus : state.budget?.cpus, memory_mb: numeric(source?.memory_mb) ? source.memory_mb : state.budget?.memory_mb };
}
function updateProjectOptions() {
  const signature = JSON.stringify(state.projects || []);
  const select = $("deploy-project");
  if (select.dataset.projects === signature) return;
  const value = select.value;
  select.replaceChildren(new Option("Default", ""), ...(state.projects || []).map(project => new Option(project.name || project.id, project.id)));
  select.value = value; select.dataset.projects = signature;
}
function renderSummary() {
  const services=servicesForProject();const ids=new Set(services.map(service=>service.id));
  $("workspace-name").textContent = state.workspace || "Not reported";
  const errors = (state.events || []).filter(event => event.ok === false && (!currentProject || ids.has(event.service))).length;
  $("nav-errors").textContent = errors ? ` ${errors} failed` : "";
  $("runtime-banner").hidden = !state.runtime?.error;
  $("runtime-banner").textContent = state.runtime?.error ? `Container engine: ${state.runtime.error}` : "";
  renderPageIdentity();
  updateProjectOptions();
  renderGithub();
  const targets = [...new Set(services.map(targetName))];
  const select = $("target-filter");
  const signature = JSON.stringify(targets);
  if (select.dataset.targets !== signature) { select.replaceChildren(new Option("All targets", ""), ...targets.map(target => new Option(target, target))); select.value = currentTarget; select.dataset.targets = signature; }
  select.value = currentTarget;
  if (select.selectedIndex < 0) { currentTarget = ""; select.value = ""; }
  $("budget-summary").textContent = `${cores(state.budget?.cpus)} cores · ${memory(state.budget?.memory_mb)}`;
  $("allocation-summary").textContent = state.allocation?.enabled ? "Automatic · weighted sharing within service ceilings" : "Manual · configured limits";
}
function usageCell(service, metric) {
  const value = metric === "cpu" ? service.cpu_percent : service.memory_mb;
  const cell = node("td", "cell-value", numeric(value) ? metric === "cpu" ? `${cores(value)}%` : memory(value) : "—");
  cell.dataset.label = metric === "cpu" ? "CPU use" : "Memory use";
  cell.title = service.telemetry_error || (service.cpu_sample_pending && metric === "cpu" ? "Collecting the first CPU interval" : metric === "cpu" ? service.cpu_source || "100% = one logical CPU" : service.memory_source || "Observed service memory");
  const canvas = node("canvas", "sparkline"); canvas.dataset.history = service.id; canvas.dataset.metric = metric; canvas.dataset.sparkline = "true"; canvas.setAttribute("aria-hidden", "true"); cell.append(canvas);
  return cell;
}
function actionButton(service, action, label) {
  const button = node("button", `button secondary${action === "stop" ? "" : ""}`, label);
  button.type = "button";
  button.dataset.service = service.id;
  button.dataset.action = action;
  const canManage = !service.observe_only && (service.enabled !== false || action === "stop") && service.capabilities?.lifecycle !== false;
  button.disabled = pending.has(service.id) || (action !== "logs" && !canManage);
  button.title = action === "logs" ? `View logs for ${service.name}` : service.observe_only ? "Managed by the host supervisor" : service.enabled === false ? "Enable this service in the manifest first" : service.capabilities?.lifecycle === false ? "This integration does not support this action" : `${label} ${service.name}`;
  button.setAttribute("aria-label", `${label} ${service.name}`);
  return button;
}
function compareServices(a, b, sort) {
  const byName = (a.name || a.id).localeCompare(b.name || b.id);
  if (sort === "running") return Number(b.status === "running") - Number(a.status === "running") || byName;
  if (sort === "name") return byName;
  const key = sort === "cpu" ? "cpu_percent" : "memory_mb";
  // Missing measurements stay last, including when a measured value is zero.
  if (!Number.isFinite(a[key])) return Number.isFinite(b[key]) ? 1 : byName;
  if (!Number.isFinite(b[key])) return -1;
  return b[key] - a[key] || byName;
}
function renderServices() {
  if (!state) return;
  const services = servicesForProject(); const query = $("search").value.trim().toLowerCase();
  const shown = services.filter(service => {
    const matches = currentFilter === "all" || (currentFilter === "job" ? service.kind === "job" : currentFilter === "attention" ? statusGroup(service)==="attention" : currentFilter === "stopped" ? ["stopped","disabled"].includes(statusGroup(service)) : service.status === currentFilter);
    return matches && (!currentTarget || targetName(service) === currentTarget) && `${service.name} ${service.id} ${service.description || ""} ${service.image || ""}`.toLowerCase().includes(query);
  });
  const sort = $("service-sort").value;
  shown.sort((a, b) => compareServices(a, b, sort));
  const fragment = document.createDocumentFragment();
  for (const service of shown) {
    const row = node("tr", eligibleService(service) ? "" : "inactive-row");
    const identity = node("td"); identity.dataset.label = "Service";
    const name = node("button", "service-name", service.name || service.id); name.type = "button"; name.dataset.service = service.id; name.dataset.action = "details"; name.setAttribute("aria-label", `View details for ${service.name || service.id}`); name.title = service.name || service.id;
    const meta = `${projectName(service.project)} · ${service.observe_only ? `${service.runtime || "host"} monitor` : service.kind || "daemon"}${service.image ? ` · ${service.image}` : ""}`;
    const sub = node("div", "service-meta", meta); sub.title = meta; identity.append(name, sub);
    const status = node("td"); status.dataset.label = "State"; status.append(node("span", `status-badge ${["running", "failed", "error", "starting"].includes(service.status) ? service.status : ""}`, service.status || "Unknown"));
    const target = node("td", "target-cell", targetName(service)); target.dataset.label = "Runs in"; target.title = `${targetName(service)}${service.execution?.target ? ` · ${service.execution.target}` : ""}`;
    const actions = node("td", "actions"); actions.dataset.label = "Control";
    if (service.observe_only) { const label = node("span", "monitor-only", "Read-only"); label.title = "Started outside Init. Usage is observed; lifecycle is managed by its supervisor."; actions.append(label); }
    else {
      actions.append(actionButton(service, service.status === "running" ? "stop" : service.kind === "job" ? "run" : "start", service.status === "running" ? "Stop" : service.kind === "job" ? "Run" : "Start"));
      if (service.status === "running" && service.kind !== "job") actions.append(actionButton(service, "restart", "Restart"));
    }
    actions.append(actionButton(service, "logs", "Logs"));
    row.append(identity, status, target, usageCell(service, "cpu"), usageCell(service, "memory"), actions); fragment.append(row);
  }
  $("service-rows").replaceChildren(fragment); $("table-count").textContent = services.length; $("shown-count").textContent = `${shown.length} of ${services.length} services`;
  $("empty-state").hidden = shown.length > 0; $("empty-state").querySelector("h2").textContent = services.length ? "No services match" : "No services yet";
  $("clear-service-filters").hidden = !services.length;
  $("empty-state").querySelector("[data-open-deploy]").hidden = services.length > 0;
  $("empty-state").querySelector("p").textContent = services.length ? "Try another target, search, or state filter." : "Deploy a service to this workspace.";
}
function statusGroup(service) {
  const status=(service.status||"").toLowerCase();
  if(["failed","error","unhealthy","dead"].includes(status)||service.health==="unhealthy")return "attention";
  if(service.enabled===false)return "disabled";
  if(["running","healthy"].includes(status))return "running";
  if(["starting","queued","pending","restarting","activating"].includes(status))return "starting";
  if(service.kind==="job"&&service.next_run)return "scheduled";
  if(["stopped","exited","inactive","not created","created","missing","paused"].includes(status))return "stopped";
  return "unknown";
}
function statusGroups(services) {
  return [["attention","Needs attention","--red"],["starting","Starting","--amber"],["running","Running","--success"],["scheduled","Scheduled","--graph-mem"],["stopped","Stopped","--muted"],["disabled","Disabled","--muted"],["unknown","Unknown","--line"]].map(([key,label,color])=>({key,label,color,count:services.filter(service=>statusGroup(service)===key).length}));
}
function drawStatusChart(){
  const canvas=$("status-chart"),box=canvas.getBoundingClientRect();if(box.width<1||box.height<1)return;
  const density=window.devicePixelRatio||1;canvas.width=Math.round(box.width*density);canvas.height=Math.round(box.height*density);
  const context=canvas.getContext("2d");if(!context)return;context.scale(density,density);
  const groups=statusGroups(servicesForProject()),total=groups.reduce((sum,group)=>sum+group.count,0),tokens=getComputedStyle(document.documentElement);
  let x=0;const count=groups.filter(group=>group.count).length;const width=box.width-Math.max(0,count-1)*3;
  if(!total){context.fillStyle=tokens.getPropertyValue("--line");context.fillRect(0,0,box.width,box.height);}
  for(const group of groups){if(!group.count)continue;const size=group.count/total*width;context.fillStyle=tokens.getPropertyValue(group.color);context.fillRect(x,0,size,box.height);x+=size+3;}
  canvas.setAttribute("aria-label",`${total} services. ${groups.map(group=>`${group.count} ${group.label.toLowerCase()}`).join(", ")}.`);
}
function renderOverview(){
  const services=servicesForProject(),usage=scopedTelemetry(services);
  document.querySelectorAll("[data-window]").forEach(button=>{const active=Number(button.dataset.window)===chartWindow;button.classList.toggle("active",active);button.setAttribute("aria-pressed",String(active));});
  for(const metric of ["cpu","memory"]){
    $("overview-"+metric).textContent=metric==="cpu"?(numeric(usage.cpu)?`${cores(usage.cpu)}%`:"—"):memory(usage.memory);
    const canvas=$("overview-chart-"+metric);canvas.dataset.history=recordedHistoryKey();
    $("overview-coverage-"+metric).textContent=usage.eligible?`Current reading · ${usage[metric+"_count"]} of ${usage.eligible} active targets reporting${usage[metric+"_complete"]?"":" · partial coverage"}`:"No active services";
    let summary=canvas.closest(".chart-card").querySelector(".history-stat");if(!summary){summary=node("p","history-stat");canvas.before(summary);}summary.dataset.historySummary=canvas.dataset.history;summary.dataset.metric=metric;summary.textContent=historyStatsText(canvas.dataset.history,metric);
  }
  $("overview-total").textContent=`${services.length} total`;
  const legend=document.createDocumentFragment();
  for(const group of statusGroups(services)){
    if(!group.count&&group.key!=="attention")continue;
    const filter={attention:"attention",running:"running",stopped:"stopped",disabled:"stopped",scheduled:"job"}[group.key];
    const item=node(filter?"button":"div","status-legend-row "+group.key+(group.count?"":" zero"));
    item.append(node("span","",group.count||group.key!=="attention"?group.label:"None need attention"),node("strong","",group.count));
    if(filter){item.type="button";item.dataset.statusFilter=filter;}
    legend.append(item);
  }$("status-legend").replaceChildren(legend);
  $("overview-control").textContent=`${services.filter(service=>!service.observe_only).length} managed · ${services.filter(service=>service.observe_only).length} read-only`;
  const locations=document.createDocumentFragment();for(const target of [...new Set(services.map(targetName))]){const members=services.filter(service=>targetName(service)===target);const button=node("button","location-link",`${target} · ${members.filter(service=>service.status==="running").length}/${members.length} running`);button.type="button";button.dataset.overviewTarget=target;locations.append(button);}$("overview-locations").replaceChildren(locations);
  const priority=service=>["attention","starting","running","scheduled","stopped","disabled","unknown"].indexOf(statusGroup(service));
  const fragment=document.createDocumentFragment();for(const service of [...services].sort((a,b)=>priority(a)-priority(b)||(a.name||a.id).localeCompare(b.name||b.id)).slice(0,8)){
    const row=node("div","overview-service");const name=node("button","service-link",service.name||service.id);name.dataset.service=service.id;name.dataset.action="details";const identity=node("div");identity.append(name,node("span","service-meta",service.image||targetName(service)));row.append(identity,node("span",`status-badge ${service.status==="running"?"running":""}`,service.status||"Unknown"),node("span","cell-value",numeric(service.cpu_percent)?`${cores(service.cpu_percent)}% CPU`:"—"));if(!service.observe_only)row.append(actionButton(service,service.status==="running"?"stop":service.kind==="job"?"run":"start",service.status==="running"?"Stop":service.kind==="job"?"Run now":"Start"));else row.append(node("span","monitor-only","Read-only"));fragment.append(row);
  }$("overview-services").replaceChildren(fragment);$("overview-empty").hidden=services.length>0;
}
document.addEventListener("click",event=>{const button=event.target.closest("[data-status-filter],[data-overview-target]");if(!button)return;currentFilter=button.dataset.statusFilter||"all";currentTarget=button.dataset.overviewTarget||"";$("search").value="";document.querySelectorAll("[data-filter]").forEach(item=>{item.classList.toggle("active",item.dataset.filter===currentFilter);item.setAttribute("aria-pressed",String(item.dataset.filter===currentFilter));});navigate("services");});
function githubWorkflow({service, branch, runner, environment, workspace, config}) {
  if(!/^[a-z][a-z0-9-]{0,47}$/.test(service))throw new Error("Choose a managed service.");
  if(!/^[A-Za-z0-9][A-Za-z0-9._\/-]{0,127}$/.test(branch)||branch.includes(".."))throw new Error("Use a branch name with letters, numbers, dots, slashes, underscores or hyphens.");
  for(const [label,value] of [["Runner label",runner],["Environment",environment]])if(!/^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$/.test(value))throw new Error(`${label} must contain letters, numbers, underscores or hyphens.`);
  for(const value of [workspace,config])if(!value||/[\r\n\0]/.test(value)||value.includes("${{"))throw new Error("Enter valid local workspace and configuration paths without GitHub expressions.");
  const quote=JSON.stringify;
  return `name: Deploy with Init
on:
  push:
    branches: [${quote(branch)}]
  workflow_dispatch:
permissions:
  contents: read
concurrency:
  group: init-${service}
  cancel-in-progress: false
jobs:
  deploy:
    if: github.ref == 'refs/heads/${branch}'
    runs-on: [self-hosted, ${quote(runner)}]
    environment: ${quote(environment)}
    timeout-minutes: 35
    env:
      INIT_WORKSPACE: ${quote(workspace)}
      INIT_CONFIG: ${quote(config)}
      INIT_SERVICE: ${quote(service)}
    steps:
      - uses: actions/checkout@v7
        with:
          persist-credentials: false
      - name: Build and deploy
        shell: bash
        run: |
          set -euo pipefail
          init --workspace "$INIT_WORKSPACE" --config "$INIT_CONFIG" \\
            deploy-image "$INIT_SERVICE" "init-$INIT_SERVICE:$GITHUB_SHA" \\
            --build-context "$GITHUB_WORKSPACE" --apply
`;
}
let generatedWorkflow="";
function renderGithub() {
  const services=(state.services||[]).filter(service=>service.runtime==="docker"&&!service.observe_only&&service.enabled!==false&&service.kind!=="job");
  const select=$("github-service"), signature=JSON.stringify(services.map(service=>[service.id,service.name]));
  if(select.dataset.services!==signature){const value=select.value;select.replaceChildren(new Option(services.length?"Choose a service":"Deploy a daemon first",""),...services.map(service=>new Option(service.name||service.id,service.id)));select.value=value;select.dataset.services=signature;}
  for(const [id,value] of [["github-workspace",state.workspace],["github-config",state.config]])if(!$(id).dataset.initialized&&value){$(id).value=value;$(id).dataset.initialized="true";}
}
function invalidateGithubWorkflow(){generatedWorkflow="";$("github-code").textContent="Settings changed. Generate a workflow to review it.";$("github-copy").disabled=$("github-download").disabled=true;}
$("github-form").addEventListener("input",invalidateGithubWorkflow);
$("github-form").addEventListener("submit",event=>{
  event.preventDefault();$("github-error").hidden=true;
  try{generatedWorkflow=githubWorkflow(Object.fromEntries(["service","branch","runner","environment","workspace","config"].map(key=>[key,$(`github-${key}`).value.trim()])));$("github-code").textContent=generatedWorkflow;$("github-copy").disabled=$("github-download").disabled=false;}catch(error){$("github-error").textContent=error.message;$("github-error").hidden=false;}
});
$("github-copy").addEventListener("click",async()=>{try{await navigator.clipboard.writeText(generatedWorkflow);toast("Workflow copied");}catch{toast("Copy unavailable. Download the workflow instead.");}});
$("github-download").addEventListener("click",()=>{if(!generatedWorkflow)return;const url=URL.createObjectURL(new Blob([generatedWorkflow],{type:"text/yaml"}));const link=node("a");link.href=url;link.download="deploy-init.yml";link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);});

function projectAction(project, action, label, className = "button secondary") {
  const button=node("button",className,label);button.type="button";button.dataset.projectAction=action;button.dataset.project=project;return button;
}
function renderWorkspaces() {
  const selected=Boolean(currentProject);$("workspace-index").hidden=selected;$("workspace-overview").hidden=!selected;
  if(!selected){
    const fragment=document.createDocumentFragment();
    for(const project of [{id:"__default__",name:"Default",description:"Services without a named workspace"},...(state.projects||[])]){
      const services=servicesForProject(project.id),usage=scopedTelemetry(services);const row=node("div","workspace-list-row");
      const identity=node("div","workspace-row-identity");const name=projectAction(project.id,"overview",project.name||project.id,"workspace-name-button");identity.append(name,node("span","muted",project.description||project.id));
      const count=node("div","workspace-row-value",`${services.filter(service=>service.status==="running").length} running`);count.append(node("span","cell-sub",`${services.length} services`));count.dataset.label="Services";
      const cpu=node("div","workspace-row-value",numeric(usage.cpu)?`${cores(usage.cpu)}%`:"—");cpu.dataset.label="Current CPU";if(numeric(usage.cpu)&&!usage.cpu_complete)cpu.append(node("span","cell-sub",`Partial ${usage.cpu_count}/${usage.eligible} targets`));
      const ram=node("div","workspace-row-value",memory(usage.memory));ram.dataset.label="Current memory";if(numeric(usage.memory)&&!usage.memory_complete)ram.append(node("span","cell-sub",`Partial ${usage.memory_count}/${usage.eligible} targets`));
      const actions=node("div","workspace-row-actions");const add=projectAction(project.id,"deploy","Add service");add.setAttribute("aria-label",`Add service to ${project.name||project.id}`);actions.append(add);row.append(identity,count,cpu,ram,actions);fragment.append(row);
    }
    $("workspace-cards").replaceChildren(fragment);return;
  }
  const services=servicesForProject(),usage=scopedTelemetry(services),managed=services.filter(service=>!service.observe_only);
  $("workspace-service-status").textContent=`${services.filter(service=>service.status==="running").length} running · ${services.length} services · ${managed.length} managed`;
  const cpuCoverage=numeric(usage.cpu)&&!usage.cpu_complete?` (partial ${usage.cpu_count}/${usage.eligible} targets)`:"";const memoryCoverage=numeric(usage.memory)&&!usage.memory_complete?` (partial ${usage.memory_count}/${usage.eligible} targets)`:"";
  $("workspace-current-usage").textContent=`Current use: ${numeric(usage.cpu)?`${cores(usage.cpu)}% CPU${cpuCoverage}`:"CPU not measured"} · ${numeric(usage.memory)?`${memory(usage.memory)} memory${memoryCoverage}`:"Memory not measured"}`;
  $("workspace-all-services").textContent=`View all ${services.length} services`;
  const fragment=document.createDocumentFragment();
  for(const service of services.slice(0,6)){
    const row=node("div","workspace-service-row");const identity=node("div","workspace-service-identity");const name=node("button","service-name",service.name||service.id);name.type="button";name.dataset.service=service.id;name.dataset.action="details";name.title=service.name||service.id;identity.append(name,node("span","cell-sub",targetName(service)));
    const status=node("span",`status-badge ${service.status==="running"?"running":""}`,service.status||"Unknown");
    const reading=node("div","workspace-service-reading",numeric(service.cpu_percent)?`${cores(service.cpu_percent)}% CPU`:"— CPU");reading.append(node("span","cell-sub",memory(service.memory_mb)));
    const action=service.observe_only?node("span","monitor-only","Monitor only"):actionButton(service,service.status==="running"?"stop":service.kind==="job"?"run":"start",service.status==="running"?"Stop":service.kind==="job"?"Run":"Start");
    row.append(identity,status,reading,action);fragment.append(row);
  }
  $("workspace-services").replaceChildren(fragment);$("workspace-services-empty").hidden=services.length>0;$("workspace-all-services").hidden=!services.length;
  const targets=[...new Set(services.map(targetName))];$("workspace-targets").replaceChildren(...(targets.length?targets.map(target=>node("p","context-value",target)):[node("p","field-help","No services deployed in this workspace yet.")]));
  $("workspace-network-description").textContent=currentProject==="__default__"?"Managed containers without a named group use the default managed network. Native services keep their host networking.":"Managed containers in this group share a private network, created on first deployment. Native services keep their host networking.";
  const networks=[...new Set(managed.filter(service=>service.runtime==="docker").map(service=>service.network).filter(Boolean))];
  $("workspace-networks").replaceChildren(...networks.map(network=>{const observed=managed.some(service=>service.actual_networks?.includes(network));const line=node("p","context-value network-value",network);line.append(node("span","cell-sub",observed?"Observed on the container engine":"Declared; not observed yet"));return line;}));
  const stored=services.filter(service=>service.workspace_storage?.persistent).length;
  $("workspace-storage-summary").textContent=stored?`${stored} service${stored===1?" has":"s have"} persistent workspace storage on its execution target.`:"No persistent service workspaces reported yet.";
  const ids=new Set(services.map(service=>service.id));const events=(state.events||[]).filter(event=>ids.has(event.service)).slice(0,4);$("workspace-recent-events").replaceChildren(eventList(events));$("workspace-no-events").hidden=events.length>0;
  const descriptor=historyDescriptor(null,currentProject,86400);
  for(const metric of ["cpu","memory"]){const canvas=$(`workspace-history-${metric}`);canvas.dataset.history=descriptor.key;let summary=canvas.closest(".chart-card").querySelector(".history-stat");if(!summary){summary=node("p","history-stat");canvas.before(summary);}summary.dataset.historySummary=descriptor.key;summary.dataset.metric=metric;summary.textContent=historyStatsText(descriptor.key,metric);}
}
function globalRoute(view) { return ["resources","settings","monitor","machines","github"].includes(view); }
function routeHref(view,service = null) {
  const prefix=!globalRoute(view)&&currentProject?`/p/${encodeURIComponent(currentProject)}`:"";
  return `#${prefix}/${view}${service?`/${encodeURIComponent(service)}`:""}`;
}
function renderPageIdentity() {
  const global=globalRoute(currentRoute),index=currentRoute==="workspaces"&&!currentProject;
  const views={workspaces:currentProject?"Overview":"Workspaces",services:"Services",overview:"Usage",activity:"Activity"};
  const globalTitles={monitor:"Host monitor",resources:"Resources",github:"GitHub setup",machines:"Machines",settings:"Settings"};
  const title=global?globalTitles[currentRoute]:scopeName();$("page-title").textContent=title;document.title=`${global?title:`${views[currentRoute]} · ${title}`} · Init`;
  const project=(state?.projects||[]).find(project=>project.id===currentProject);
  const descriptions={monitor:"Per-core CPU, memory and network on this machine. Independent of workspaces.",resources:"One resource pool on this machine, shared across all workspaces.",github:"Update a Docker service from GitHub, using a runner on this machine.",machines:"Connect to your Linux machines and Macs with SSH, optionally over Tailscale.",settings:"Controller settings for this machine and preferences for this interface."};
  $("page-description").textContent=global?descriptions[currentRoute]:currentProject==="__default__"?"Services without a named workspace.":project?.description||"";
  $("page-description").hidden=!$("page-description").textContent;
  $("scope-label").textContent=activeMachine?.profile.name||"This computer";
  $("all-workspaces-link").hidden=global||!currentProject;$("scope-tabs").hidden=global;$("tab-workspaces").textContent=views.workspaces;
  $("workspace-create").hidden=!index;document.querySelectorAll("[data-create-workspace]").forEach(button=>{button.disabled=!state;});
  document.querySelector(".header-deploy").hidden=global;
  document.querySelectorAll("[data-route]").forEach(link=>{const active=link.dataset.route===currentRoute;link.classList.toggle("active",active);if(active)link.setAttribute("aria-current","page");else link.removeAttribute("aria-current");link.href=routeHref(link.dataset.route);});
  renderWorkspaceNav();
}
let collapsedWorkspaces=new Set();
try { const saved=JSON.parse(readStoredValue(localStorage,machineStorageKey("init-collapsed-workspaces"))||"[]");if(Array.isArray(saved))collapsedWorkspaces=new Set(saved.filter(id=>typeof id==="string")); } catch { }
function persistCollapsedWorkspaces() { try { localStorage.setItem(machineStorageKey("init-collapsed-workspaces"),JSON.stringify([...collapsedWorkspaces])); } catch { } }
function workspaceNavGroups(services, projects) {
  const groups=[{id:"__default__",name:"Default",services:[]},...projects.map(project=>({id:project.id,name:project.name||project.id,services:[]}))];
  const byId=new Map(groups.map(group=>[group.id,group]));
  for(const service of services){
    const id=service.project||"__default__";
    if(!byId.has(id)){const group={id,name:id,services:[]};groups.push(group);byId.set(id,group);}
    byId.get(id).services.push(service);
  }
  for(const group of groups)group.services.sort((a,b)=>(a.name||a.id).localeCompare(b.name||b.id)||a.id.localeCompare(b.id));
  return groups;
}
function workspaceNavSignature(groups) { return JSON.stringify(groups.map(group=>[group.id,group.name,group.services.map(service=>[service.id,service.name,service.status,statusGroup(service)])])); }
let renderedWorkspaceNav="";
function buildWorkspaceNav(groups) {
  const fragment=document.createDocumentFragment();
  for(const group of groups){
    const expanded=!collapsedWorkspaces.has(group.id),listId=`nav-ws-${group.id}`;
    const item=node("li","workspace-group");item.dataset.workspace=group.id;
    const row=node("div","workspace-row");
    const toggle=node("button","workspace-toggle");toggle.type="button";toggle.dataset.workspaceToggle=group.id;toggle.dataset.navKey=`toggle:${group.id}`;toggle.setAttribute("aria-expanded",String(expanded));toggle.setAttribute("aria-controls",listId);toggle.setAttribute("aria-label",`${group.name} services`);
    toggle.innerHTML='<svg class="icon" aria-hidden="true"><use href="#ph-caret-right"/></svg>';
    const link=node("a","workspace-link");link.href=`#/p/${encodeURIComponent(group.id)}/workspaces`;link.dataset.workspaceLink=group.id;link.dataset.navKey=`workspace:${group.id}`;
    const attention=group.services.filter(service=>statusGroup(service)==="attention").length;
    const count=node("span","workspace-count");count.append(node("span","sr-only",", "),String(group.services.length),node("span","sr-only",` ${group.services.length===1?"service":"services"}`));
    link.append(node("span","workspace-label",group.name),count);
    if(attention){const alert=node("span","workspace-alert");alert.append(node("span","sr-only",`, ${attention} ${attention===1?"needs":"need"} attention`));link.append(alert);}
    row.append(toggle,link);
    const list=node("ul","workspace-services");list.id=listId;list.hidden=!expanded;
    for(const service of group.services){
      const entry=node("li"),serviceLink=node("a","service-nav-link");
      serviceLink.href=`#/p/${encodeURIComponent(group.id)}/services/${encodeURIComponent(service.id)}`;serviceLink.dataset.serviceLink=service.id;serviceLink.dataset.navKey=`service:${service.id}`;
      serviceLink.append(node("span",`service-dot ${statusGroup(service)}`),node("span","service-nav-name",service.name||service.id),node("span","sr-only",`, ${service.status||"status unknown"}`));
      entry.append(serviceLink);list.append(entry);
    }
    if(!group.services.length)list.append(node("li","tree-message","No services yet"));
    const addItem=node("li"),add=projectAction(group.id,"deploy","Add service","service-nav-add");add.dataset.navKey=`add:${group.id}`;add.setAttribute("aria-label",`Add service to ${group.name}`);add.insertAdjacentHTML("afterbegin",'<svg class="icon" aria-hidden="true"><use href="#ph-plus"/></svg>');addItem.append(add);list.append(addItem);
    item.append(row,list);fragment.append(item);
  }
  return fragment;
}
function renderWorkspaceNav() {
  const tree=$("workspace-tree"),sidebar=$("app-sidebar");
  if(state){
    const groups=workspaceNavGroups(state.services||[],state.projects||[]),signature=workspaceNavSignature(groups);
    if(signature!==renderedWorkspaceNav){
      const key=tree.contains(document.activeElement)?document.activeElement.dataset.navKey:null,scroll=[tree.scrollTop,sidebar.scrollTop];
      tree.replaceChildren(buildWorkspaceNav(groups));renderedWorkspaceNav=signature;
      [tree.scrollTop,sidebar.scrollTop]=scroll;
      if(key)[...tree.querySelectorAll("[data-nav-key]")].find(element=>element.dataset.navKey===key)?.focus({preventScroll:true});
    }
  }
  const scoped=!globalRoute(currentRoute),selectedService=currentRoute==="services"?decodeURIComponent(location.hash.match(/\/services\/([^/]+)$/)?.[1]||""):"";
  const all=$("all-workspaces-nav"),allCurrent=scoped&&!currentProject;all.classList.toggle("active",allCurrent);if(allCurrent)all.setAttribute("aria-current","true");else all.removeAttribute("aria-current");
  for(const link of tree.querySelectorAll("[data-workspace-link]")){const current=scoped&&link.dataset.workspaceLink===currentProject;link.parentElement.classList.toggle("current",current);if(current)link.setAttribute("aria-current","true");else link.removeAttribute("aria-current");}
  for(const link of tree.querySelectorAll("[data-service-link]")){const current=Boolean(selectedService)&&link.dataset.serviceLink===selectedService;link.classList.toggle("current",current);if(current)link.setAttribute("aria-current","true");else link.removeAttribute("aria-current");}
}
function setWorkspaceExpanded(id, expanded) {
  if(expanded===!collapsedWorkspaces.has(id))return;
  if(expanded)collapsedWorkspaces.delete(id);else collapsedWorkspaces.add(id);persistCollapsedWorkspaces();
  const toggle=[...$("workspace-tree").querySelectorAll("[data-workspace-toggle]")].find(button=>button.dataset.workspaceToggle===id);
  if(toggle){toggle.setAttribute("aria-expanded",String(expanded));$(toggle.getAttribute("aria-controls")).hidden=!expanded;}
}
let renderedScope=null;
function syncScope() {
  if(currentProject===renderedScope)return;
  if(renderedScope!==null)currentTarget="";
  renderedScope=currentProject;
  if(currentProject)setWorkspaceExpanded(currentProject,true);
}
$("workspace-tree").addEventListener("click",event=>{const toggle=event.target.closest("[data-workspace-toggle]");if(toggle)setWorkspaceExpanded(toggle.dataset.workspaceToggle,toggle.getAttribute("aria-expanded")!=="true");});
function setNavigationOpen(open) {
  const sidebar=$("app-sidebar");
  document.body.classList.toggle("nav-open",open);$("nav-scrim").hidden=!open;$("nav-toggle").setAttribute("aria-expanded",String(open));
  for(const element of [$("main"),document.querySelector(".mobile-bar"),document.querySelector(".skip-link")])element.inert=open;
  if(open){sidebar.setAttribute("role","dialog");sidebar.setAttribute("aria-modal","true");(navigationFocusables().find(element=>element.hasAttribute("aria-current"))||$("nav-close")).focus();}
  else{sidebar.removeAttribute("role");sidebar.removeAttribute("aria-modal");}
}
function closeNavigation() { setNavigationOpen(false);$("nav-toggle").focus(); }
function navigationFocusables() { return [...$("app-sidebar").querySelectorAll("a[href],button:not(:disabled),select:not(:disabled),summary")].filter(element=>element.getClientRects().length); }
$("nav-toggle").addEventListener("click",()=>setNavigationOpen(true));
$("nav-close").addEventListener("click",closeNavigation);
$("nav-scrim").addEventListener("click",closeNavigation);
document.addEventListener("keydown",event=>{
  // A native dialog opened from the drawer manages its own focus.
  if(!document.body.classList.contains("nav-open")||document.querySelector("dialog[open]"))return;
  if(event.key==="Escape"){event.preventDefault();closeNavigation();return;}
  if(event.key!=="Tab")return;
  const items=navigationFocusables(),first=items[0],last=items[items.length-1],inside=$("app-sidebar").contains(document.activeElement);
  if(!first)return;
  if(event.shiftKey&&(!inside||document.activeElement===first)){event.preventDefault();last.focus();}
  else if(!event.shiftKey&&(!inside||document.activeElement===last)){event.preventDefault();first.focus();}
});
$("app-sidebar").addEventListener("click",event=>{
  const link=event.target.closest("a[href]");if(!link||!document.body.classList.contains("nav-open"))return;
  const sameHash=new URL(link.href).hash===location.hash;setNavigationOpen(false);
  // Same-hash clicks emit no hashchange, so place focus in the destination here.
  if(sameHash)$("page-title").focus({preventScroll:true});
});
matchMedia("(min-width: 1024px)").addEventListener("change",event=>{if(event.matches&&document.body.classList.contains("nav-open"))setNavigationOpen(false);});

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
  const services = servicesForProject();
  const signature = services.map(service => `${service.id}:${service.name}`).join("|");
  if (filter.dataset.services !== signature) {
    const previous = filter.value;
    const options = [new Option("All services", ""), ...services.map(service => new Option(service.name || service.id, service.id))];
    filter.replaceChildren(...options);
    filter.value = previous;
    if (filter.selectedIndex < 0) filter.value = "";
    filter.dataset.services = signature;
  }
  const ids = new Set(services.map(service => service.id));
  const events = (state.events || []).filter(event => (!currentProject || ids.has(event.service)) && (!filter.value || event.service === filter.value));
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
    identity.append(name, node("span", "cell-sub", `${projectName(service.project)} · ${targetName(service)}`));
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
  $("details-subtitle").textContent = `${service.id}${activeMachine ? ` · ${activeMachine.profile.name}` : ""}`;
  const overview = node("dl", "details-grid");
  detailField(overview, "State", service.status || "Unknown");
  detailField(overview, "Runs in", targetName(service));
  detailField(overview, "Runtime", service.runtime);
  detailField(overview, "Workspace group", projectName(service.project));
  detailField(overview, "Execution target", service.execution?.target || "Not reported", true);
  if (service.container_id) detailField(overview, "Container ID", service.container_id, true);
  if (service.started_at) detailField(overview, "Started", service.started_at);
  if (numeric(service.restart_count)) detailField(overview, "Restarts", service.restart_count);
  if (service.actual_networks?.length) detailField(overview, "Observed networks", service.actual_networks.join(", "), true);
  if (service.image) detailField(overview, "Image", service.image, true);
  if (service.command?.length) detailField(overview, "Command arguments", service.command.join("\n"), true);
  if (service.ports?.length) detailField(overview, "Published ports", service.ports.join("\n"), true);
  if (service.env_file) detailField(overview, "Environment file", service.env_file, true);
  if (numeric(service.pid)) detailField(overview, `PID (${service.execution?.pid_namespace || "service host"})`, service.pid);
  const storage = service.workspace_storage;
  detailField(overview, "Service storage", storage ? `${storage.type === "volume" ? "Persistent volume" : "Bind mount"} · ${storage.source} → ${storage.mount_path}` : service.observe_only ? "No private workspace reported" : "Container filesystem", true);
  if (storage) detailField(overview, "Storage location", `${storage.location}. Existing data is retained on reuse.`, true);
  if (service.actual_working_dir || service.working_dir) detailField(overview, "Working directory", service.actual_working_dir || service.working_dir, true);
  if (service.actual_mounts?.length) detailField(overview, "Observed mounts", service.actual_mounts.map(mount=>`${mount.name || mount.source} → ${mount.target}${mount.read_only ? " (read only)" : ""}`).join("\n"), true);
  detailField(overview, "Control", service.observe_only ? "Read-only" : service.enabled === false ? "Disabled in manifest" : "Managed");
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
  const graphs = node("div", "detail-graphs");
  for (const metric of ["cpu","memory"]) { const card = node("section", "chart-card"); card.append(node("h3", "", metric === "cpu" ? "CPU in use · 100% = one logical CPU" : "Memory in use")); const canvas = node("canvas"); canvas.dataset.history=recordedHistoryKey(service.id,null,chartWindow); canvas.dataset.metric=metric; canvas.tabIndex=0; const cap=metric==="cpu" ? service.actual_limits?.cpus : service.actual_limits?.memory_mb; if (numeric(cap) && cap>0) canvas.dataset.limit=metric==="cpu" ? cap*100 : cap; canvas.setAttribute("aria-label", metric === "cpu" ? "Service CPU history, use left and right arrows to inspect samples" : "Service memory history, use left and right arrows to inspect samples"); const samples = node("table", "chart-samples"); samples.append(node("caption", "", `${metric} samples`), node("tbody")); const sampleWrapper=node("div","sr-only");sampleWrapper.append(samples);const stats=node("p","history-stat",historyStatsText(canvas.dataset.history,metric));stats.dataset.historySummary=canvas.dataset.history;stats.dataset.metric=metric;card.append(stats,canvas,node("output","chart-readout"),sampleWrapper); graphs.append(card); }
  const detailStatus=node("p","history-note",historyStatusText(historyDescriptor(service.id,null,chartWindow)));detailStatus.id="detail-history-status";graphs.append(detailStatus);$("detail-usage-content").replaceChildren(graphs);
  $("detail-tab-resources").hidden=service.observe_only;
  const resources = node("div");
  resources.append(node("p", "detail-resource-note", service.observe_only ? "This service is monitored through its host supervisor. Workspace allocation does not control its resource limits." : state.allocation?.enabled ? "Automatic allocation shares the global resource budget between service minimums and ceilings. Runtime limits show the latest reported enforcement." : "Manual mode uses the declared ceiling as the effective limit. Runtime limits show the latest reported enforcement."));
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
function updateLogOutput(output, text) {
  const pinned = output.scrollHeight - output.scrollTop - output.clientHeight < 36;
  const top = output.scrollTop;
  const selection = window.getSelection();
  if (selection && !selection.isCollapsed && output.contains(selection.anchorNode)) return;
  if (output.textContent !== text) output.textContent = text;
  output.scrollTop = pinned ? output.scrollHeight : top;
}
function liveLogStatus(id, message) { $(id).textContent = message; }
async function loadDetailLogs(quiet = false) {
  const id = detailsServiceId;
  if (!id || detailsLogsLoading === id) return;
  const request = ++detailsLogRequest;
  detailsLogsLoading = id;
  liveLogStatus("detail-logs-status", "Updating…");
  if (!quiet) $("detail-logs-output").textContent = "Loading…";
  try {
    const result = await api(`/api/services/${encodeURIComponent(id)}/logs?tail=200`);
    if (request !== detailsLogRequest || !$("details-dialog").open || detailsServiceId !== id) return;
    updateLogOutput($("detail-logs-output"), result.logs || "No output recorded for this service.");
    liveLogStatus("detail-logs-status", "Live · updates automatically");
  } catch (error) {
    if (request !== detailsLogRequest) return;
    liveLogStatus("detail-logs-status", "Connection interrupted · retrying");
    if (!quiet) $("detail-logs-output").textContent = `Unable to load logs: ${error.message}`;
  } finally { if (request === detailsLogRequest) detailsLogsLoading = null; }
}
function selectDetailTab(name, focus = false) {
  detailsTab = name;
  document.querySelectorAll("[data-detail-tab]").forEach(button => {
    const active = button.dataset.detailTab === name;
    button.setAttribute("aria-selected", String(active)); button.tabIndex = active ? 0 : -1;
    $(`detail-${button.dataset.detailTab}`).hidden = !active;
    if (active && focus) button.focus();
  });
  if (name === "logs") loadDetailLogs();
  renderUsageCharts();
  if(name==="usage")synchronizeHistory(true);
}
function openDetails(service) {
  detailsServiceId = service.id;
  $("details-dialog").showModal();
  selectDetailTab("overview");
  renderDetails();
}
function workspaceTableFocus(active) { return active?.dataset?.projectAction&&active.closest("#workspace-cards")?{project:active.dataset.project,action:active.dataset.projectAction}:null; }
function renderState() {
  syncScope();
  const active = document.activeElement;
  const scope = active?.closest("#service-rows, #overview-services, #workspace-services, #resource-allocation-rows, #details-dialog");
  const focus = scope && active.dataset.service ? { scope: scope.id, service: active.dataset.service, action: active.dataset.action } : null;
  const projectFocus=workspaceTableFocus(active);
  const chartFocus=active?.matches("canvas[data-history]")?{history:active.dataset.history,metric:active.dataset.metric,container:active.closest("dialog")?.id||"main",selectedAt:graphSelection.get(`${active.dataset.history}|${active.dataset.metric}`)}:null;
  renderHostMonitor(state.host_metrics, activeMachine?.profile.name || "This computer"); renderSummary(); renderOverview(); renderServices(); renderResources(); renderEvents(); renderWorkspaces(); renderDetails(); renderSettings(); renderRecordedHistoryStatus(); renderUsageCharts();
  if(chartFocus&&!active.isConnected){const target=[...$(chartFocus.container).querySelectorAll("canvas[data-history]")].find(canvas=>canvas.tabIndex>=0&&canvas.dataset.history===chartFocus.history&&canvas.dataset.metric===chartFocus.metric);if(target){if(numeric(chartFocus.selectedAt))graphSelection.set(`${chartFocus.history}|${chartFocus.metric}`,chartFocus.selectedAt);target.focus({preventScroll:true});drawUsageChart(target);}}
  if (focus && !active.isConnected) {
    const buttons = [...$(focus.scope).querySelectorAll("button[data-service]")];
    const target = buttons.find(button => button.dataset.service === focus.service && button.dataset.action === focus.action && !button.disabled)
      || buttons.find(button => button.dataset.service === focus.service && button.dataset.action === "details" && !button.disabled);
    target?.focus({ preventScroll: true });
  }
  if(projectFocus&&!active.isConnected){const target=[...$("workspace-cards").querySelectorAll("button[data-project-action]")].find(button=>button.dataset.project===projectFocus.project&&button.dataset.projectAction===projectFocus.action);target?.focus({preventScroll:true});}
  synchronizeHistory();
  if(currentRoute==="settings")loadSettings();
}
async function fetchState() {
  try {
    await session();
    state = await api("/api/state");
    document.body.dataset.connection="connected";
    lastStateAt = Date.now();
    recordUsage();
    renderState();
    const routeParts=location.hash.replace(/^#\/?/,"").split("/").filter(Boolean);if(routeParts[0]==="p")routeParts.splice(0,2);
    if (routeParts[0] === "services" && routeParts[1] && !$("details-dialog").open) { const selected = servicesForProject().find(service => service.id === decodeURIComponent(routeParts[1])); if (selected) openDetails(selected); }
    updateFreshness();
    $("error-banner").hidden = true;
    if (state.manifest_error) showError(`Manifest: ${state.manifest_error}. Showing the last valid configuration.`);
    $("budget-edit").disabled = false;
    document.querySelectorAll("[data-open-deploy]").forEach(button => { button.disabled = false; });
  } catch (error) {
    document.body.dataset.connection="interrupted";
    const previous = lastStateAt ? ` Showing data from ${new Date(lastStateAt).toLocaleTimeString()}.` : "";
    showError(`${error.message}.${previous} Retrying automatically.`);
  } finally { updateFreshness(); }
}
function refresh(force = false) {
  if (!machinesReady) return Promise.resolve();
  if (refreshPromise) return force ? refreshPromise.then(() => refresh(false)) : refreshPromise;
  refreshPromise = fetchState().finally(() => { refreshPromise = null; });
  return refreshPromise;
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
  $("resources-title").textContent = `${service ? service.name : "Resource budget"}${activeMachine ? ` · ${activeMachine.profile.name}` : ""}`;
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
  if (action === "details") { navigate("services", service.id); return; }
  if (action === "resources") { if ($("details-dialog").open) $("details-dialog").close(); openResources(service); return; }
  if (action === "logs") {
    navigate("services", service.id);
    // Apply the route before choosing a tab; hashchange is asynchronous.
    showRoute();
    selectDetailTab("logs");
    return;
  }
  pending.add(service.id); renderState();
  try {
    await post(`/api/services/${encodeURIComponent(service.id)}/${action}`);
    toast(`${service.name}: ${action} completed`);
    await refresh();
  } catch (error) { showError(error.message); }
  finally { pending.delete(service.id); renderState(); }
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
document.querySelectorAll(".close-dialog").forEach((button) => button.addEventListener("click", () => { if (!resourceApplying && !deployApplying && !projectApplying) button.closest("dialog").close(); }));
document.querySelectorAll("dialog").forEach((dialog) => {
  dialog.addEventListener("cancel", (event) => { if (resourceApplying || deployApplying || projectApplying) event.preventDefault(); });
  dialog.addEventListener("click", (event) => { if (event.target === dialog && !resourceApplying && !deployApplying && !projectApplying) { const rect = dialog.getBoundingClientRect(); if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) dialog.close(); } });
  dialog.addEventListener("close", () => { previewRequest++; if (dialog.id === "details-dialog") { const closedId=detailsServiceId; detailsLogRequest++; detailsServiceId = null; detailsLogsLoading = null; if(currentRoute==="services" && closedId && location.hash.endsWith(`/`+encodeURIComponent(closedId)))navigate("services"); } });
});
document.querySelectorAll("[data-filter]").forEach((button) => button.addEventListener("click", () => {
  currentFilter = button.dataset.filter;
  document.querySelectorAll("[data-filter]").forEach((item) => { item.classList.toggle("active", item === button); item.setAttribute("aria-pressed", String(item === button)); });
  renderServices();
}));
$("service-sort").addEventListener("change", () => { renderServices(); renderUsageCharts(); });
$("clear-service-filters").addEventListener("click", () => {
  currentFilter = "all"; currentTarget = ""; $("search").value = ""; $("target-filter").value = "";
  document.querySelectorAll("[data-filter]").forEach(button => {
    const active = button.dataset.filter === "all";
    button.classList.toggle("active", active); button.setAttribute("aria-pressed", String(active));
  });
  renderServices(); renderUsageCharts(); $("search").focus();
});
$("search").addEventListener("input", () => { renderServices(); renderUsageCharts(); });
$("target-filter").addEventListener("change", () => { currentTarget=$("target-filter").value; renderServices(); renderUsageCharts(); });
$("activity-filter").addEventListener("change", renderEvents);
document.addEventListener("click",event=>{const button=event.target.closest("button[data-window]");if(button)setHistoryWindow(Number(button.dataset.window));const hostButton=event.target.closest("button[data-host-window]");if(hostButton){hostChartWindow=Number(hostButton.dataset.hostWindow);document.querySelectorAll("[data-host-window]").forEach(item=>{const active=item===hostButton;item.classList.toggle("active",active);item.setAttribute("aria-pressed",String(active));});renderUsageCharts();}});
document.querySelectorAll("[data-detail-tab]").forEach(button => {
  button.addEventListener("click", () => selectDetailTab(button.dataset.detailTab));
  button.addEventListener("keydown", event => {
    const names = [...document.querySelectorAll("[data-detail-tab]")].filter(tab=>!tab.hidden).map(tab=>tab.dataset.detailTab);
    const index = names.indexOf(button.dataset.detailTab);
    const next = event.key === "ArrowRight" ? (index + 1) % names.length : event.key === "ArrowLeft" ? (index + names.length - 1) % names.length : event.key === "Home" ? 0 : event.key === "End" ? names.length - 1 : -1;
    if (next >= 0) { event.preventDefault(); selectDetailTab(names[next], true); }
  });
});
let controllerSettings=null;
let settingsLoading=false;
let settingsRequestedAt=0;
let settingsError=null;
let settingsDirty=false;
let settingsApplying=false;
function settingsField(grid,label,value) {
  const term=node("dt","",label),description=node("dd","",value??"Not reported");grid.append(term,description);
}
function renderSettings() {
  if(!$("view-settings"))return;
  if(document.activeElement!==$("settings-theme"))$("settings-theme").value=document.documentElement.dataset.theme==="light"?"light":"dark";
  if(document.activeElement!==$("settings-update-interval"))$("settings-update-interval").value=String(pollIntervalMs);
  $("settings-status").textContent=settingsError?`Controller settings unavailable: ${settingsError}. Retrying automatically.`:controllerSettings?`Controller ${controllerSettings.version||""}${settingsLoading?" · checking latest settings…":""}`:"Loading controller settings…";
  if(controllerSettings&&!settingsDirty&&!settingsApplying)$("settings-max-jobs").value=controllerSettings.controller.max_parallel_jobs;
  $("settings-max-jobs").disabled=!controllerSettings||settingsApplying;$("save-controller-settings").disabled=!controllerSettings||settingsApplying||!settingsDirty;
  const history=$("settings-history-details");history.replaceChildren();
  if(controllerSettings){const recorded=controllerSettings.history;settingsField(history,"Status",recorded.status||"Not reported");if(recorded.error)settingsField(history,"Collection error",recorded.error);settingsField(history,"Retention",numeric(recorded.retention_seconds)?`${recorded.retention_seconds/3600} hours`:"Not reported");settingsField(history,"Capture interval",numeric(recorded.sample_interval_seconds)?`${recorded.sample_interval_seconds} seconds`:"Not reported");settingsField(history,"Stored bucket size",numeric(recorded.bucket_seconds)?`${recorded.bucket_seconds} seconds`:"Not reported");settingsField(history,"Earliest available capture",numeric(recorded.earliest_at)?fullTime(recorded.earliest_at):"No measurements recorded yet");settingsField(history,"Latest available capture",numeric(recorded.latest_at)?fullTime(recorded.latest_at):"No measurements recorded yet");}
  const runtime=$("settings-runtime-details");runtime.replaceChildren();settingsField(runtime,"Container engine location",state?.runtime?.execution?.label||"Not reported");settingsField(runtime,"Container engine",state?.runtime?.available?"Connected":state?.runtime?.error||"Not available");if(state?.runtime?.context)settingsField(runtime,"Engine context",state.runtime.context);settingsField(runtime,"Engine capacity",state?.runtime?.available?`${cores(state.runtime.cpus)} cores · ${memory(state.runtime.memory_mb)}`:"Not reported");settingsField(runtime,"Resource enforcement","Managed Docker containers and opted-in Linux systemd units can enforce limits. Existing Mac launchd services are monitor-only.");
  const paths=$("settings-path-details");paths.replaceChildren();if(controllerSettings?.paths)for(const [key,label] of [["workspace","Controller workspace"],["config","Configuration"],["state","Controller state"],["history","Recorded utilization"]])settingsField(paths,label,controllerSettings.paths[key]);
  const config=controllerSettings?.paths?.config,workspace=controllerSettings?.paths?.workspace;const quoted=`${workspace?` --workspace ${shellQuote(workspace)}`:""}${config?` --config ${shellQuote(config)}`:""}`;
  $("settings-cli-location").textContent=machineId?`Run these commands on ${activeMachine?.profile.name||"the selected machine"}. Paths refer to that remote controller; these commands do not run through SSH automatically.`:"Run these commands on this computer with the controller workspace shown above.";
  $("settings-cli-example").textContent=`init${quoted} deploy worker.yaml\ninit${quoted} deploy worker.yaml --apply\ninit${quoted} tui`;
}
function shellQuote(value) { return `'${String(value).replace(/'/g,"'\\''")}'`; }
async function loadSettings(force = false) {
  if(settingsLoading||(!force&&Date.now()-settingsRequestedAt<HISTORY_REFRESH_MS))return;
  settingsLoading=true;settingsRequestedAt=Date.now();settingsError=null;renderSettings();
  try{const result=await api("/api/settings");if(!result.controller||!result.history||!result.paths)throw new Error("Invalid controller settings response");controllerSettings=result;}catch(error){settingsError=error.message;}finally{settingsLoading=false;renderSettings();}
}
$("settings-max-jobs").addEventListener("input",()=>{settingsDirty=true;$("settings-error").hidden=true;$("settings-saved-note").textContent="Unsaved controller change";$("save-controller-settings").disabled=settingsApplying;});
$("settings-controller-form").addEventListener("submit",async event=>{
  event.preventDefault();if(settingsApplying||!controllerSettings||!$("settings-controller-form").reportValidity())return;
  const maximum=Number($("settings-max-jobs").value);if(!Number.isInteger(maximum)||maximum<1||maximum>256){$("settings-error").textContent="Choose a whole number from 1 to 256.";$("settings-error").hidden=false;return;}
  settingsApplying=true;$("settings-error").hidden=true;$("save-controller-settings").textContent="Saving…";renderSettings();
  try{const result=await post("/api/settings",{max_parallel_jobs:maximum});controllerSettings.controller=result.controller;settingsDirty=false;$("settings-saved-note").textContent=result.message||"Controller setting saved";toast("Controller job limit saved");refresh(true);}catch(error){$("settings-error").textContent=error.message;$("settings-error").hidden=false;}finally{settingsApplying=false;$("save-controller-settings").textContent="Save changes";renderSettings();}
});
$("settings-theme").addEventListener("change",()=>{applyTheme($("settings-theme").value);try{localStorage.setItem("init-theme-v3",$("settings-theme").value);}catch{}});
$("settings-update-interval").addEventListener("change",()=>{const value=Number($("settings-update-interval").value);if(![5000,10000,30000].includes(value))return;pollIntervalMs=value;try{localStorage.setItem("init-update-ms",String(value));}catch{}restartPolling();renderUsageCharts();toast(`Live updates every ${value/1000} seconds`);});
document.querySelectorAll(".settings-nav a").forEach(link=>link.addEventListener("click",event=>{event.preventDefault();document.querySelector(link.getAttribute("href"))?.scrollIntoView({block:"start",behavior:"smooth"});}));

function persistProject() { try { localStorage.setItem(machineStorageKey("init-project"),currentProject); } catch { /* The selection still works in this window. */ } }
function navigate(view, service = null) { const next=routeHref(view,service);if(location.hash===next)showRoute();else location.hash=next; }
let lastRouteGlobal=null;
function showRoute(event) {
  const parts=location.hash.replace(/^#\/?/,"").split("/").filter(Boolean);const explicitProject=parts[0]==="p";
  if(explicitProject){currentProject=decodeURIComponent(parts[1]||"");parts.splice(0,2);}
  if(parts[0]==="events"){parts[0]="activity";history.replaceState(null,"",`#${currentProject?`/p/${encodeURIComponent(currentProject)}`:""}/${parts.join("/")}`);}
  currentRoute=["overview","services","monitor","resources","activity","workspaces","github","machines","settings"].includes(parts[0])?parts[0]:"workspaces";
  if(!explicitProject&&!globalRoute(currentRoute))currentProject="";
  syncScope();
  const global=globalRoute(currentRoute);if(global!==lastRouteGlobal){$("machine-tools").open=global;lastRouteGlobal=global;}
  if(document.body.classList.contains("nav-open"))setNavigationOpen(false);
  document.querySelectorAll(".view").forEach(view=>{view.hidden=view.id!==`view-${currentRoute}`;});
  renderPageIdentity();persistProject();
  if(state){renderState();const selected=currentRoute==="services"&&parts[1]?servicesForProject().find(service=>service.id===decodeURIComponent(parts[1])):null;if(selected){if(!$("details-dialog").open)openDetails(selected);else{if(detailsServiceId!==selected.id){detailsLogRequest++;detailsLogsLoading=null;$("detail-logs-output").textContent="Loading logs…";$("detail-logs-status").textContent="Loading…";}detailsServiceId=selected.id;renderDetails();if(detailsTab==="logs")loadDetailLogs();}}else if($("details-dialog").open)$("details-dialog").close();}
  if(event&&!document.querySelector("dialog[open]"))$("page-title").focus({preventScroll:true});window.scrollTo({top:0,behavior:"instant"});renderUsageCharts();synchronizeHistory();if(currentRoute==="settings")loadSettings();
}
window.addEventListener("hashchange",showRoute);
let chordAt=0;
document.addEventListener("keydown",event=>{ if(event.ctrlKey||event.metaKey||event.altKey||event.target.closest("input,textarea,select,[contenteditable=true]")||document.querySelector("dialog[open]"))return; if(event.key==="/"){event.preventDefault();navigate("services");$("search").focus();return;} const key=event.key.toLowerCase(); if(key==="g"){chordAt=Date.now();return;} if(Date.now()-chordAt<1200){const route={o:"overview",s:"services",m:"monitor",r:"resources",e:"activity",a:"activity",w:"workspaces",h:"github",c:"settings"}[key];if(route){event.preventDefault();navigate(route);}chordAt=0;} });
$("budget-edit").addEventListener("click", () => { if (state) openResources(null); });
const deployFieldIds = ["deploy-source","deploy-repository","deploy-reference","deploy-context","deploy-image","deploy-id","deploy-name","deploy-project","deploy-kind","deploy-interval","deploy-cpu","deploy-memory","deploy-command","deploy-ports","deploy-env","deploy-weight","deploy-min-cpu","deploy-min-memory"];
function syncPortDraft(){
  $("deploy-ports").value=[...$("deploy-port-rows").children].map(row=>`127.0.0.1:${row.querySelector(".host-port").value}:${row.querySelector(".container-port").value}${row.querySelector("select").value==="udp"?"/udp":""}`).join("\n");
  invalidateDeployment();
}
function addPortRow(host="",container="",protocol="tcp"){
  if($("deploy-port-rows").children.length>=32)return;
  const row=node("div","port-row");
  for(const [label,key,value] of [["Computer port","host-port",host],["Service port","container-port",container]]){const field=node("label","",label);const input=node("input",key);input.type="number";input.min=1;input.max=65535;input.step=1;input.required=true;input.value=value;field.append(input);row.append(field);}
  const field=node("label","","Protocol"),select=node("select");select.append(new Option("TCP","tcp"),new Option("UDP","udp"));select.value=protocol;field.append(select);row.append(field);
  const remove=node("button","button secondary","Remove");remove.type="button";remove.addEventListener("click",()=>{row.remove();syncPortDraft();});row.append(remove);row.addEventListener("input",syncPortDraft);$("deploy-port-rows").append(row);
}
function restorePortRows(){
  const lines=$("deploy-ports").value.split(/\r?\n/).map(line=>line.trim()).filter(Boolean);const parsed=lines.map(line=>line.match(/^127\.0\.0\.1:(\d+):(\d+)(?:\/(tcp|udp))?$/));
  $("deploy-port-rows").replaceChildren();const legacy=parsed.some(value=>!value);$("deploy-ports").hidden=!legacy;$("deploy-port-rows").hidden=legacy;$("add-port").hidden=legacy;
  if(!legacy)for(const match of parsed)addPortRow(match[1],match[2],match[3]||"tcp");
}
$("add-port").addEventListener("click",()=>{addPortRow();syncPortDraft();});
function mountRows() { return [...$("deploy-mounts").querySelectorAll(".mount-row")]; }
function addMountRow(value = {}) {
  if (mountRows().length >= 32) return;
  const row=node("div","mount-row");
  for(const [key,label,placeholder] of [["source","Source","Volume name or engine host path"],["target","Container path","/data"]]){ const field=node("label","",label);const input=node("input",`mount-${key}`);input.value=value[key]||"";input.placeholder=placeholder;input.maxLength=1024;field.append(input);row.append(field); }
  const read=node("label","check-label","Read only");const checkbox=node("input","mount-read-only");checkbox.type="checkbox";checkbox.checked=Boolean(value.read_only);read.prepend(checkbox);row.append(read);
  const remove=node("button","button secondary","Remove");remove.type="button";remove.addEventListener("click",()=>{row.remove();invalidateDeployment();});row.append(remove);$("deploy-mounts").append(row);
}
function deploymentDraft() {
  return {fields:Object.fromEntries(deployFieldIds.map(id=>[id,$(id).value])),workspace:document.querySelector('input[name="deploy-workspace"]:checked').value,mounts:mountRows().map(row=>({source:row.querySelector(".mount-source").value,target:row.querySelector(".mount-target").value,read_only:row.querySelector(".mount-read-only").checked}))};
}
function saveDeploymentDraft(){ try{sessionStorage.setItem(machineStorageKey("init-deploy-draft"),JSON.stringify(deploymentDraft()));}catch{/* The draft remains in the open form. */} }
function deploymentPayload() {
  const lines=id=>$(id).value.split(/\r?\n/).map(line=>line.trim()).filter(Boolean);
  return {id:$("deploy-id").value.trim(),name:$("deploy-name").value.trim(),image:$("deploy-image").value.trim(),project:$("deploy-project").value||null,kind:$("deploy-kind").value,limits:{cpus:Number($("deploy-cpu").value),memory_mb:Number($("deploy-memory").value)},workspace:document.querySelector('input[name="deploy-workspace"]:checked').value==="persistent",command:$("deploy-command").value.split(/\r?\n/).filter(line=>line.length>0),ports:lines("deploy-ports"),env_file:$("deploy-env").value.trim()||null,volumes:mountRows().map(row=>({source:row.querySelector(".mount-source").value.trim(),target:row.querySelector(".mount-target").value.trim(),read_only:row.querySelector(".mount-read-only").checked})),interval_seconds:$("deploy-kind").value==="job"?Number($("deploy-interval").value):null,allocation:{weight:Number($("deploy-weight").value),min_cpus:Number($("deploy-min-cpu").value),min_memory_mb:Number($("deploy-min-memory").value)}};
}
function deploymentBody(apply, service = deploymentPayload()) {
  const body = {apply, service};
  if ($("deploy-source").value === "github") body.github = {repository:$("deploy-repository").value.trim(),reference:$("deploy-reference").value.trim() || null,context:$("deploy-context").value.trim() || "."};
  return body;
}
function syncDeploymentSource() {
  const github = $("deploy-source").value === "github";
  $("deploy-image-fields").hidden = github; $("deploy-image").disabled = github; $("deploy-image").required = !github;
  $("deploy-github-fields").hidden = !github;
  for (const id of ["deploy-repository", "deploy-reference", "deploy-context"]) $(id).disabled = !github;
  $("deploy-repository").required = github;
}
function deploymentSignature(){return JSON.stringify(deploymentBody(false));}
function deploymentError(message){$("deploy-error").textContent=message;$("deploy-error").hidden=!message;}
function updateDeploymentApply(){ $("apply-deployment").disabled=deployApplying||deployPreviewSignature!==deploymentSignature()||(deployNeedsReuse&&!$("reuse-workspace").checked); }
function invalidateDeployment(){deployRequest++;if(!deployApplying)$("preview-deployment").disabled=false;deployPreviewSignature=null;$("deploy-preview").hidden=true;$("reuse-workspace-field").hidden=true;$("reuse-workspace").checked=false;deployNeedsReuse=false;$("deploy-preview-note").textContent="Settings changed. Check again before deploying.";$("deploy-defaults").textContent=`${cores(Number($("deploy-cpu").value))} cores · ${memory(Number($("deploy-memory").value))}`;$("deploy-target").textContent=`Into ${projectName($("deploy-project").value)} on ${activeMachine?.profile.name || "this computer"} · ${state?.runtime?.execution?.label||state?.runtime?.server_name||"Configured Docker engine"}`;deploymentError("");saveDeploymentDraft();updateDeploymentApply();}
function defaultDeploymentResources(){
  const capacity=state.runtime?.available?state.runtime:state.capacity;
  const cpu=Math.max(.01,Math.min(.25,state.budget.cpus,capacity?.cpus??.25));
  const ram=Math.max(6,Math.min(256,Math.floor(state.budget.memory_mb),capacity?.memory_mb??256));
  $("deploy-cpu").value=cpu;$("deploy-memory").value=ram;
  $("deploy-min-cpu").value=Math.min(.1,cpu);$("deploy-min-memory").value=Math.min(64,ram);
}
function configureDeploymentRanges(){
  const capacity=state.runtime?.available?state.runtime:state.capacity;const cpuMax=numeric(capacity?.cpus)?Math.max(.01,capacity.cpus):65536;const memoryMax=numeric(capacity?.memory_mb)?Math.max(6,Math.floor(capacity.memory_mb)):1099511627776;
  for(const [key,max] of [["cpu",cpuMax],["memory",memoryMax]]){$(`deploy-${key}`).max=$(`deploy-${key}-range`).max=max;$(`deploy-${key}-range`).value=$(`deploy-${key}`).value;}
  document.querySelectorAll("[data-size]").forEach(button=>{const size=deploymentSizes[button.dataset.size];button.disabled=size[0]>cpuMax||size[1]>memoryMax;});
  $("deploy-capacity").textContent=`Engine capacity: ${cores(capacity?.cpus)} cores · ${memory(capacity?.memory_mb)}. Global pool: ${cores(state.budget?.cpus)} cores · ${memory(state.budget?.memory_mb)}. These are reserved ceilings, not measured usage; automatic mode shares within them.`;
}
function openDeployment(project = null){
  if(!state||deployApplying)return;
  updateProjectOptions();
  renderDeploymentMachines();
  if(!$("deploy-dialog").dataset.initialized){
    defaultDeploymentResources();
    try{const draft=JSON.parse(readStoredValue(sessionStorage,machineStorageKey("init-deploy-draft"))||"null");if(draft){for(const id of deployFieldIds)if(typeof draft.fields?.[id]==="string")$(id).value=draft.fields[id];if(["container","persistent"].includes(draft.workspace))document.querySelector(`input[name="deploy-workspace"][value="${draft.workspace}"]`).checked=true;for(const mount of (draft.mounts||[]).slice(0,32))addMountRow(mount);}}catch{/* An invalid saved draft does not prevent a new deployment. */}
    $("preset-note").hidden=$("preset-undo").hidden=true;previousPresetDraft=null;
    $("deploy-dialog").dataset.initialized="true";
  }
  const selected=project??currentProject;if(selected&&selected!=="__default__")$("deploy-project").value=selected;else if(project!==null||currentProject==="__default__")$("deploy-project").value="";
  $("deploy-interval-field").hidden=$("deploy-kind").value!=="job";syncDeploymentSource();configureDeploymentRanges();restorePortRows();invalidateDeployment();showDeploymentStep(1);$("deploy-dialog").showModal();$("deploy-dialog").scrollTop=0;
}
function fillDeploymentName(){
  const source=$("deploy-source").value==="github"?$("deploy-repository").value.trim().replace(/\/$/, "").replace(/\.git$/, ""):$("deploy-image").value.trim();
  if(!$("deploy-id").value.trim()&&source){
    const base=source.replace(/\/$/, "").split("/").pop().split(/[:@]/)[0].toLowerCase().replace(/[^a-z0-9-]/g,"-").replace(/^[^a-z]+/,"").slice(0,42)||"service";
    let id=base,suffix=2;while(state.services.some(service=>service.id===id))id=`${base}-${suffix++}`;
    $("deploy-id").value=id;
  }
}
function validateDeployment(){
  fillDeploymentName();
  const invalid=$("deploy-form").querySelector(":invalid");
  if(invalid){showDeploymentStep(1);for(let parent=invalid.parentElement;parent;parent=parent.parentElement)if(parent.tagName==="DETAILS")parent.open=true;invalid.reportValidity();return "Complete the highlighted setting before continuing.";}
  const service=deploymentPayload();if(state.services.some(existing=>existing.id===service.id))return `Service ID ${service.id} is already in use.`;
  if(service.workspace&&service.volumes.some(volume=>volume.target==="/workspace"))return "Private persistent storage already mounts /workspace. Remove the additional mount at that path.";
  if(new TextEncoder().encode(JSON.stringify(deploymentBody(false,service))).length>16384)return "The deployment configuration exceeds 16 KiB. Reduce arguments or mounts.";
  return "";
}
async function previewDeployment(){
  if(deployApplying)return;const error=validateDeployment();if(error){deploymentError(error);return;}
  const request=++deployRequest;const signature=deploymentSignature();$("preview-deployment").disabled=true;$("deploy-preview-note").textContent="Checking this deployment with Init…";deploymentError("");
  try{
    const plan=await post("/api/services",deploymentBody(false));if(request!==deployRequest||!$("deploy-dialog").open)return;
    const preview=node("div","deployment-review");const facts=node("dl","details-grid");const service=plan.service||{};
    for(const [label,value] of [["Service",service.name||service.id],["Machine",activeMachine ? `${activeMachine.profile.name} · ${activeMachine.profile.host} (SSH)` : "This computer"],["Image",service.image],["Resources",`${cores(service.limits?.cpus)} cores · ${memory(service.limits?.memory_mb)}`],["Ports on that machine",service.ports?.length?service.ports.join("\n"):"None"],["Storage",plan.workspace?.enabled?"Persistent at /workspace":"Container only · resets on recreation"]])detailField(facts,label,value||"Not reported");
    const technical=node("details","advanced-settings"),technicalFacts=node("dl","details-grid");technical.append(node("summary","","Deployment details"),technicalFacts);
    for(const [label,value] of [["Action",plan.action],["Workspace group",plan.project?.name||"Default"],["Execution target",plan.execution?.label||"Not reported"],["Private network",plan.network],["Container",plan.container],["Effective CPU",`${cores(plan.effective_limits?.cpus)} cores`],["Effective memory",memory(plan.effective_limits?.memory_mb)],["Command",service.command?.length?service.command.join("\n"):"Image default"]])detailField(technicalFacts,label,value||"Not reported");
    if(plan.workspace?.enabled){detailField(facts,"Private storage",`${plan.workspace.volume} → ${plan.workspace.mount_path}`,true);detailField(facts,"Workspace operation",plan.workspace.reused?"Reuse existing owned volume; prior data is retained":"Create a new empty persistent volume",true);preview.append(node("p","form-note","The image must be able to write to /workspace; non-root images may need permissions configured in the image. The volume is retained independently of containers."));}
    if(service.volumes?.length)detailField(facts,"Mounts",service.volumes.map(volume=>`${volume.source} → ${volume.target}${volume.read_only?" (read only)":""}`).join("\n"),true);
    if(plan.github){detailField(facts,"GitHub repository",plan.github.repository,true);detailField(facts,"Branch or tag",plan.github.reference||"Repository default branch");detailField(facts,"Build directory",plan.github.context||".");}preview.prepend(facts);
    for(const warning of plan.warnings||[])preview.append(node("p","form-warning",warning));
    if(plan.allocations?.length){const scroll=node("div","table-scroll");const table=node("table","preview-table");const head=node("thead");const tr=node("tr");for(const name of ["Service","CPU","Memory"])tr.append(node("th","",name));head.append(tr);const body=node("tbody");for(const allocation of plan.allocations){const row=node("tr");row.append(node("td","",allocation.id),node("td","",`${cores(allocation.cpus)} cores`),node("td","",memory(allocation.memory_mb)));body.append(row);}table.append(head,body);scroll.append(table);technical.append(node("h3","","Resulting allocations"),scroll);}preview.append(technical);
    $("deploy-preview").replaceChildren(preview);$("deploy-preview").hidden=false;deployNeedsReuse=Boolean(plan.workspace?.enabled&&plan.workspace?.reused);$("reuse-workspace-field").hidden=!deployNeedsReuse;$("reuse-workspace").checked=false;deployPreviewSignature=signature;$("deploy-preview-note").textContent=$("deploy-source").value==="github"?"Settings checked. Deploy service will clone the repository, build its Dockerfile, and start the service. Repository access and build success are checked during deployment.":"Settings checked. Your service will start when you choose Deploy service.";updateDeploymentApply();
  }catch(error){if(request===deployRequest){deploymentError(error.message);$("deploy-preview-note").textContent="Deployment could not be validated. Edit the form and preview again.";}}
  finally{if(request===deployRequest)$("preview-deployment").disabled=false;}
}
function setDeploymentPending(pending){deployApplying=pending;$("deploy-dialog").querySelectorAll("input,select,textarea,button").forEach(element=>{element.disabled=pending;});$("apply-deployment").textContent=pending?"Deploying…":"Deploy service";if(!pending){syncDeploymentSource();updateDeploymentApply();}else if($("deploy-source").value==="github")$("apply-deployment").textContent="Building & deploying…";}
document.querySelectorAll("[data-open-deploy]").forEach(button=>button.addEventListener("click",()=>openDeployment()));
$("add-mount").addEventListener("click",()=>{addMountRow();invalidateDeployment();});
$("deploy-form").addEventListener("input",event=>{if(event.target.id==="reuse-workspace"){updateDeploymentApply();return;}if(event.target.id==="deploy-source")syncDeploymentSource();if(event.target.id==="deploy-kind")$("deploy-interval-field").hidden=$("deploy-kind").value!=="job";for(const key of ["cpu","memory"]){if(event.target.id===`deploy-${key}-range`)$(`deploy-${key}`).value=event.target.value;if(event.target.id===`deploy-${key}`)$(`deploy-${key}-range`).value=event.target.value;}invalidateDeployment();});
$("preview-deployment").addEventListener("click",previewDeployment);
const deploymentPresets={web:{image:"nginx/nginx-unprivileged:stable-alpine",id:"web",kind:"daemon",ports:"127.0.0.1:8080:8080"}};
$("deploy-own-image").addEventListener("click",()=>{$("deploy-source").value="image";syncDeploymentSource();invalidateDeployment();$("deploy-image").focus();$("deploy-image").select();});
$("deploy-from-github").addEventListener("click",()=>{$("deploy-source").value="github";syncDeploymentSource();invalidateDeployment();$("deploy-repository").focus();});
$("deploy-repository").addEventListener("change",()=>{fillDeploymentName();invalidateDeployment();});
let previousPresetDraft=null;
function restoreDeploymentDraft(draft){
  for(const id of deployFieldIds)if(typeof draft.fields?.[id]==="string")$(id).value=draft.fields[id];
  if(["container","persistent"].includes(draft.workspace))document.querySelector(`input[name="deploy-workspace"][value="${draft.workspace}"]`).checked=true;
  $("deploy-mounts").replaceChildren();for(const mount of (draft.mounts||[]).slice(0,32))addMountRow(mount);
  $("deploy-interval-field").hidden=$("deploy-kind").value!=="job";syncDeploymentSource();configureDeploymentRanges();restorePortRows();invalidateDeployment();
}
document.querySelectorAll("[data-preset]").forEach(button=>button.addEventListener("click",()=>{
  previousPresetDraft=deploymentDraft();$("deploy-source").value="image";syncDeploymentSource();const preset=deploymentPresets[button.dataset.preset];
  for(const key of ["image","id","kind","ports"])$("deploy-"+key).value=preset[key];
  let id=preset.id,n=2;while(state.services.some(service=>service.id===id))id=`${preset.id}-${n++}`;$("deploy-id").value=id;$("deploy-name").value="";
  defaultDeploymentResources();$("deploy-interval").value=3600;
  $("deploy-interval-field").hidden=preset.kind!=="job";configureDeploymentRanges();restorePortRows();invalidateDeployment();
  $("preset-note").textContent="Starter applied. Custom settings kept.";$("preset-note").hidden=$("preset-undo").hidden=false;
}));
$("preset-undo").addEventListener("click",()=>{if(previousPresetDraft)restoreDeploymentDraft(previousPresetDraft);previousPresetDraft=null;$("preset-note").hidden=$("preset-undo").hidden=true;});
const deploymentSizes={small:[.25,256],medium:[1,1024],large:[2,4096]};
document.querySelectorAll("[data-size]").forEach(button=>button.addEventListener("click",()=>{const [cpu,ram]=deploymentSizes[button.dataset.size];$("deploy-cpu").value=cpu;$("deploy-memory").value=ram;configureDeploymentRanges();invalidateDeployment();}));
function showDeploymentStep(step) {
  deploymentStep=step;
  document.querySelectorAll("[data-deploy-panel]").forEach(panel=>{panel.hidden=Number(panel.dataset.deployPanel)!==step;});
  $("deploy-title").textContent=step===1?"Deploy service":"Review deployment";
  $("deploy-back").hidden=step===1;$("deploy-next").hidden=step===2;$("apply-deployment").hidden=step!==2;
  $("deploy-dialog").scrollTop=0;
}
async function advanceDeployment(step) {
  if(deployApplying)return;
  if(step===2){const error=validateDeployment();if(error){deploymentError(error);return;}}
  showDeploymentStep(step);
  if(step===2&&deployPreviewSignature!==deploymentSignature())await previewDeployment();
  $("deploy-title").focus({preventScroll:true});
}
$("deploy-next").addEventListener("click",()=>advanceDeployment(deploymentStep+1));
$("deploy-back").addEventListener("click",()=>advanceDeployment(deploymentStep-1));
$("deploy-image").addEventListener("change",()=>{fillDeploymentName();invalidateDeployment();});
$("deploy-form").addEventListener("submit",async event=>{
  event.preventDefault();if(deploymentStep===1){await advanceDeployment(2);return;}if(deployApplying||deployPreviewSignature!==deploymentSignature()||(deployNeedsReuse&&!$("reuse-workspace").checked)){deploymentError("Check the current settings before deploying, and acknowledge reuse if required.");return;}
  const service=deploymentPayload();setDeploymentPending(true);deploymentError("");$("deploy-preview-note").textContent="Deploying… Large images can take a while. The controller is completing the accepted deployment.";
  try{await post("/api/services",deploymentBody(true,service));try{clearStoredValue(sessionStorage,machineStorageKey("init-deploy-draft"));}catch{}await refresh(true);currentProject=service.project||"__default__";$("deploy-dialog").close();$("deploy-dialog").dataset.initialized="";$("deploy-form").reset();$("deploy-mounts").replaceChildren();$("deploy-port-rows").replaceChildren();toast(`${service.name||service.id} deployed in ${projectName(service.project)}`);navigate("services",service.id);}catch(error){deploymentError(error.message);$("deploy-preview-note").textContent="The request returned an error. The form is retained; inspect the message before trying again.";refresh(true);}finally{setDeploymentPending(false);}
});
document.querySelectorAll("[data-create-workspace]").forEach(button=>button.addEventListener("click",()=>$("project-dialog").showModal()));
$("project-form").addEventListener("submit",async event=>{
  event.preventDefault();if(projectApplying)return;projectApplying=true;$("project-error").hidden=true;$("project-dialog").querySelectorAll("input,textarea,button").forEach(element=>{element.disabled=true;});$("create-project").textContent="Creating…";
  const project={id:$("project-id").value.trim(),name:$("project-name").value.trim()||$("project-id").value.trim(),description:$("project-description").value.trim()};
  try{await post("/api/projects",project);await refresh(true);currentProject=project.id;$("project-dialog").close();$("project-form").reset();toast(`${project.name} workspace created`);navigate("workspaces");}catch(error){$("project-error").textContent=error.message;$("project-error").hidden=false;}finally{projectApplying=false;$("project-dialog").querySelectorAll("input,textarea,button").forEach(element=>{element.disabled=false;});$("create-project").textContent="Create workspace";}
});
document.addEventListener("click",event=>{const button=event.target.closest("button[data-project-action]");if(!button||!state)return;if(button.dataset.projectAction==="deploy")openDeployment(button.dataset.project);else{currentProject=button.dataset.project;persistProject();navigate(button.dataset.projectAction==="overview"?"workspaces":"services");renderState();}});

function updateFreshness(){
  if(document.hidden)return;
  const age=lastStateAt?Math.max(0,Math.floor((Date.now()-lastStateAt)/1000)):null;
  const stale=age!==null&&age>=Math.max(15,pollIntervalMs/1000*2.5);document.body.dataset.stale=String(stale);
  const interrupted=document.body.dataset.connection==="interrupted";
  const label=interrupted?"Offline · retrying":age===null?"Connecting…":stale?"Stale":"Live";
  if($("connection-state").textContent!==label)$("connection-state").textContent=label;
  $("connection-age").textContent=age===null||interrupted?"":` · ${age}s ago`;
  $("runtime-label").textContent=state?.runtime?.available?state.runtime.execution?.label||"Docker available":"Docker unavailable";
}
setInterval(updateFreshness,1000);
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  $("theme-toggle").textContent = theme === "dark" ? "Light theme" : "Dark theme";
  $("settings-theme").value=theme;
  renderUsageCharts();
}
let savedTheme;
try { savedTheme = readStoredValue(localStorage,"init-theme-v3"); } catch { /* The current session can still change theme. */ }
applyTheme(savedTheme === "light" ? "light" : "dark");
$("theme-toggle").addEventListener("click", () => {
  const theme = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  applyTheme(theme);
  try { localStorage.setItem("init-theme-v3", theme); } catch { /* The current session can still change theme. */ }
});
if(!location.hash || /^#\/(?:resources|settings|monitor|machines|github)(?:\/|$)/.test(location.hash)){try{currentProject=readStoredValue(localStorage,machineStorageKey("init-project"))||"";}catch{}if(!location.hash)history.replaceState(null,"",routeHref("workspaces"));}
installMachineControls();
showRoute();
initializeMachines();
function synchronizeVisibleViews() {
  if (document.hidden) return;
  refresh();
  if ($("details-dialog").open && detailsTab === "logs") loadDetailLogs(true);
}
function restartPolling(){clearInterval(pollTimer);pollTimer=setInterval(synchronizeVisibleViews,pollIntervalMs);}
restartPolling();
document.addEventListener("visibilitychange", synchronizeVisibleViews);
window.addEventListener("resize", renderUsageCharts);

$("all-workspaces-link").addEventListener("click",()=>{currentProject="";persistProject();navigate("workspaces");});
$("workspace-all-services").addEventListener("click",()=>navigate("services"));
$("workspace-open-monitor").addEventListener("click",()=>{setHistoryWindow(86400);navigate("overview");});
$("workspace-all-events").addEventListener("click",()=>navigate("activity"));
