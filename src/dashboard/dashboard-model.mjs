// No browser or network dependencies: shared by the UI and offline tests.
export function vehicleReadings(snapshot) {
  const sources = (snapshot?.sources ?? []).filter(source => source.kind === 'vehicle');
  return sources.length === 1 ? sources[0].readings ?? {} : {};
}
export function display(value, format) {
  if (value === undefined || value === null) return 'Unknown';
  if (format === 'open') return typeof value === 'boolean' ? (value ? 'Open' : 'Closed') : 'Unknown';
  if (format === 'on') return typeof value === 'boolean' ? (value ? 'On' : 'Off') : 'Unknown';
  if (format === 'lock') return typeof value === 'boolean' ? (value ? 'Locked' : 'Unlocked') : 'Unknown';
  if (format === 'climate') {
    const states = {off:'Off', on:'On', front:'Defrost', blowingonly:'Fan only'};
    return Object.hasOwn(states, value) ? states[value] : 'Unknown';
  }
  if (typeof value !== 'number' || !Number.isFinite(value)) return 'Unknown';
  if (format === 'seat') return Number.isInteger(value) && value >= 0 && value <= 3 ? (value === 0 ? 'Off' : `Level ${value}`) : 'Unknown';
  if (format === 'rear-seat') return Number.isInteger(value) && value >= 0 && value <= 3 ? (value === 0 ? 'Off' : 'On') : 'Unknown';
  if (format === 'temperature') return `${value.toLocaleString(undefined, {maximumFractionDigits:1})}°C`;
  if (format === 'pressure') return `${value.toFixed(1)} bar`;
  if (format === 'soc') return `${Math.round(value)}%`;
  return 'Unknown';
}
export function relativeTime(stamp, now = Date.now()) {
  if (typeof stamp !== 'string') return 'No data yet';
  const milliseconds = Date.parse(stamp);
  if (!Number.isFinite(milliseconds)) return 'No data yet';
  const seconds = Math.floor((now - milliseconds) / 1000);
  if (seconds < 0) return 'Timestamp ahead';
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`;
  return `${Math.floor(seconds / 86400)}d ago`;
}
export function vehicleState(readings) {
  const charge = readings['vehicle/drivetrain/charging'];
  const parked = readings['vehicle/drivetrain/parked'];
  const state = charge?.value === true ? ['Charging', [charge]]
    : parked?.value === false ? ['Driving', [parked]]
    : parked?.value === true && charge?.value === false ? ['Parked', [parked, charge]]
    : ['Unknown', []];
  return {text: state[0], stale: state[1].some(reading => !reading.fresh)};
}
export const TOKEN_KEY = 'saic.dashboard.token';
export function loadToken(storage) {
  try { return storage.getItem(TOKEN_KEY) ?? ''; } catch { return ''; }
}
export function saveToken(storage, token) {
  try { storage.setItem(TOKEN_KEY, token); return true; } catch { return false; }
}
