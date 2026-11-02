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
assert.equal(vm.runInContext('machineUrl("mac")',context),'/?machine=mac#/workspaces','Switching machines lands on that machine\'s workspaces');
assert.equal(vm.runInContext('machineUrl("")',context),'/#/workspaces');
assert.throws(()=>vm.runInContext('machineApiPath("/api/state", "../other")',context));
assert.equal(vm.runInContext('machineApiPath("/api/state", "")',context),'/api/state');
console.log('Machine routing checks passed: remote operations, local session/profile access, isolated drafts and legacy preference fallback.');

const requests=[];
const wireContext=vm.createContext({machineId:'linux',sessionToken:'token',session:async()=>{},api:(path,options,targetMachine)=>{requests.push({path,...options,targetMachine});}});
const postStart=source.indexOf('async function post('),postEnd=source.indexOf('function showError(',postStart);
vm.runInContext(source.slice(postStart,postEnd),wireContext);
await vm.runInContext('post("/api/settings",{max_parallel_jobs:2})',wireContext);
assert.equal(requests[0].headers['X-Init-Token'],'token');
assert.equal(requests[0].headers['X-Selfhost-Token'],undefined);
assert.equal(requests[0].body,'{"max_parallel_jobs":2}');
assert.equal(requests[0].targetMachine,'linux');

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
vm.runInContext('renderDestinationMachines("deploy-machine")',context);
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

// Keep routing and creation real; replace only the browser and HTTP transport.
function workspaceFixture(transport) {
  const elements=new Map(),requests=[],navigations=[],refreshes=[];
  const field=id=>{
    if(!elements.has(id))elements.set(id,{value:'',hidden:false,disabled:false,textContent:'',options:[],
      get selectedIndex(){return this.options.findIndex(option=>option.value===this.value);},
      replaceChildren(...options){this.options=options;},add(option){this.options.push(option);}});
    return elements.get(id);
  };
  const controls=['project-machine','project-id','project-name','project-description','create-project'].map(field);
  const dialog=field('project-dialog');dialog.closed=0;dialog.open=false;
  dialog.querySelectorAll=()=>controls;dialog.showModal=()=>{dialog.open=true;};dialog.close=()=>{dialog.closed++;dialog.open=false;};
  const form=field('project-form');form.resets=0;form.reset=()=>{form.resets++;};
  const sandbox=vm.createContext({URLSearchParams,encodeURIComponent,
    location:{search:'?machine=linux',pathname:'/',assign:url=>navigations.push(url)},
    $:field,Option:class {constructor(text,value){this.text=text;this.value=value;this.disabled=false;}},
    state:{},currentProject:'old-workspace',projectApplying:false,deployApplying:false,resourceApplying:false,settingsApplying:false,pending:new Set(),sessionToken:null,
    refresh:async force=>refreshes.push(force),toast:()=>{},navigate:route=>navigations.push(route),
    fetch:async(path,options)=>{
      requests.push({path,...options});
      if(path==='/api/session')return {ok:true,status:200,json:async()=>({token:'local-session-token'})};
      return transport?transport(path,options):{ok:true,status:200,json:async()=>({})};
    }});
  vm.runInContext(machinesSource,sandbox);
  vm.runInContext(source.slice(source.indexOf('async function api('),source.indexOf('function showError(')),sandbox);
  const workspaceStart=source.indexOf('function projectError(');
  vm.runInContext(source.slice(workspaceStart,source.indexOf('document.querySelectorAll("[data-create-workspace]")',workspaceStart)),sandbox);
  vm.runInContext(`savedMachines=[
    {profile:{id:"linux",name:"Linux"},identity:{},revision:"linux-saved-newer"},
    {profile:{id:"mac",name:"Build Mac"},identity:{},revision:"mac-saved"},
    {profile:{id:"untested",name:"Untested"},identity:null,revision:"untested-saved"}
  ];activeMachine={profile:{id:"linux",name:"Linux"},identity:{},revision:"linux-open-pinned"};openWorkspaceForm();`,sandbox);
  field('project-id').value=' tools ';field('project-name').value=' Build tools ';field('project-description').value=' Shared services ';
  sandbox.event={preventDefault(){}};
  return {sandbox,field,controls,requests,navigations,refreshes,submit:()=>vm.runInContext('createWorkspace(event)',sandbox)};
}
const routing=workspaceFixture();
await vm.runInContext('post("/api/projects",{id:"local"},"")',routing.sandbox);
assert.equal(routing.requests[0].path,'/api/session','A remote view still obtains its session from the local controller');
assert.equal(routing.requests[1].path,'/api/projects','Explicit This computer never falls back to the viewed remote machine');
assert.equal(routing.requests[1].headers['X-Init-Machine-Revision'],undefined);
assert.equal(routing.requests[1].headers['X-Init-Token'],'local-session-token');
await vm.runInContext('post("/api/projects",{id:"current"})',routing.sandbox);
assert.equal(routing.requests[2].path,'/api/machines/linux/proxy/projects');
assert.equal(routing.requests[2].headers['X-Init-Machine-Revision'],'linux-open-pinned','The current view keeps its pinned connection revision');
await vm.runInContext('post("/api/projects",{id:"foreign"},"mac")',routing.sandbox);
assert.equal(routing.requests[3].path,'/api/machines/mac/proxy/projects');
assert.equal(routing.requests[3].headers['X-Init-Machine-Revision'],'mac-saved','Another destination uses that saved connection revision');
assert.equal(routing.requests.filter(request=>request.path==='/api/session').length,1);

