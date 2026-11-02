import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

// Exercise the actual browser comparator without adding a frontend build system.
const source = readFileSync(new URL("../assets/app.js", import.meta.url), "utf8");
const start = source.indexOf("function compareServices(");
const end = source.indexOf("function renderServices()", start);
assert.ok(start >= 0 && end > start);
const compare = vm.runInNewContext(source.slice(start, end) + "\ncompareServices");
const services = [
  {id: "z", status: "stopped", cpu_percent: null, memory_mb: null},
  {id: "b", status: "running", cpu_percent: 0, memory_mb: 200},
  {id: "a", status: "running", cpu_percent: 10, memory_mb: 0},
  {id: "c", status: "running", cpu_percent: undefined, memory_mb: NaN},
];
const order = sort => [...services].sort((a,b) => compare(a,b,sort)).map(s => s.id);
assert.deepEqual(order("running"), ["a","b","c","z"]);
assert.deepEqual(order("name"), ["a","b","c","z"]);
assert.deepEqual(order("cpu"), ["a","b","c","z"]);
assert.deepEqual(order("memory"), ["b","a","c","z"]);
assert.ok(compare({id:"z",name:"Alpha"},{id:"a",name:"Zulu"},"name") < 0);
console.log("Service ordering checks passed (zero, missing, invalid readings, name tie-breaks).");
