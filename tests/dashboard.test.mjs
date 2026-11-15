import test from 'node:test';
import assert from 'node:assert/strict';
import {display, relativeTime, vehicleReadings, vehicleState, loadToken, saveToken, TOKEN_KEY} from '../src/dashboard/dashboard-model.mjs';

test('unknown data never becomes a reassuring off, closed or zero', () => {
  for (const format of ['open','on','lock','seat','rear-seat','pressure','temperature','climate','soc']) {
    for (const missing of [undefined,null,NaN,{},'unexpected']) assert.equal(display(missing, format), 'Unknown');
  }
  assert.equal(display(false, 'open'), 'Closed');
  assert.equal(display(true, 'open'), 'Open');
  assert.equal(display(0, 'seat'), 'Off');
  assert.equal(display(3, 'seat'), 'Level 3');
  assert.equal(display(255, 'seat'), 'Unknown');
  assert.equal(display(1, 'rear-seat'), 'On');
  assert.equal(display('__proto__', 'climate'), 'Unknown');
  assert.equal(display(2.6, 'pressure'), '2.6 bar');
  assert.equal(display(0, 'temperature'), '0°C');
});
test('relative timestamp describes the observation and handles absent/future time', () => {
  const now = Date.parse('2026-11-15T12:00:00Z');
  assert.equal(relativeTime('2026-11-15T11:58:00Z', now), '2m ago');
  assert.equal(relativeTime('2026-11-13T12:00:00Z', now), '2d ago');
  assert.equal(relativeTime('2026-11-15T12:01:00Z', now), 'Timestamp ahead');
  assert.equal(relativeTime(null, now), 'No data yet');
  assert.equal(relativeTime('invalid', now), 'No data yet');
});
test('multiple vehicles never combine readings in the diagram', () => {
  const source = {kind:'vehicle', readings:{'vehicle/doors/locked':{value:true,fresh:true}}};
  assert.deepEqual(vehicleReadings({sources:[source]}), source.readings);
  assert.deepEqual(vehicleReadings({sources:[source,source],readings:source.readings}), {});
  assert.deepEqual(vehicleReadings({sources:[]}), {});
});
test('driving, parked and charging preserve unknowns and freshness', () => {
  const r = (parked, charging, fresh=true) => ({'vehicle/drivetrain/parked':{value:parked,fresh},'vehicle/drivetrain/charging':{value:charging,fresh}});
  assert.deepEqual(vehicleState(r(true,false)), {text:'Parked',stale:false});
  assert.deepEqual(vehicleState(r(false,false)), {text:'Driving',stale:false});
  assert.deepEqual(vehicleState(r(true,true,false)), {text:'Charging',stale:true});
  assert.equal(vehicleState(r(true,undefined)).text, 'Unknown');
  assert.equal(vehicleState({}).text, 'Unknown');
});
test('tokens survive a new browser session and storage denial is explicit', () => {
  const data = new Map();
  const storage = {getItem:key=>data.get(key), setItem:(key,value)=>data.set(key,value)};
  assert.equal(loadToken(storage), '');
  assert.equal(saveToken(storage, 'synthetic-token'), true);
  assert.equal(data.get(TOKEN_KEY), 'synthetic-token');
  assert.equal(loadToken(storage), 'synthetic-token');
  const denied = {getItem(){throw Error();},setItem(){throw Error();}};
  assert.equal(loadToken(denied), '');
  assert.equal(saveToken(denied, 'synthetic-token'), false);
});
