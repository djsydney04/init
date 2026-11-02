import assert from 'node:assert/strict';
import {readFileSync, writeFileSync, mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import vm from 'node:vm';
const source=readFileSync(new URL('../assets/app.js',import.meta.url),'utf8');
const start=source.indexOf('function githubWorkflow('), end=source.indexOf('let generatedWorkflow=',start);
assert.ok(start>=0&&end>start);
const generate=vm.runInNewContext(source.slice(start,end)+'\ngithubWorkflow');
const settings={service:'web',branch:'release/production',runner:'init-production',environment:'production',workspace:'/tmp/a b/$(false)',config:"/tmp/a'b/services.yaml"};
const workflow=generate(settings);
assert.match(workflow,/branches: \["release\/production"\]/);
assert.match(workflow,/if: github.ref == 'refs\/heads\/release\/production'/);
assert.match(workflow,/cancel-in-progress: false/);
assert.match(workflow,/runs-on: \[self-hosted, "init-production"\]/);
assert.doesNotMatch(workflow,/pull_request|contents: write/);
for(const [field,value] of [['branch',"main' || true"],['runner','${{ secrets.X }}'],['environment','prod\npermissions: write-all'],['workspace','${{ secrets.X }}'],['service','--help'],['config','/tmp/a\nfile']]) assert.throws(()=>generate({...settings,[field]:value}));
// Execute only the generated shell with a stub: paths must remain literal arguments.
const dir=mkdtempSync(join(tmpdir(),'init-workflow-'));
try{
 writeFileSync(join(dir,'init'),'#!/bin/sh\nprintf "%s\\0" "$@"\n',{mode:0o700});
 const run=workflow.split('        run: |\n')[1].split('\n').map(line=>line.slice(10)).join('\n');
 const env={...process.env,PATH:dir,GITHUB_SHA:'abc123',GITHUB_WORKSPACE:'/tmp/repo with spaces'};
 for(const match of workflow.matchAll(/^      (INIT_[A-Z]+): (.+)$/gm))env[match[1]]=JSON.parse(match[2]);
 const result=spawnSync('/bin/bash',['-c',run],{env,encoding:'utf8'});
 assert.equal(result.status,0,result.stderr);
 assert.deepEqual(result.stdout.split('\0').slice(0,-1),['--workspace',settings.workspace,'--config',settings.config,'deploy-image','web','init-web:abc123','--build-context','/tmp/repo with spaces','--apply']);
}finally{rmSync(dir,{recursive:true,force:true});}
console.log('GitHub workflow checks passed: branch gating, expressions, permissions, and literal shell arguments.');