const destinations=workspaceFixture();
assert.equal(destinations.field('project-machine').value,'linux','New workspace starts on the machine being viewed');
assert.equal(destinations.field('project-machine').options[0].value,'');
assert.equal(destinations.field('project-machine').options.find(option=>option.value==='untested').disabled,true);
for(const destination of ['untested','removed']) {
  destinations.field('project-machine').value=destination;
  await destinations.submit();
  assert.match(destinations.field('project-error').textContent,/tested machine/);
  assert.equal(destinations.requests.length,0,'Invalid destinations cannot create a workspace or silently fall back');
  assert.equal(destinations.navigations.length,0);
}

let acceptCreate,creationRequested;
const requested=new Promise(resolve=>{creationRequested=resolve;});
const foreign=workspaceFixture(()=>{creationRequested();return new Promise(resolve=>{acceptCreate=resolve;});});
foreign.field('project-machine').value='mac';
const creating=foreign.submit();
await requested;
assert.ok(foreign.controls.every(control=>control.disabled),'Accepted creation locks its fields and destination');
await foreign.submit();
assert.equal(foreign.requests.filter(request=>request.method==='POST').length,1,'Repeated submit cannot create another workspace');
assert.equal(foreign.requests[1].path,'/api/machines/mac/proxy/projects');
assert.deepEqual(JSON.parse(foreign.requests[1].body),{id:'tools',name:'Build tools',description:'Shared services'});
acceptCreate({ok:true,status:200,json:async()=>({})});await creating;
assert.deepEqual(foreign.navigations,['/?machine=mac#/p/tools/workspaces'],'Successful foreign creation opens that machine and workspace');
assert.equal(foreign.refreshes.length,0,'Foreign creation never refreshes the controller being left');
assert.equal(foreign.field('project-dialog').closed,1);

const failed=workspaceFixture(()=>({ok:false,status:409,json:async()=>({error:'Machine connection changed. Test it again.'})}));
failed.field('project-machine').value='mac';await failed.submit();
assert.match(failed.field('project-error').textContent,/Machine connection changed/);
assert.equal(failed.field('project-error').hidden,false);
assert.equal(failed.field('project-machine').value,'mac');
assert.equal(failed.field('project-id').value,' tools ');
assert.equal(failed.field('project-name').value,' Build tools ');
assert.equal(failed.field('project-description').value,' Shared services ');
assert.equal(failed.field('project-dialog').open,true);
assert.equal(failed.field('project-form').resets,0);
assert.ok(failed.controls.every(control=>!control.disabled),'An error restores the retained form for correction');
assert.equal(failed.requests.filter(request=>request.method==='POST').length,1,'An error never retries on a different machine');
assert.equal(failed.refreshes.length,0);assert.equal(failed.navigations.length,0);

const busy=workspaceFixture();busy.sandbox.pending.add('web');busy.field('project-machine').value='mac';await busy.submit();
assert.equal(busy.requests.length,0,'A pending mutation prevents leaving for another machine during creation');
assert.match(busy.field('project-error').textContent,/current change/);
busy.field('project-machine').value='linux';await busy.submit();
assert.equal(busy.requests[1].path,'/api/machines/linux/proxy/projects','Creation on the current machine remains available');
assert.deepEqual(busy.refreshes,[true]);assert.deepEqual(busy.navigations,['workspaces']);
assert.equal(busy.sandbox.currentProject,'tools');assert.equal(busy.field('project-form').resets,1);
console.log('Workspace creation checks passed: explicit destinations, revision isolation, retained errors, correct navigation, and pending-write guards.');

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
assert.equal(vm.runInContext('routeHref("github")',routeContext),'#/github','GitHub setup is machine-wide and keeps the selected workspace');
assert.equal(vm.runInContext('routeHref("workspaces")',routeContext),'#/p/all/workspaces');
console.log('Recorded utilization checks passed: unique targets, nulls, weighted bounds, retained-history errors, scoped/global navigation.');

