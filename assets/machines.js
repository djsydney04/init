"use strict";

const machineId = new URLSearchParams(location.search).get("machine") || "";
let activeMachine = null;
let savedMachines = [];
let machinesReady = false;
let machineBusy = false;

function machineStorageKey(key) { return machineId ? `${key}:machine:${machineId}` : key; }
function machineApiPath(path, id) {
  if (!id || path === "/api/session" || path === "/api/machines" || path.startsWith("/api/machines/")) return path;
  if (!/^[a-z][a-z0-9-]{0,47}$/.test(id) || !path.startsWith("/api/")) throw new Error("Invalid machine connection");
  return `/api/machines/${encodeURIComponent(id)}/proxy/${path.slice(5)}`;
}
function machineUrl(id, route = "overview", deploy = false) {
  const query = new URLSearchParams();
  if (id) query.set("machine", id);
  if (deploy) query.set("deploy", "1");
  return `${location.pathname}${query.toString() ? `?${query}` : ""}#/${route}`;
}
function openMachine(id, deploy = false) {
  if (pending.size || deployApplying || resourceApplying || projectApplying || machineBusy) {
    toast("Wait for the current change to finish before switching machines.");
    $("machine-select").value = $("deploy-machine").value = machineId;
    return;
  }
  if (deploy && id && !savedMachines.some(machine => machine.profile.id === id && machine.identity)) {
    $("deploy-machine").value = machineId;
    deploymentError("Test this SSH connection in Machines before deploying to it.");
    return;
  }
  if (deploy) invalidateDeployment();
  location.assign(machineUrl(id, deploy ? "services" : "overview", deploy));
}
function renderDeploymentMachines() {
  const options = savedMachines.map(({profile, identity}) => {
    const option = new Option(`${profile.name} · SSH${identity ? "" : " · Test connection first"}`, profile.id);
    option.disabled = !identity;
    return option;
  });
  $("deploy-machine").replaceChildren(new Option("This computer", ""), ...options);
  $("deploy-machine").value = machineId;
}
function machineError(message) { $("machine-error").textContent = message; $("machine-error").hidden = !message; }
function editMachine(profile) {
  for (const key of ["id", "name", "host", "ssh_port", "init_port"]) $(`machine-${key}`).value = profile?.[key] ?? (key === "init_port" ? "8787" : "");
  $("machine-id").readOnly = Boolean(profile);
  $("machine-form-title").textContent = profile ? "Edit connection" : "Add a machine";
  $("machine-save").textContent = profile ? "Save changes" : "Save connection";
  machineError("");
  $("machine-host").focus();
}
function renderMachines() {
  const select = $("machine-select");
  select.replaceChildren(new Option("This computer", ""), ...savedMachines.map(({profile}) => new Option(profile.name, profile.id)));
  select.value = machineId;
  if (machineId && select.selectedIndex < 0) { select.add(new Option("Unavailable machine", machineId)); select.value = machineId; }
  const cards = document.createDocumentFragment();
  const local = node("article", "panel machine-card");
  local.append(node("h2", "", "This computer"), node("p", "muted", "The controller you opened Init from."));
  const localButton = node("button", "button secondary", machineId ? "Open local services" : "Currently selected");
  localButton.type = "button"; localButton.disabled = !machineId; localButton.addEventListener("click", () => openMachine("")); local.append(localButton); cards.append(local);
  for (const machine of savedMachines) {
    const {profile, identity} = machine;
    const card = node("article", "panel machine-card");
    const heading = node("div", "machine-heading");
    heading.append(node("h2", "", profile.name), node("span", "status-badge", profile.id === machineId ? "Selected" : identity ? "Tested" : "Setup needed"));
    card.append(heading, node("p", "machine-address", `${profile.host} · SSH ${profile.ssh_port || "config/default"} · Init ${profile.init_port}`));
    card.append(node("p", "muted", identity ? `Init ${identity.version} · ${identity.workspace}` : "Test SSH and the remote controller before opening this machine."));
    const actions = node("div", "detail-actions");
    for (const [action, label] of [["open", "Open services"], ["test", "Test connection"], ["edit", "Edit"], ["remove", "Remove connection"]]) {
      const button = node("button", `button ${action === "open" ? "primary" : "secondary"}`, label);
      button.type = "button"; button.dataset.machineAction = action; button.dataset.machineId = profile.id;
      button.disabled = machineBusy || (action === "open" && !identity) || (action === "remove" && profile.id === machineId);
      if (action === "remove" && profile.id === machineId) button.title = "Switch to this computer before removing this connection";
      actions.append(button);
    }
    card.append(actions); cards.append(card);
  }
  $("machine-cards").replaceChildren(cards);
}
async function loadMachines() { savedMachines = (await api("/api/machines")).machines; renderMachines(); }
function setMachineBusy(value) {
  machineBusy = value;
  $("machine-form").querySelectorAll("input,button").forEach(element => { element.disabled = value; });
  renderMachines();
}
async function initializeMachines() {
  try {
    await loadMachines();
    activeMachine = savedMachines.find(machine => machine.profile.id === machineId) || null;
    if (machineId) {
      $("machine-context").hidden = false;
      $("machine-context-name").textContent = activeMachine ? `${activeMachine.profile.name} · ${activeMachine.profile.host}` : "Unavailable machine";
      if (!activeMachine) throw new Error("This saved machine no longer exists. Choose This computer or add it again in Machines");
      if (!activeMachine.identity) throw new Error("Test this connection in Machines before opening its services");
    }
    machinesReady = true;
    await refresh();
    const url = new URL(location.href);
    if (url.searchParams.get("deploy") === "1") {
      url.searchParams.delete("deploy");
      history.replaceState(null, "", url.pathname + url.search + url.hash);
      if (state) openDeployment();
    }
  } catch (error) { showError(error.message); }
}
function installMachineControls() {
  $("deploy-machine").addEventListener("change", event => {
    if (event.target.value !== machineId) openMachine(event.target.value, true);
  });
  $("deploy-connect-machine").addEventListener("click", () => {
    if (deployApplying) return;
    $("deploy-dialog").close();
    navigate("machines");
  });
  $("machine-select").addEventListener("change", event => openMachine(event.target.value));
  $("machine-new").addEventListener("click", () => editMachine(null));
  $("machine-form").addEventListener("submit", async event => {
    event.preventDefault(); if (machineBusy) return;
    const profile = {id:$("machine-id").value.trim(), name:$("machine-name").value.trim(), host:$("machine-host").value.trim(), ssh_port:$("machine-ssh_port").value ? Number($("machine-ssh_port").value) : null, init_port:Number($("machine-init_port").value)};
    setMachineBusy(true); machineError("");
    try { await post("/api/machines", profile); await loadMachines(); editMachine(null); toast("Connection saved. Test it before opening services."); }
    catch (error) { machineError(error.message); }
    finally { setMachineBusy(false); }
  });
  $("machine-cards").addEventListener("click", async event => {
    const button = event.target.closest("button[data-machine-action]"); if (!button || machineBusy) return;
    const machine = savedMachines.find(item => item.profile.id === button.dataset.machineId); if (!machine) return;
    const action = button.dataset.machineAction;
    if (action === "open") { openMachine(machine.profile.id); return; }
    if (action === "edit") { editMachine(machine.profile); return; }
    setMachineBusy(true); machineError(""); $("machine-result").textContent = action === "test" ? `Checking ${machine.profile.name}…` : "Removing saved connection…";
    try {
      const result = await post(`/api/machines/${encodeURIComponent(machine.profile.id)}/${action}`);
      await loadMachines();
      $("machine-result").textContent = action === "test" ? `Connected to ${machine.profile.name}. Init ${result.identity.version}, workspace ${result.identity.workspace}. Open services to use it.` : "Connection removed. The remote services were not changed.";
    } catch (error) { $("machine-result").textContent = ""; machineError(error.message); }
    finally { setMachineBusy(false); }
  });
  $("tailscale-check").addEventListener("click", async () => {
    const button = $("tailscale-check"); button.disabled = true; $("tailscale-status").textContent = "Checking Tailscale on this computer…"; $("tailscale-peers").replaceChildren();
    try {
      const result = await api("/api/machines/tailscale");
      $("tailscale-status").textContent = !result.installed ? "Tailscale is not installed on this computer. Install it on both machines and sign in to the same network." : !result.connected ? `Tailscale is ${result.status}. Open Tailscale and connect, then check again.` : `Tailscale is connected. ${result.peers.length} visible machines. SSH and Init still need to be set up on the destination.`;
      for (const peer of result.peers) {
        const row = node("div", "tailscale-peer"); const info = node("div");
        info.append(node("strong", "", peer.name), node("p", "muted", `${peer.os} · ${peer.online === true ? "Online" : peer.online === false ? "Offline" : "Status unknown"} · ${peer.host}`));
        const use = node("button", "button secondary", "Use address"); use.type = "button";
        use.addEventListener("click", () => { if (machineBusy) return; editMachine(null); $("machine-host").value = peer.host; $("machine-name").value = peer.name; $("machine-id").value = peer.name.toLowerCase().replace(/[^a-z0-9-]/g,"-").replace(/^[^a-z]+/,"").slice(0,48) || "machine"; $("machine-host").focus(); });
        row.append(info, use); $("tailscale-peers").append(row);
      }
    } catch (error) { $("tailscale-status").textContent = error.message; }
    finally { button.disabled = false; }
  });
}
