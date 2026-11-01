import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source=readFileSync(new URL('../assets/app.js',import.meta.url),'utf8');
const machinesSource=readFileSync(new URL('../assets/machines.js',import.meta.url),'utf8');
const html=readFileSync(new URL('../assets/index.html',import.meta.url),'utf8');
const ids=[...html.matchAll(/\bid="([^"]+)"/g)].map(match=>match[1]);
assert.equal(new Set(ids).size,ids.length,'HTML IDs must be unique');
for(const [,id] of (source+machinesSource).matchAll(/\$\("([^"]+)"\)/g))assert.ok(ids.includes(id),`Missing DOM target ${id}`);
const start=source.indexOf('function statusGroup('),end=source.indexOf('function drawStatusChart()',start);
const {statusGroup,statusGroups}=vm.runInNewContext(source.slice(start,end)+'\n({statusGroup,statusGroups})');
assert.equal(statusGroup({status:'running',health:'unhealthy'}),'attention');
assert.equal(statusGroup({status:'running',enabled:false}),'disabled');
assert.equal(statusGroup({status:'exited',kind:'job',next_run:123}),'scheduled');
assert.equal(statusGroup({status:'not created'}),'stopped');
assert.equal(statusGroup({status:'restarting'}),'starting');
assert.equal(statusGroup({status:'unrecognized'}),'unknown');
const groups=statusGroups([{status:'running'},{status:'error'},{status:'exited'},{status:null}]);
assert.equal(groups.reduce((n,g)=>n+g.count,0),4);
assert.equal(groups.find(g=>g.key==='running').count,1);
console.log('UI checks passed: DOM contracts, status grouping, and honest unknown states.');

const context=vm.createContext({URLSearchParams,location:{search:'?machine=linux',pathname:'/'}});
vm.runInContext(machinesSource,context);
for (const path of ['/api/state','/api/services/web/logs?tail=200','/api/services/web/restart']) {
  assert.equal(vm.runInContext(`machineApiPath(${JSON.stringify(path)}, machineId)`,context), `/api/machines/linux/proxy/${path.slice(5)}`);
}
for (const path of ['/api/session','/api/machines','/api/machines/mac/test','/api/machines/tailscale']) {
  assert.equal(vm.runInContext(`machineApiPath(${JSON.stringify(path)}, machineId)`,context), path);
}
assert.equal(vm.runInContext('machineStorageKey("selfhost-deploy-draft")',context),'selfhost-deploy-draft:machine:linux');
assert.equal(vm.runInContext('machineUrl("mac")',context),'/?machine=mac#/overview');
assert.equal(vm.runInContext('machineUrl("")',context),'/#/overview');
assert.throws(()=>vm.runInContext('machineApiPath("/api/state", "../other")',context));
assert.equal(vm.runInContext('machineApiPath("/api/state", "")',context),'/api/state');
console.log('Machine routing checks passed: remote operations, local session/profile access, isolated drafts.');
