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

assert.equal(vm.runInContext('machineUrl("mac", "services", true)',context),'/?machine=mac&deploy=1#/services');
assert.equal(vm.runInContext('machineUrl("", "services", true)',context),'/?deploy=1#/services');
const navigations=[], messages=[];
let invalidated=0;
const machineSelect={value:'linux'};
const deploymentSelect={value:'linux',replaceChildren(...options){this.options=options;}};
Object.assign(context, {
  pending:new Set(),deployApplying:false,resourceApplying:false,projectApplying:false,
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
