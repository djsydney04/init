import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source=readFileSync(new URL('../assets/app.js',import.meta.url),'utf8');
const machinesSource=readFileSync(new URL('../assets/machines.js',import.meta.url),'utf8');
const html=readFileSync(new URL('../assets/index.html',import.meta.url),'utf8');
const ids=[...html.matchAll(/\bid="([^"]+)"/g)].map(match=>match[1]);
assert.equal(new Set(ids).size,ids.length,'HTML IDs must be unique');
const dynamicIds=[...source.matchAll(/\.id="([^"]+)"/g)].map(match=>match[1]);
for(const [,id] of (source+machinesSource).matchAll(/\$\("([^"]+)"\)/g))assert.ok(ids.includes(id)||dynamicIds.includes(id),`Missing DOM target ${id}`);
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
for (const path of ['/api/state','/api/history?project=tools&window_seconds=86400&max_points=720','/api/settings','/api/services/web/logs?tail=200','/api/services/web/restart']) {
  assert.equal(vm.runInContext(`machineApiPath(${JSON.stringify(path)}, machineId)`,context), `/api/machines/linux/proxy/${path.slice(5)}`);
}
for (const path of ['/api/session','/api/machines','/api/machines/mac/test','/api/machines/tailscale']) {
  assert.equal(vm.runInContext(`machineApiPath(${JSON.stringify(path)}, machineId)`,context), path);
}
assert.equal(vm.runInContext('machineStorageKey("init-deploy-draft")',context),'init-deploy-draft:machine:linux');
assert.equal(vm.runInContext('machineStorageKey("selfhost-deploy-draft")',context),'init-deploy-draft:machine:linux');
const stored=new Map([
  ['selfhost-theme-v3','light'],['selfhost-update-ms','10000'],['selfhost-project:machine:linux','tools'],
  ['selfhost-project','local-tools'],['selfhost-deploy-draft:machine:linux','legacy draft'],
  ['selfhost-deploy-draft:machine:mac','other machine draft']
]);
context.storage={getItem:key=>stored.get(key)??null,setItem:(key,value)=>stored.set(key,value),removeItem:key=>stored.delete(key)};
assert.equal(vm.runInContext('readStoredValue(storage,"init-theme-v3")',context),'light');
assert.equal(vm.runInContext('readStoredValue(storage,"init-update-ms")',context),'10000');
assert.equal(vm.runInContext('readStoredValue(storage,machineStorageKey("init-project"))',context),'tools');
assert.equal(vm.runInContext('readStoredValue(storage,"init-project")',context),'local-tools');
stored.set('init-project:machine:linux','');
assert.equal(vm.runInContext('readStoredValue(storage,machineStorageKey("init-project"))',context),'','An explicit All workspaces selection overrides its legacy value');
assert.equal(vm.runInContext('readStoredValue(storage,machineStorageKey("init-deploy-draft"))',context),'legacy draft');
stored.set('init-deploy-draft:machine:linux','current draft');
assert.equal(vm.runInContext('readStoredValue(storage,machineStorageKey("init-deploy-draft"))',context),'current draft');
vm.runInContext('clearStoredValue(storage,machineStorageKey("init-deploy-draft"))',context);
assert.equal(vm.runInContext('readStoredValue(storage,machineStorageKey("init-deploy-draft"))',context),null,'A completed deployment cannot restore its legacy draft');
assert.equal(stored.get('selfhost-deploy-draft:machine:mac'),'other machine draft','Clearing one machine never removes another machine draft');
assert.equal(vm.runInContext('machineUrl("mac")',context),'/?machine=mac#/overview');
assert.equal(vm.runInContext('machineUrl("")',context),'/#/overview');
assert.throws(()=>vm.runInContext('machineApiPath("/api/state", "../other")',context));
assert.equal(vm.runInContext('machineApiPath("/api/state", "")',context),'/api/state');
console.log('Machine routing checks passed: remote operations, local session/profile access, isolated drafts and legacy preference fallback.');

const requests=[];
const wireContext=vm.createContext({sessionToken:'token',session:async()=>{},api:(path,options)=>{requests.push({path,...options});}});
const postStart=source.indexOf('async function post('),postEnd=source.indexOf('function showError(',postStart);
vm.runInContext(source.slice(postStart,postEnd),wireContext);
await vm.runInContext('post("/api/settings",{max_parallel_jobs:2})',wireContext);
assert.equal(requests[0].headers['X-Init-Token'],'token');
assert.equal(requests[0].headers['X-Selfhost-Token'],undefined);
assert.equal(requests[0].body,'{"max_parallel_jobs":2}');

assert.equal(vm.runInContext('machineUrl("mac", "services", true)',context),'/?machine=mac&deploy=1#/services');
assert.equal(vm.runInContext('machineUrl("", "services", true)',context),'/?deploy=1#/services');
const navigations=[], messages=[];
let invalidated=0;
const machineSelect={value:'linux'};
const deploymentSelect={value:'linux',replaceChildren(...options){this.options=options;}};
Object.assign(context, {
  pending:new Set(),deployApplying:false,resourceApplying:false,projectApplying:false,settingsApplying:false,
  $:id=>id==='machine-select'?machineSelect:deploymentSelect,
  Option:class {constructor(text,value){this.text=text;this.value=value;this.disabled=false;}},
  toast:message=>messages.push(message),deploymentError:message=>messages.push(message),
  invalidateDeployment:()=>{invalidated++;}
});
context.location.assign=url=>navigations.push(url);
vm.runInContext('savedMachines = [{profile:{id:"linux",name:"Linux"},identity:{}},{profile:{id:"mac",name:"Build Mac"},identity:{}},{profile:{id:"untested",name:"Untested"},identity:null}]',context);
vm.runInContext('renderDeploymentMachines()',context);
assert.equal(deploymentSelect.options[0].value,'');
assert.equal(deploymentSelect.options.find(option=>option.value==='mac').disabled,false);
assert.equal(deploymentSelect.options.find(option=>option.value==='untested').disabled,true);
vm.runInContext('openMachine("mac", true)',context);
assert.equal(navigations.pop(),'/?machine=mac&deploy=1#/services');
assert.equal(invalidated,1,'A previous preview must be invalidated before changing destination');
vm.runInContext('openMachine("untested", true)',context);
assert.equal(navigations.length,0,'Untested SSH destinations cannot deploy');
assert.equal(deploymentSelect.value,'linux');
context.deployApplying=true;
vm.runInContext('openMachine("mac", true)',context);
assert.equal(navigations.length,0,'An accepted deployment must keep its destination');
assert.equal(invalidated,1);
context.deployApplying=false;
vm.runInContext('openMachine("", true)',context);
assert.equal(navigations.pop(),'/?deploy=1#/services');
console.log('Deployment destination checks passed: tested connections, local/remote routing, and pending-write guards.');

const fields=new Map();
const field=id=>{if(!fields.has(id))fields.set(id,{value:'',hidden:false,focus(){}});return fields.get(id);};
const setup=vm.createContext({$:field,state:{services:[{id:'nginx'},{id:'nginx-2'}],budget:{cpus:.05,memory_mb:32},capacity:{cpus:4,memory_mb:8192}},deployApplying:false});
function loadFunction(name){const match=new RegExp(`(?:async )?function ${name}\\(`).exec(source);const start=match.index,end=source.indexOf('\n}',start)+2;vm.runInContext(source.slice(start,end),setup);}
loadFunction('fillDeploymentName');loadFunction('defaultDeploymentResources');loadFunction('validateDeployment');
field('deploy-image').value='registry.example/app/NGINX:alpine';
vm.runInContext('fillDeploymentName()',setup);
assert.equal(field('deploy-id').value,'nginx-3','Image names become unique valid service names');
field('deploy-id').value='my-service';
vm.runInContext('fillDeploymentName()',setup);
assert.equal(field('deploy-id').value,'my-service','Keep an explicitly chosen service name');
field('deploy-id').value='';field('deploy-image').value='registry.example/123:latest';
vm.runInContext('fillDeploymentName()',setup);
assert.equal(field('deploy-id').value,'service');
vm.runInContext('defaultDeploymentResources()',setup);
assert.equal(field('deploy-cpu').value,.05);assert.equal(field('deploy-memory').value,32);
assert.equal(field('deploy-min-cpu').value,.05);assert.equal(field('deploy-min-memory').value,32,'Default minimums must fit small machine budgets');
setup.state.budget={cpus:4,memory_mb:4096};
vm.runInContext('defaultDeploymentResources()',setup);
assert.equal(field('deploy-cpu').value,.25);assert.equal(field('deploy-memory').value,256);
const custom={tagName:'DETAILS',open:false,parentElement:null},nested={tagName:'DETAILS',open:false,parentElement:custom};
let reported=false,shown=0,previews=0;
field('deploy-form').querySelector=()=>({parentElement:nested,reportValidity(){reported=true;}});
Object.assign(setup,{showDeploymentStep:step=>{shown=step;},deploymentError:()=>{},deploymentStep:1,previewDeployment:async()=>{previews++;},deployPreviewSignature:null,deploymentSignature:()=>'{current}',document:{querySelector:()=>null}});
assert.match(vm.runInContext('validateDeployment()',setup),/highlighted/);
assert.equal(custom.open,true);assert.equal(nested.open,true);assert.equal(reported,true);assert.equal(shown,1,'Reveal invalid settings even inside collapsed customization');
loadFunction('advanceDeployment');
setup.validateDeployment=()=>'';
await vm.runInContext('advanceDeployment(2)',setup);
assert.equal(shown,2);assert.equal(previews,1,'Setup moves directly to a checked review');
setup.deployApplying=true;
await vm.runInContext('advanceDeployment(1)',setup);
assert.equal(shown,2,'An accepted deployment cannot leave its review');
assert.equal([...html.matchAll(/data-deploy-panel="/g)].length,2);
console.log('Quick deployment checks passed: names, bounded defaults, accessible validation, and two-step review.');

loadFunction('deploymentBody');loadFunction('syncDeploymentSource');
setup.deploymentPayload=()=>({id:'app',image:'',limits:{cpus:.25,memory_mb:256}});
field('deploy-source').value='github';field('deploy-repository').value=' https://github.com/example/app ';field('deploy-reference').value='release/v2';field('deploy-context').value='services/web';
vm.runInContext('syncDeploymentSource()',setup);
assert.equal(field('deploy-image').disabled,true);
assert.equal(field('deploy-repository').required,true);
assert.equal(field('deploy-reference').disabled,false);
const body=JSON.parse(vm.runInContext('JSON.stringify(deploymentBody(false))',setup));
assert.deepEqual(body.github,{repository:'https://github.com/example/app',reference:'release/v2',context:'services/web'});
assert.equal(body.apply,false);
field('deploy-id').value='';field('deploy-repository').value='https://github.com/example/App.git/';
vm.runInContext('fillDeploymentName()',setup);
assert.equal(field('deploy-id').value,'app');
field('deploy-source').value='image';vm.runInContext('syncDeploymentSource()',setup);
assert.equal(field('deploy-repository').disabled,true);
assert.equal(field('deploy-image').required,true);
assert.equal(vm.runInContext('deploymentBody(true).github',setup),undefined,'Image deployments never include a stale GitHub source');
console.log('GitHub deployment UI checks passed: source routing, source validation fields, defaults and repository-derived names.');

// Recorded utilization must remain truthful across aliases, missing samples and project scopes.
const numeric=value=>typeof value==='number'&&Number.isFinite(value);
const now=Math.floor(Date.now()/1000);
const liveContext=vm.createContext({numeric,eligibleService:service=>service.telemetry_eligible!==false,Date});
vm.runInContext(source.slice(source.indexOf('function scopedTelemetry('),source.indexOf('function scopeHistoryKey(')),liveContext);
const live=services=>{liveContext.rows=services;return vm.runInContext('scopedTelemetry(rows)',liveContext);};
let reading=live([{id:'a',telemetry_target:'same',sampled_at:now,cpu_percent:2},{id:'b',telemetry_target:'same',sampled_at:now,memory_mb:9}]);
assert.equal(reading.eligible,1);assert.equal(reading.cpu,2);assert.equal(reading.memory,9);assert.equal(reading.cpu_complete,true);
reading=live([{id:'a',sampled_at:now,cpu_percent:0,memory_mb:0},{id:'b',sampled_at:now,memory_mb:3}]);
assert.equal(reading.cpu,0,'An actual zero remains measured');assert.equal(reading.cpu_complete,false);assert.equal(reading.memory,3);assert.equal(reading.memory_complete,true);
reading=live([{id:'a',sampled_at:now-31,cpu_percent:7,memory_mb:8}]);assert.equal(reading.cpu,null);assert.equal(reading.memory,null);
const historyContext=vm.createContext({numeric,usageHistory:new Map(),historyRequests:new Map(),fullTime:value=>String(value)});
vm.runInContext(source.slice(source.indexOf('function historicalStats('),source.indexOf('function renderRecordedHistoryStatus(')),historyContext);
historyContext.usageHistory.set('records',[{cpu:10,cpu_samples:2,cpu_min:8,cpu_max:12},{cpu:30,cpu_samples:1,cpu_min:30,cpu_max:30},{cpu:null,cpu_samples:0}]);
const stats=vm.runInContext('historicalStats("records","cpu")',historyContext);assert.equal(stats.average,50/3);assert.equal(stats.minimum,8);assert.equal(stats.maximum,30);assert.equal(stats.count,3);
historyContext.historyRequests.set('records',{response:{status:'error',error:'disk full',bucket_seconds:120,points:[{cpu_percent:10}],coverage:{cpu_buckets:1,memory_buckets:0,buckets:720}}});
const status=vm.runInContext('historyStatusText({key:"records"})',historyContext);assert.match(status,/disk full/);assert.match(status,/retained/);assert.match(status,/1\/720/);
const routeContext=vm.createContext({currentProject:'all',encodeURIComponent});vm.runInContext(source.slice(source.indexOf('function globalRoute('),source.indexOf('function renderPageIdentity(')),routeContext);
assert.equal(vm.runInContext('routeHref("settings")',routeContext),'#/settings');assert.equal(vm.runInContext('routeHref("services")',routeContext),'#/p/all/services');
console.log('Recorded utilization checks passed: unique targets, nulls, weighted bounds, retained-history errors, scoped/global navigation.');
