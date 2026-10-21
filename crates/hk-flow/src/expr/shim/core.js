// Expression scope: the `$`-variables n8n documents for expressions, built lazily per
// evaluation from host data. Returns the API object the Rust side calls.

const state = { gen: -1, cache: new Map() };
const compiled = new Map();
const J = (s) => (s === '' ? undefined : JSON.parse(s));
const memo = (key, f) => {
  const c = state.cache;
  if (!c.has(key)) c.set(key, f());
  return c.get(key);
};
const inputAll = (port) => memo('in' + port, () => J(hostCall('input', String(port))) || []);

function nodeItems(name, branch, run) {
  return memo('n/' + name + '/' + branch + '/' + run, () => J(hostCall('run', name, String(branch), String(run))) || []);
}
function nodeParams(name) {
  const p = hostCall('nodeParams', name);
  if (p !== '') return JSON.parse(p);
  if (Number(hostCall('runCount', name)) > 0) return {};
  throw new Error("Referenced node '" + name + "' doesn't exist");
}

function buildScope(m) {
  const cur = () => inputAll(0)[m.itemIndex];
  const paired = (name, itemIndex) => {
    const [run, out, idx] = hostCall('paired', name, String(itemIndex)).split(',').map(Number);
    return nodeItems(name, out, run)[idx];
  };
  const nodeApi = (name) => ({
    get isExecuted() { return Number(hostCall('runCount', name)) > 0; },
    all(branch = 0, run = -1) { return nodeItems(name, branch, run); },
    first(branch = 0, run = -1) { return nodeItems(name, branch, run)[0]; },
    last(branch = 0, run = -1) { const a = nodeItems(name, branch, run); return a[a.length - 1]; },
    get item() { return paired(name, m.itemIndex); },
    itemMatching(i) { return paired(name, i); },
    get params() { return nodeParams(name); },
    get context() { return {}; },
  });
  const legacy = (name) => {
    const pick = () => {
      try { return paired(name, m.itemIndex); } catch (e) {
        const all = nodeItems(name, 0, -1);
        return all[m.itemIndex] || all[0];
      }
    };
    return {
      get json() { const i = pick(); return i ? i.json : undefined; },
      get data() { const i = pick(); return i ? i.json : undefined; },
      get binary() { const i = pick(); return i ? i.binary : undefined; },
      get parameter() { return nodeParams(name); },
      get context() { return {}; },
      get runIndex() { return Number(hostCall('runCount', name)) - 1; },
    };
  };
  const nodeProxy = new Proxy({}, {
    get: (_, name) => (typeof name === 'string' ? legacy(name) : undefined),
    has: (_, name) => typeof name === 'string',
  });
  const inputApi = {
    get item() { return cur(); },
    first(b = 0) { return inputAll(b)[0]; },
    last(b = 0) { const a = inputAll(b); return a[a.length - 1]; },
    all(b = 0) { return inputAll(b); },
    get params() { return memo('params', () => JSON.parse(hostCall('params'))); },
    get context() { return {}; },
  };
  const customData = {};
  const S = Object.create(null);
  const get = (n, f) => Object.defineProperty(S, n, { get: f, enumerable: true, configurable: true });
  get('$json', () => { const it = cur(); return it ? it.json : {}; });
  get('$binary', () => { const it = cur(); return it && it.binary ? it.binary : {}; });
  get('$input', () => inputApi);
  S.$ = nodeApi;
  get('$node', () => nodeProxy);
  S.$items = (name, out = 0, run = -1) => (name === undefined ? inputAll(0) : nodeItems(name, out, run));
  get('$itemIndex', () => m.itemIndex);
  get('$position', () => m.itemIndex);
  get('$runIndex', () => m.runIndex);
  get('$parameter', () => inputApi.params);
  get('$prevNode', () => memo('prev', () => J(hostCall('prevNode'))));
  get('$workflow', () => memo('wf', () => JSON.parse(hostCall('workflow'))));
  get('$execution', () => memo('exec', () => {
    const e = JSON.parse(hostCall('execution'));
    e.customData = {
      set: (k, v) => { customData[k] = v; },
      get: (k) => customData[k],
      setAll: (o) => { for (const k of Object.keys(o)) customData[k] = o[k]; },
      getAll: () => Object.assign({}, customData),
    };
    return e;
  }));
  get('$env', () => memo('env', () => JSON.parse(hostCall('env'))));
  get('$vars', () => memo('vars', () => JSON.parse(hostCall('vars'))));
  get('$now', () => DateTime.now());
  get('$today', () => DateTime.now().startOf('day'));
  S.$jmespath = (a, b) => {
    const [obj, q] = typeof a === 'string' && typeof b !== 'string' ? [b, a] : [a, b];
    return J(hostCall('jmespath', JSON.stringify(obj), String(q)));
  };
  S.$if = (cond, yes, no) => (cond ? yes : no);
  S.$min = Math.min;
  S.$max = Math.max;
  S.$ifEmpty = (v, fallback) => (v === undefined || v === null || v === '' || (Array.isArray(v) && v.length === 0) ||
    (typeof v === 'object' && !Array.isArray(v) && Object.keys(v).length === 0 && !(v instanceof DateTime)) ? fallback : v);
  S.DateTime = DateTime;
  S.Duration = Duration;
  S.Interval = Interval;
  return S;
}

function compile(src) {
  let f = compiled.get(src);
  if (!f) {
    // `with` + a null-prototype scope: `$json` etc. resolve there, everything else falls
    // through to the realm globals (Math, JSON, ...).
    f = new Function('__s', 'with(__s){return (' + src + '\n)}');
    compiled.set(src, f);
  }
  return f;
}

function ser(v) {
  if (v === undefined || typeof v === 'function' || typeof v === 'symbol') return '';
  const s = JSON.stringify(v, (k, x) => (typeof x === 'bigint' ? x.toString() : x));
  return s === undefined ? '' : s;
}
function render(v) {
  if (v === undefined || v === null) return '';
  if (typeof v === 'string') return v;
  if (typeof v === 'object' && (Array.isArray(v) || Object.getPrototypeOf(v) === Object.prototype)) return JSON.stringify(v);
  return String(v);
}

for (const [k, v] of [['DateTime', DateTime], ['Duration', Duration], ['Interval', Interval]]) {
  Object.defineProperty(globalThis, k, { value: v, writable: true, configurable: true, enumerable: false });
}

return function run(src, mode) {
  const m = JSON.parse(hostCall('meta'));
  env.tz = m.timezone || 'UTC';
  env.now = m.now;
  if (m.gen !== state.gen) {
    state.gen = m.gen;
    state.cache.clear();
  }
  const v = compile(src)(buildScope(m));
  return mode === 0 ? ser(v) : render(v);
};
