import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';

const source = readFileSync(new URL('../assets/updater.js', import.meta.url), 'utf8');
const html = readFileSync(new URL('../assets/index.html', import.meta.url), 'utf8');
const ids = ['desktop-update', 'desktop-update-title', 'desktop-update-description', 'desktop-update-progress', 'desktop-update-install', 'desktop-update-skip'];
for (const id of ids) assert.ok(html.includes(`id="${id}"`), `Updater DOM target ${id} must exist`);
assert.match(html, /id="desktop-update"[^>]*\bhidden\b/, 'browser UI starts hidden');

function harness(bridge) {
  const elements = Object.fromEntries(ids.map(id => [id, {
    hidden: true, disabled: false, textContent: '', attributes: {}, handlers: {},
    setAttribute(key, value) { this.attributes[key] = value; },
    removeAttribute(key) { delete this[key]; },
    addEventListener(name, fn) { this.handlers[name] = fn; },
  }]));
  const intervals = [];
  const window = {setInterval: fn => intervals.push(fn)};
  if (bridge) window.__INIT_UPDATER__ = bridge;
  vm.runInNewContext(source, {window, document: {getElementById: id => elements[id]}});
  return {elements, intervals};
}
const flush = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };
const browser = harness();
assert.equal(browser.elements['desktop-update'].hidden, true, 'browser must never offer desktop updates');
assert.equal(browser.intervals.length, 0, 'browser does not poll native controls');

let state = {status: 'available', version: '0.6.0', dismissed: false, blocked: null};
let installs = 0;
let skips = 0;
let resolveInstall;
const bridge = {
  status: async () => state,
  install: () => {
    installs++;
    return new Promise(resolve => { resolveInstall = resolve; });
  },
  skip: async () => {
    skips++;
    state = {...state, dismissed: true};
    return state;
  },
};
let {elements, intervals} = harness(bridge);
await flush();
const card = elements['desktop-update'];
const install = elements['desktop-update-install'];
const skip = elements['desktop-update-skip'];
const description = elements['desktop-update-description'];
const progress = elements['desktop-update-progress'];
assert.equal(card.hidden, false, 'available update appears');
assert.equal(install.textContent, 'Update');
assert.match(description.textContent, /0\.6\.0/);
assert.match(description.textContent, /restart/);
skip.handlers.click();
await flush();
assert.equal(skips, 1);
assert.equal(card.hidden, true, 'Skip hides the update');
intervals[0]();
await flush();
assert.equal(card.hidden, true, 'native dismissal survives UI refresh');
({elements, intervals} = harness(bridge));
await flush();
assert.equal(elements['desktop-update'].hidden, true, 'page reload keeps session Skip');
state = {...state, dismissed: false};
intervals[0]();
await flush();
assert.equal(elements['desktop-update'].hidden, false, 'new app session can offer the update again');

const updateButton = elements['desktop-update-install'];
updateButton.handlers.click();
updateButton.handlers.click();
assert.equal(installs, 1, 'double click must start one native install');
assert.equal(updateButton.disabled, true);
state = {status: 'downloading', version: '0.6.0', downloaded: 50, total: 200};
resolveInstall(state);
await flush();
assert.equal(updateButton.hidden, true);
assert.equal(elements['desktop-update-skip'].hidden, true, 'Skip must not interrupt installation');
assert.equal(elements['desktop-update-progress'].value, 50);
assert.match(elements['desktop-update-description'].textContent, /25%/);
state = {...state, total: null};
intervals[0]();
await flush();
assert.equal(elements['desktop-update-progress'].value, undefined, 'unknown total must be indeterminate');
state = {status: 'error', version: '0.6.0', message: '<script>network failed</script>'};
intervals[0]();
await flush();
assert.equal(updateButton.textContent, 'Retry');
assert.equal(updateButton.hidden, false);
assert.equal(elements['desktop-update-description'].textContent, state.message, 'untrusted errors are rendered as text');
assert.equal(elements['desktop-update'].attributes['aria-busy'], 'false');
bridge.install = async () => {
  installs++;
  state = {status: 'downloading', version: '0.6.0', downloaded: 0, total: null};
  return state;
};
updateButton.handlers.click();
await flush();
assert.equal(installs, 2, 'Retry starts another native attempt after failure');
assert.equal(updateButton.hidden, true);
state = {status: 'available', version: '0.6.0', blocked: 'Close the separately started daemon'};
intervals[0]();
await flush();
assert.equal(updateButton.disabled, true, 'external-daemon preflight blocks replacement');
assert.equal(elements['desktop-update-skip'].disabled, false);
assert.equal(elements['desktop-update-description'].textContent, state.blocked);
state = {status: 'restarting', version: '0.6.0'};
intervals[0]();
await flush();
assert.equal(elements['desktop-update-title'].textContent, 'Restarting Init');
assert.equal(updateButton.hidden, true);
console.log('Updater UI checks passed: desktop-only prompt, session Skip, duplicate protection, progress, safe failure/retry, and external-daemon preflight.');
