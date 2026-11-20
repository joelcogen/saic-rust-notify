import {vehicleReadings, display, relativeTime, vehicleState, TOKEN_KEY, loadToken, saveToken} from '/dashboard-model.mjs';

const $ = selector => document.querySelector(selector);
let storage;
try { storage = window.localStorage; } catch { /* Browsers can prohibit storage. */ }
let token = loadToken(storage);
let snapshot = null;
let readings = {};
let busy = false;
let loading = false;
let session = 0;
let unavailable = false;
let storageWarning = false;
const commands = [...document.querySelectorAll('[data-action]'), $('#start-climate')];

function message(text, tone = 'secondary') {
  const notice = $('#message');
  notice.className = `alert alert-${tone} mb-3`;
  notice.textContent = text;
  notice.hidden = !text;
}
function loginMessage(text, error = false) {
  const notice = $('#login-message');
  notice.textContent = text;
  notice.dataset.quality = error ? 'failed' : 'unknown';
}
function showLogin(text = '') {
  $('#dashboard').hidden = true;
  $('#refresh-group').hidden = true;
  $('#login').hidden = false;
  loginMessage(text, Boolean(text));
  if ($('#climate-dialog').open) $('#climate-dialog').close();
  snapshot = null;
  readings = {};
  render();
}
function showDashboard() {
  $('#login').hidden = true;
  $('#dashboard').hidden = false;
  $('#refresh-group').hidden = false;
  $('#token').value = '';
}
function updateButtons() {
  for (const button of commands) button.disabled = busy || unavailable || snapshot?.ready !== true;
  $('#refresh').textContent = busy ? '…' : '↻';
  $('#dashboard').setAttribute('aria-busy', String(busy));
}
function renderTime() {
  const stamp = readings['vehicle/refresh/lastVehicleState']?.value;
  $('#updated').textContent = relativeTime(stamp);
  if (typeof stamp === 'string' && Number.isFinite(Date.parse(stamp))) {
    $('#updated').dateTime = stamp;
    $('#updated').title = new Date(stamp).toLocaleString();
  } else {
    $('#updated').removeAttribute('datetime');
    $('#updated').removeAttribute('title');
  }
}
function renderConnection() {
  const vehicles = (snapshot?.sources ?? []).filter(source => source.kind === 'vehicle');
  const [text, quality] = unavailable ? ['Unavailable', 'failed']
    : !snapshot ? ['Connecting…', 'unknown']
    : !snapshot.connected ? ['Disconnected', 'failed']
    : vehicles.length > 1 ? ['Multiple vehicles', 'unknown']
    : snapshot.ready ? ['Connected', 'healthy'] : ['Not ready', 'unknown'];
  const connection = $('#connection');
  if (connection.textContent !== text) connection.textContent = text;
  connection.dataset.quality = quality;
}
function render() {
  readings = vehicleReadings(snapshot);
  let hasStaleReading = false;
  for (const element of document.querySelectorAll('[data-reading]')) {
    const reading = readings[`vehicle/${element.dataset.reading}`];
    const value = display(reading?.value, element.dataset.format);
    const quality = value === 'Unknown' ? 'unknown'
      : unavailable || !reading.fresh ? 'stale'
      : value === 'Locked' || value === 'Closed' ? 'healthy'
      : value === 'Off' ? 'off' : 'known';
    element.textContent = quality === 'unknown' ? element.dataset.unknown ?? value : value;
    hasStaleReading ||= quality === 'stale';
    element.dataset.quality = quality;
    element.title = quality === 'unknown' ? 'No usable reading reported'
      : quality === 'stale' ? 'Stale vehicle reading' : 'Last known vehicle reading';
  }
  const state = vehicleState(readings);
  const staleState = state.text !== 'Unknown' && (unavailable || state.stale);
  $('#vehicle-state').textContent = state.text;
  $('#vehicle-state').dataset.quality = state.text === 'Unknown' ? 'unknown' : staleState ? 'stale' : 'known';
  $('#vehicle-state').title = staleState ? 'Stale vehicle reading' : '';
  $('#stale-notice').hidden = !(staleState || hasStaleReading);
  renderConnection();
  renderTime();
  updateButtons();
}
async function request(path, method = 'GET', body, credential = token) {
  let response;
  try {
    response = await fetch(path, {
      method, headers: {Authorization: `Bearer ${credential}`, ...(body === undefined ? {} : {'Content-Type':'application/json'})},
      body: body === undefined ? undefined : JSON.stringify(body),
      cache:'no-store', credentials:'omit', redirect:'error',
      signal: AbortSignal.timeout(method === 'POST' ? 370000 : 15000),
    });
  } catch {
    throw new Error(method === 'POST' ? 'Connection lost. The action may have completed. Check vehicle state before trying again.' : 'Cannot reach the service. Reconnecting automatically.');
  }
  if (response.status === 401) {
    const error = new Error('The saved token was rejected. Enter a valid token to reconnect.');
    error.unauthorized = true;
    throw error;
  }
  if (!response.ok) throw new Error((await response.text()).slice(0, 500) || `Request failed (${response.status}).`);
  return method === 'GET' ? response.json() : response.text();
}
function handleError(error) {
  unavailable = true;
  if (error.unauthorized) {
    token = '';
    session++;
    showLogin(error.message);
    $('#token').focus();
    // Keep the persisted value until an explicit replacement/forget, including
    // during temporary configuration failures. It is never put into the URL.
  } else {
    message(error.message, 'danger');
    render();
  }
}
async function loadStatus(preserveMessage = false) {
  if (!token || loading || document.hidden) return;
  loading = true;
  const generation = session;
  try {
    const result = await request('/api/status');
    if (generation !== session) return;
    snapshot = result;
    const recovered = unavailable;
    unavailable = false;
    showDashboard();
    render();
    if (recovered && !preserveMessage && $('#message').textContent.startsWith('Cannot reach the service.')) {
      if (storageWarning) message('Browser storage is unavailable. Your token will only be kept for this visit.');
      else message('Connection restored.', 'success');
    }
  } catch (error) {
    if (generation === session) handleError(error);
  } finally { loading = false; }
}
$('#token-form').addEventListener('submit', async event => {
  event.preventDefault();
  const candidate = $('#token').value.trim();
  if (!candidate) return;
  $('#connect').disabled = true;
  loginMessage('Connecting…');
  const generation = ++session;
  try {
    const result = await request('/api/status', 'GET', undefined, candidate);
    if (generation !== session) return;
    token = candidate;
    storageWarning = !saveToken(storage, token);
    snapshot = result;
    unavailable = false;
    showDashboard();
    message(storageWarning ? 'Browser storage is unavailable. Your token will only be kept for this visit.' : '');
    render();
    $('#dashboard-title').focus({preventScroll:true});
  } catch (error) { loginMessage(error.message, true); }
  finally { $('#connect').disabled = false; }
});
async function act(path, body) {
  if (busy || unavailable || snapshot?.ready !== true) return;
  busy = true;
  updateButtons();
  message(path === '/refresh' ? 'Refreshing vehicle data…' : path === '/locate' ? 'Requesting a light flash…' : 'Starting climate. This may take a few minutes…');
  try {
    const result = await request(path, 'POST', body);
    message(result, 'success');
  } catch (error) { handleError(error); }
  finally {
    busy = false;
    updateButtons();
    // Cached read only, including after uncertain command outcomes. Never replay.
    await loadStatus(true);
  }
}
for (const button of document.querySelectorAll('[data-action]')) button.addEventListener('click', () => act(button.dataset.action));
$('#open-climate').addEventListener('click', () => $('#climate-dialog').showModal());
$('#close-climate').addEventListener('click', () => $('#climate-dialog').close());
$('#climate-form').addEventListener('submit', event => {
  event.preventDefault();
  const value = $('#temperature').value;
  const body = value === '' ? undefined : {temperature:Number(value)};
  if (body && !Number.isFinite(body.temperature)) return;
  $('#climate-dialog').close();
  act('/climate/start', body);
});
document.addEventListener('visibilitychange', () => { if (!document.hidden) loadStatus(); });
window.addEventListener('storage', event => {
  if ((event.key === TOKEN_KEY || event.key === null) && !busy) {
    session++;
    token = loadToken(storage);
    if (token) loadStatus(); else showLogin();
  }
});
setInterval(() => loadStatus(), 10000);
setInterval(renderTime, 1000);
if (token) {
  showDashboard();
  render();
  loadStatus();
} else showLogin();
