// Exercise the actual UI controller with a synthetic DOM/network boundary.
// This is not a substitute for browser layout QA.
import test from 'node:test';
import assert from 'node:assert/strict';
import vm from 'node:vm';
import {readFileSync} from 'node:fs';
import * as model from '../src/dashboard/dashboard-model.mjs';

const source = readFileSync(new URL('../src/dashboard/dashboard.js', import.meta.url), 'utf8').replace(/^import .*\n/, '');
const html = readFileSync(new URL('../src/dashboard/index.html', import.meta.url), 'utf8');
const reading = value => ({value, fresh:true});
const fixture = () => ({connected:true,ready:true,sources:[{kind:'vehicle',readings:{
  'vehicle/doors/locked':reading(true),
  'vehicle/drivetrain/parked':reading(true),
  'vehicle/drivetrain/charging':reading(false),
  'vehicle/climate/interiorTemperature':reading(5),
  'vehicle/refresh/lastVehicleState':reading('2026-11-15T12:00:00Z'),
}}]});
function element(dataset={}) {
  const handlers = new Map();
  return {dataset, hidden:false, disabled:false, value:'', textContent:'', open:false,
    classList:{toggle(){}}, title:'',
    addEventListener:(name,handler)=>handlers.set(name,handler),
    trigger(name){return handlers.get(name)?.({preventDefault(){}});},
    setAttribute(){},removeAttribute(){},focus(){},showModal(){this.open=true;},close(){this.open=false;},
  };
}
function harness({stored='', handler, denied=false}={}) {
  const nodes = new Map([...html.matchAll(/id="([^"]+)"/g)].map(match=>['#'+match[1],element()]));
  const labels = [...html.matchAll(/data-reading="([^"]+)" data-format="([^"]+)"/g)].map(match=>element({reading:match[1],format:match[2]}));
  const actions = ['/refresh','/locate'].map(action=>element({action}));
  nodes.set('#refresh',actions[0]);
  const calls = [];
  const values = new Map(stored?[[model.TOKEN_KEY,stored]]:[]);
  const intervals = new Map();
  const storage = {getItem:key=>values.get(key),setItem:(key,value)=>{if(denied)throw Error();values.set(key,value);},removeItem:key=>values.delete(key)};
  const document = {hidden:false,querySelector:selector=>nodes.get(selector),querySelectorAll:selector=>selector==='[data-action]'?actions:labels,addEventListener(){}};
  const context = vm.createContext({...model,document,window:{localStorage:storage,addEventListener(){}},AbortSignal,
    setInterval:(fn,ms)=>intervals.set(ms,fn),
    fetch:async(path,options)=>{calls.push({path,options});return handler?handler(path,options):response(options.method==='GET'?fixture():'Action completed');},
  });
  vm.runInContext(source,context);
  return {nodes,labels,actions,calls,values,intervals,context};
}
const response = (body,status=200) => ({ok:status>=200&&status<300,status,json:async()=>body,text:async()=>String(body)});
const settle = async()=>{for(let i=0;i<12;i++)await Promise.resolve();};
async function login(h, candidate='synthetic-token') {
  h.nodes.get('#token').value=candidate;
  await h.nodes.get('#token-form').trigger('submit');
}

test('first visit requests token; validation stores it; reload only reads cache', async()=>{
  const first=harness();
  assert.equal(first.calls.length,0);
  assert.equal(first.nodes.get('#dashboard').hidden,true);
  await login(first);
  assert.equal(first.values.get(model.TOKEN_KEY),'synthetic-token');
  assert.equal(first.calls[0].path,'/api/status');
  assert.equal(first.calls[0].options.headers.Authorization,'Bearer synthetic-token');
  assert.equal(first.calls[0].options.redirect,'error');
  assert.equal(first.nodes.get('#dashboard').hidden,false);
  assert.equal(first.nodes.get('#token').value,'');
  const reloaded=harness({stored:first.values.get(model.TOKEN_KEY)});
  await settle();
  assert.equal(reloaded.nodes.get('#login').hidden,true);
  assert.equal(reloaded.calls.length,1);
  assert.equal(reloaded.calls[0].options.method,'GET');
  assert.equal(reloaded.nodes.get('#vehicle-state').textContent,'Parked');
});
test('invalid tokens are not persisted and storage failure is explained',async()=>{
  const h=harness({handler:()=>response('Unauthorized',401)});
  await login(h,'wrong');
  assert.equal(h.values.size,0);
  assert.match(h.nodes.get('#login-message').textContent,/token was rejected/);
  assert.equal(h.nodes.get('#login-message').dataset.quality,'failed');
  const denied=harness({denied:true});
  await login(denied);
  assert.match(denied.nodes.get('#message').textContent,/storage is unavailable/);
  assert.equal(denied.nodes.get('#dashboard').hidden,false);
});
test('saved-token loading shows Connecting until cached status arrives',async()=>{
  let finish;
  const h=harness({stored:'synthetic-token',handler:()=>new Promise(resolve=>{finish=resolve;})});
  assert.equal(h.nodes.get('#connection').textContent,'Connecting…');
  assert.equal(h.nodes.get('#connection').dataset.quality,'unknown');
  assert.equal(h.actions[0].disabled,true);
  finish(response(fixture()));
  await settle();
  assert.equal(h.nodes.get('#connection').textContent,'Connected');
  assert.equal(h.nodes.get('#connection').dataset.quality,'healthy');
  assert.equal(h.actions[0].disabled,false);
});
test('stale readings keep their values and the last update through polling failures',async()=>{
  const initial=fixture();
  initial.sources[0].readings['vehicle/doors/locked'].fresh=false;
  initial.sources[0].readings['vehicle/drivetrain/parked'].fresh=false;
  let handler=()=>response(initial);
  const h=harness({stored:'synthetic-token',handler:()=>handler()});
  const reading=key=>h.labels.find(label=>label.dataset.reading===key);
  await settle();
  assert.equal(reading('doors/locked').textContent,'Locked');
  assert.equal(reading('doors/locked').dataset.quality,'stale');
  assert.equal(reading('drivetrain/soc').dataset.quality,'unknown');
  assert.equal(h.nodes.get('#vehicle-state').dataset.quality,'stale');
  assert.equal(h.nodes.get('#stale-notice').hidden,false);
  assert.equal(h.nodes.get('#updated').dateTime,'2026-11-15T12:00:00Z');

  handler=()=>{throw Error('synthetic network failure');};
  await h.intervals.get(10000)();
  assert.equal(h.nodes.get('#connection').textContent,'Unavailable');
  assert.equal(h.nodes.get('#connection').dataset.quality,'failed');
  assert.match(h.nodes.get('#message').className,/alert-danger/);
  assert.equal(reading('climate/interiorTemperature').textContent,'5°C');
  assert.equal(reading('climate/interiorTemperature').dataset.quality,'stale');
  assert.equal(h.nodes.get('#updated').dateTime,'2026-11-15T12:00:00Z');

  handler=()=>response(fixture());
  await h.intervals.get(10000)();
  assert.equal(h.nodes.get('#stale-notice').hidden,true);
  assert.equal(reading('doors/locked').dataset.quality,'healthy');
  assert.match(h.nodes.get('#message').className,/alert-success/);
});
test('actions use POST once, lock buttons and update only from cached status',async()=>{
  let finish;
  const h=harness({stored:'synthetic-token',handler:(path,options)=> options.method==='POST'?new Promise(resolve=>{finish=resolve;}):response(fixture())});
  await settle();
  const first=h.actions[1].trigger('click');
  const duplicate=h.actions[1].trigger('click');
  assert.equal(h.actions[0].disabled,true);
  assert.match(h.nodes.get('#message').className,/alert-secondary/);
  assert.equal(h.calls.filter(call=>call.options.method==='POST').length,1);
  finish(response('Demo lights flashed'));
  await first; await duplicate;
  assert.equal(h.actions[0].disabled,false);
  assert.equal(h.calls.at(-1).path,'/api/status');
  assert.equal(h.calls.at(-1).options.method,'GET');
  assert.equal(h.nodes.get('#message').textContent,'Demo lights flashed');
  assert.match(h.nodes.get('#message').className,/alert-success/);
});
test('climate dialog sends interior override only after explicit start',async()=>{
  const h=harness({stored:'synthetic-token'});
  await settle();
  h.nodes.get('#open-climate').trigger('click');
  assert.equal(h.nodes.get('#climate-dialog').open,true);
  assert.equal(h.calls.filter(call=>call.options.method==='POST').length,0);
  h.nodes.get('#temperature').value='-2';
  h.nodes.get('#climate-form').trigger('submit');
  await settle();
  const command=h.calls.find(call=>call.options.method==='POST');
  assert.equal(command.path,'/climate/start');
  assert.equal(command.options.body,'{"temperature":-2}');
  assert.equal(h.nodes.get('#climate-dialog').open,false);
});
test('unknown outcomes remain visible and are never replayed by status polling',async()=>{
  const h=harness({stored:'synthetic-token',handler:(path,options)=>{
    if(options.method==='POST')throw Error('synthetic network failure');
    return response(fixture());
  }});
  await settle();
  await h.actions[1].trigger('click');
  await h.intervals.get(10000)();
  assert.equal(h.calls.filter(call=>call.options.method==='POST').length,1);
  assert.match(h.nodes.get('#message').textContent,/action may have completed/);
});
test('authentication expiry hides vehicle data and header refresh',async()=>{
  let reject=false;
  const h=harness({stored:'synthetic-token',handler:()=>reject?response('',401):response(fixture())});
  await settle();
  reject=true;
  await h.intervals.get(10000)();
  assert.equal(h.nodes.get('#dashboard').hidden,true);
  assert.equal(h.nodes.get('#login').hidden,false);
  assert.equal(h.labels[0].textContent,'Unknown');
  assert.equal(h.nodes.get('#refresh-group').hidden,true);
});
test('disconnection and ambiguous vehicle selection disable commands',async()=>{
  for(const state of [
    {...fixture(),connected:false,ready:false},
    {...fixture(),ready:false,sources:[...fixture().sources,...fixture().sources]},
  ]){
    const h=harness({stored:'synthetic-token',handler:()=>response(state)});
    await settle();
    assert.equal(h.actions[0].disabled,true);
    await h.actions[0].trigger('click');
    assert.equal(h.calls.filter(call=>call.options.method==='POST').length,0);
    if(state.connected) assert.equal(h.labels[0].textContent,'Unknown');
  }
});