const navContext=vm.createContext({});
vm.runInContext(source.slice(source.indexOf('function statusGroup('),source.indexOf('function statusGroups('))+source.slice(source.indexOf('function workspaceNavGroups('),source.indexOf('let renderedWorkspaceNav')),navContext);
navContext.services=[
  {id:'web',name:'Web',status:'running',project:'tools',cpu_percent:4,memory_mb:120,sampled_at:1},
  {id:'api',name:'API',status:'failed',project:'tools'},
  {id:'cron',status:'exited',kind:'job'},
  {id:'stray',status:'running',project:'legacy-group'},
];
navContext.projects=[{id:'tools',name:'Build tools'},{id:'empty',name:'Empty'}];
const navGroups=vm.runInContext('workspaceNavGroups(services,projects)',navContext);
const idsOf=list=>Array.from(list,item=>item.id);
assert.deepEqual(idsOf(navGroups),['__default__','tools','empty','legacy-group'],'Default first, declared order, undeclared workspaces last');
assert.deepEqual(Array.from(navGroups).flatMap(group=>idsOf(group.services)).sort(),['api','cron','stray','web'],'Each service appears exactly once');
assert.deepEqual(idsOf(navGroups[1].services),['api','web'],'Services are ordered by name, not by state order');
assert.equal(navGroups[2].services.length,0,'Declared workspaces without services stay visible for Add service');
assert.equal(navGroups[3].name,'legacy-group');
const signature=vm.runInContext('workspaceNavSignature(workspaceNavGroups(services,projects))',navContext);
navContext.services[0]={...navContext.services[0],cpu_percent:80,memory_mb:900,sampled_at:2};
assert.equal(vm.runInContext('workspaceNavSignature(workspaceNavGroups(services,projects))',navContext),signature,'Usage polling must not rebuild the sidebar');
navContext.services[0]={...navContext.services[0],health:'unhealthy'};
assert.notEqual(vm.runInContext('workspaceNavSignature(workspaceNavGroups(services,projects))',navContext),signature,'A service needing attention updates the sidebar');
console.log('Workspace navigation checks passed: complete grouping, stable ordering, empty and undeclared workspaces, poll-stable rebuilds.');

const expandedScopes=[];
const scopeContext=vm.createContext({currentProject:'',currentTarget:'',setWorkspaceExpanded:id=>expandedScopes.push(id)});
const syncStart=source.indexOf('let renderedScope=');
vm.runInContext(source.slice(syncStart,source.indexOf('$("workspace-tree").addEventListener',syncStart)),scopeContext);
const visit=project=>{scopeContext.currentProject=project;vm.runInContext('syncScope()',scopeContext);return scopeContext.currentTarget;};
visit('home-lab');scopeContext.currentTarget='Docker';
assert.equal(visit('home-lab'),'Docker','Polling and tab changes within a workspace keep its target filter');
assert.equal(visit('automation'),'','Changing workspace clears the previous target filter');
scopeContext.currentTarget='Docker';
assert.equal(visit(''),'','All workspaces is a different scope');
scopeContext.currentTarget='Docker';
assert.equal(visit('home-lab'),'','Back or a deep link into another workspace also clears the filter');
assert.deepEqual(Array.from(expandedScopes),['home-lab','automation','home-lab'],'Entering a workspace expands it in the sidebar');

const tableFocusContext=vm.createContext({});
const tableFocusStart=source.indexOf('function workspaceTableFocus(');
vm.runInContext(source.slice(tableFocusStart,source.indexOf('function renderState(',tableFocusStart)),tableFocusContext);
const actionButton=container=>({dataset:{projectAction:'deploy',project:'home-lab'},closest:selector=>selector==='#workspace-cards'&&container==='table'?{}:null});
tableFocusContext.sidebarButton=actionButton('sidebar');tableFocusContext.tableButton=actionButton('table');
assert.equal(vm.runInContext('workspaceTableFocus(sidebarButton)',tableFocusContext),null,'Sidebar actions are never refocused into the workspace table');
assert.equal(vm.runInContext('workspaceTableFocus(tableButton)?.project',tableFocusContext),'home-lab');
assert.equal(vm.runInContext('workspaceTableFocus(null)',tableFocusContext),null);
console.log('Scope checks passed: target filters reset across workspaces, sidebar focus stays out of the workspace table.');

const machineTools={open:false};
const drawerControls=[
  {id:'close'}, {id:'theme'}, {id:'workspace'},
  {id:'machine-tools',tag:'summary',disclosure:machineTools},
  {id:'machines',disclosure:machineTools}, {id:'settings',disclosure:machineTools},
].map(control=>({...control,
  // Browsers can report rectangles for links inside a closed native disclosure.
  getClientRects:()=>[{}],
  closest:selector=>selector==='details:not([open])'&&control.disclosure&&!control.disclosure.open?control.disclosure:null,
  matches:selector=>control.tag===selector,
}));
const drawerContext=vm.createContext({$:()=>({querySelectorAll:()=>drawerControls})});
const drawerFocusStart=source.indexOf('function navigationFocusables(');
vm.runInContext(source.slice(drawerFocusStart,source.indexOf('\n}',drawerFocusStart)+2),drawerContext);
const drawerFocusOrder=()=>Array.from(vm.runInContext('navigationFocusables()',drawerContext),control=>control.id);
assert.deepEqual(drawerFocusOrder(),['close','theme','workspace','machine-tools'],'Closed Machine tools keeps its summary in focus order and excludes hidden links even when they have rectangles');
machineTools.open=true;
assert.deepEqual(drawerFocusOrder(),['close','theme','workspace','machine-tools','machines','settings'],'Opening Machine tools restores its links in document order');
console.log('Drawer focus checks passed: closed disclosure links stay out of the keyboard focus order.');
