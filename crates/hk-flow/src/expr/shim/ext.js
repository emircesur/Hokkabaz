// n8n "data transformation functions" on String / Array / Number / Object / Boolean / Date,
// written from the public docs. Installed non-enumerable on the realm's prototypes so
// `for...in` and JSON output are unaffected.

const def = (proto, name, fn) =>
  Object.defineProperty(proto, name, { value: fn, writable: true, configurable: true, enumerable: false });
const defAll = (proto, table) => { for (const k of Object.keys(table)) def(proto, k, table[k]); };

function deepEq(a, b) {
  if (a === b) return true;
  if (typeof a === 'number' && typeof b === 'number') return Number.isNaN(a) && Number.isNaN(b);
  if (!a || !b || typeof a !== 'object' || typeof b !== 'object') return false;
  if (Array.isArray(a) !== Array.isArray(b)) return false;
  const ka = Object.keys(a), kb = Object.keys(b);
  if (ka.length !== kb.length) return false;
  return ka.every((k) => Object.prototype.hasOwnProperty.call(b, k) && deepEq(a[k], b[k]));
}
const includesDeep = (arr, v) => arr.some((x) => deepEq(x, v));
const uniqDeep = (arr) => { const out = []; for (const v of arr) if (!includesDeep(out, v)) out.push(v); return out; };

function utf8Bytes(str) {
  const out = [];
  for (const ch of str) {
    let c = ch.codePointAt(0);
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return out;
}
function utf8Decode(bytes) {
  let s = '';
  for (let i = 0; i < bytes.length;) {
    const b = bytes[i++];
    let cp;
    if (b < 0x80) cp = b;
    else if (b < 0xe0) cp = ((b & 31) << 6) | (bytes[i++] & 63);
    else if (b < 0xf0) { cp = ((b & 15) << 12) | ((bytes[i++] & 63) << 6) | (bytes[i++] & 63); } else {
      cp = ((b & 7) << 18) | ((bytes[i++] & 63) << 12) | ((bytes[i++] & 63) << 6) | (bytes[i++] & 63);
    }
    s += String.fromCodePoint(cp);
  }
  return s;
}
const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
function b64enc(str) {
  const b = utf8Bytes(str);
  let out = '';
  for (let i = 0; i < b.length; i += 3) {
    const n = (b[i] << 16) | ((b[i + 1] || 0) << 8) | (b[i + 2] || 0);
    out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63] + (i + 1 < b.length ? B64[(n >> 6) & 63] : '=') + (i + 2 < b.length ? B64[n & 63] : '=');
  }
  return out;
}
function b64dec(str) {
  const clean = str.replace(/-/g, '+').replace(/_/g, '/').replace(/[^A-Za-z0-9+/]/g, '');
  const bytes = [];
  for (let i = 0; i < clean.length; i += 4) {
    const n = [0, 1, 2, 3].map((k) => (i + k < clean.length ? B64.indexOf(clean[i + k]) : 0));
    const v = (n[0] << 18) | (n[1] << 12) | (n[2] << 6) | n[3];
    const left = Math.min(4, clean.length - i);
    bytes.push((v >> 16) & 255);
    if (left > 2) bytes.push((v >> 8) & 255);
    if (left > 3) bytes.push(v & 255);
  }
  return utf8Decode(bytes);
}

const URL_RE = /^(?:[a-z][a-z0-9+.-]*:\/\/)([^\s/?#]+)([^\s?#]*)(\?[^\s#]*)?(#\S*)?$/i;
const EMAIL_RE = /[A-Za-z0-9._%+'-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+/;
const DOMAIN_RE = /^(?=.{1,253}$)(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+[A-Za-z]{2,}$/;
const ACCENTS = { 'à': 'a', 'á': 'a', 'â': 'a', 'ã': 'a', 'ä': 'a', 'å': 'a', 'ç': 'c', 'è': 'e', 'é': 'e', 'ê': 'e', 'ë': 'e', 'ì': 'i', 'í': 'i', 'î': 'i', 'ï': 'i', 'ñ': 'n', 'ò': 'o', 'ó': 'o', 'ô': 'o', 'õ': 'o', 'ö': 'o', 'ù': 'u', 'ú': 'u', 'û': 'u', 'ü': 'u', 'ý': 'y', 'ÿ': 'y', 'ß': 'ss', 'æ': 'ae', 'œ': 'oe', 'ø': 'o', 'ğ': 'g', 'ş': 's', 'ı': 'i', 'ł': 'l', 'ž': 'z', 'š': 's', 'č': 'c', 'ř': 'r' };

function toDT(x, fmt) {
  if (x instanceof DateTime) return x;
  if (x instanceof Date) return DateTime.fromJSDate(x);
  if (typeof x === 'string') {
    const dt = fmt ? DateTime.fromFormat(x, fmt) : DateTime.fromISO(x);
    if (dt.isValid) return dt;
    if (!fmt) {
      const d = new Date(x);
      if (!Number.isNaN(d.getTime())) return DateTime.fromJSDate(d);
    }
    throw new Error("Invalid date string '" + x + "'");
  }
  if (typeof x === 'number') return DateTime.fromMillis(x);
  throw new Error('Cannot convert to DateTime');
}

const SPECIAL_BOOL_TRUE = ['true', 't', 'yes', 'y', '1'];
const SPECIAL_BOOL_FALSE = ['false', 'f', 'no', 'n', '0', ''];

defAll(String.prototype, {
  base64Encode() { return b64enc(String(this)); },
  base64Decode() { return b64dec(String(this)); },
  extractDomain() {
    const s = String(this).trim();
    const m = URL_RE.exec(s);
    if (m) return m[1].replace(/^[^@]*@/, '').replace(/:\d+$/, '');
    const e = EMAIL_RE.exec(s);
    if (e) return e[0].split('@')[1];
    return DOMAIN_RE.test(s) ? s : undefined;
  },
  extractEmail() { const m = EMAIL_RE.exec(String(this)); return m ? m[0] : undefined; },
  extractUrl() { const m = /[a-z][a-z0-9+.-]*:\/\/[^\s<>"']+/i.exec(String(this)); return m ? m[0] : undefined; },
  extractUrlPath() {
    const m = URL_RE.exec(String(this).trim());
    return m ? m[2] || '/' : undefined;
  },
  hash(algo, enc) {
    return hostCall('hash', algo === undefined ? 'md5' : String(algo), String(this), enc);
  },
  isDomain() { return DOMAIN_RE.test(String(this)); },
  isEmail() { return /^[A-Za-z0-9._%+'-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+$/.test(String(this)); },
  isEmpty() { return String(this) === ''; },
  isNotEmpty() { return String(this) !== ''; },
  isNumeric() { const s = String(this).trim(); return s !== '' && Number.isFinite(Number(s)); },
  isUrl() { return URL_RE.test(String(this)); },
  parseJson() { try { return JSON.parse(String(this)); } catch (e) { return undefined; } },
  quote(mark) { const q = mark === undefined ? '"' : String(mark); return q + String(this).split(q).join('\\' + q) + q; },
  removeMarkdown() {
    return String(this)
      .replace(/```[\s\S]*?```/g, (m) => m.replace(/```\w*\n?/g, ''))
      .replace(/!\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
      .replace(/^#{1,6}\s+/gm, '')
      .replace(/(\*\*|__)(.*?)\1/g, '$2')
      .replace(/(\*|_)(.*?)\1/g, '$2')
      .replace(/~~(.*?)~~/g, '$1')
      .replace(/`([^`]*)`/g, '$1')
      .replace(/^>\s?/gm, '')
      .replace(/^\s*[-*+]\s+/gm, '')
      .trim();
  },
  removeTags() { return String(this).replace(/<[^>]*>/g, ''); },
  replaceSpecialChars() {
    return String(this).replace(/[^\u0000-\u007f]/g, (c) => {
      const lower = ACCENTS[c.toLowerCase()];
      if (lower === undefined) return c;
      return c === c.toLowerCase() ? lower : lower[0].toUpperCase() + lower.slice(1);
    });
  },
  toBoolean() {
    const s = String(this).toLowerCase();
    if (SPECIAL_BOOL_TRUE.includes(s)) return true;
    if (SPECIAL_BOOL_FALSE.includes(s)) return false;
    throw new Error("Unable to convert '" + String(this) + "' to boolean");
  },
  toDateTime(fmt) { return toDT(String(this), fmt); },
  toDecimalNumber() { return String(this).toFloat(); },
  toFloat() {
    const n = Number(String(this).trim());
    if (String(this).trim() === '' || Number.isNaN(n)) throw new Error("Unable to convert '" + String(this) + "' to a float");
    return n;
  },
  toInt(radix) {
    const n = parseInt(String(this).trim(), radix === undefined ? 10 : radix);
    if (Number.isNaN(n)) throw new Error("Unable to convert '" + String(this) + "' to an integer");
    return n;
  },
  toWholeNumber() { return Math.trunc(String(this).toFloat()); },
  toSentenceCase() {
    return String(this).toLowerCase().replace(/(^\s*\S|[.!?]\s+\S)/g, (c) => c.toUpperCase());
  },
  toSnakeCase() {
    return String(this)
      .replace(/([a-z0-9])([A-Z])/g, '$1 $2')
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '_')
      .replace(/^_+|_+$/g, '');
  },
  toTitleCase() {
    return String(this).replace(/\w\S*/g, (w) => w[0].toUpperCase() + w.slice(1).toLowerCase());
  },
  urlDecode(entire) { return entire ? decodeURI(String(this)) : decodeURIComponent(String(this)); },
  urlEncode(entire) { return entire ? encodeURI(String(this)) : encodeURIComponent(String(this)); },
});

const numbersOf = (arr, what) =>
  arr.map((v) => {
    const n = typeof v === 'number' ? v : Number(v);
    if (typeof v === 'boolean' || v === null || v === '' || Number.isNaN(n)) {
      throw new Error("Can't perform " + what + ": array contains the non-number value " + JSON.stringify(v));
    }
    return n;
  });

defAll(Array.prototype, {
  append(...items) { return this.concat(items); },
  average() { const n = numbersOf(this, 'average'); return n.length ? n.reduce((a, b) => a + b, 0) / n.length : NaN; },
  chunk(size) {
    if (!(size >= 1)) throw new Error('chunk size must be at least 1');
    const out = [];
    for (let i = 0; i < this.length; i += size) out.push(this.slice(i, i + size));
    return out;
  },
  compact() { return this.filter((v) => v !== null && v !== undefined && v !== '' && !(typeof v === 'number' && Number.isNaN(v))); },
  difference(other) { return uniqDeep(this.filter((v) => !includesDeep(other, v))); },
  intersection(other) { return uniqDeep(this.filter((v) => includesDeep(other, v))); },
  union(other) { return uniqDeep(this.concat(other)); },
  first() { return this[0]; },
  last() { return this[this.length - 1]; },
  isEmpty() { return this.length === 0; },
  isNotEmpty() { return this.length > 0; },
  max() { const n = numbersOf(this, 'max'); return n.length ? Math.max(...n) : NaN; },
  min() { const n = numbersOf(this, 'min'); return n.length ? Math.min(...n) : NaN; },
  sum() { return numbersOf(this, 'sum').reduce((a, b) => a + b, 0); },
  pluck(...fields) {
    return this.map((o) => {
      if (fields.length === 1) return o == null ? undefined : o[fields[0]];
      const r = {};
      for (const f of fields) if (o != null && f in o) r[f] = o[f];
      return r;
    });
  },
  randomItem() { return this[Math.floor(Math.random() * this.length)]; },
  removeDuplicates(...keys) { return this.unique(...keys); },
  unique(...keys) {
    if (!keys.length) return uniqDeep(this);
    const out = [], seen = [];
    for (const o of this) {
      const sig = keys.map((k) => (o == null ? undefined : o[k]));
      if (!includesDeep(seen, sig)) { seen.push(sig); out.push(o); }
    }
    return out;
  },
  renameKeys(...pairs) {
    return this.map((o) => {
      const r = {};
      for (const k of Object.keys(o)) {
        let name = k;
        for (let i = 0; i + 1 < pairs.length; i += 2) if (pairs[i] === k) name = pairs[i + 1];
        r[name] = o[k];
      }
      return r;
    });
  },
  smartJoin(keyField, nameField) {
    const r = {};
    for (const o of this) {
      const k = o[keyField];
      if (typeof k === 'string' && k !== '' && o[nameField] !== undefined) r[k] = o[nameField];
    }
    return r;
  },
  toJsonString() { return JSON.stringify(this); },
});

defAll(Number.prototype, {
  ceil() { return Math.ceil(this); },
  floor() { return Math.floor(this); },
  abs() { return Math.abs(this); },
  round(decimals) {
    const f = Math.pow(10, decimals === undefined ? 0 : decimals);
    const v = Number(this) * f;
    // symmetrical rounding of exact .5 like toFixed users expect for money
    return Math.round(v + (v < 0 ? -Number.EPSILON * Math.abs(v) : Number.EPSILON * Math.abs(v))) / f;
  },
  isEven() { return Number.isInteger(Number(this)) && Number(this) % 2 === 0; },
  isOdd() { return Number.isInteger(Number(this)) && Math.abs(Number(this) % 2) === 1; },
  isInteger() { return Number.isInteger(Number(this)); },
  toBoolean() { return Number(this) !== 0; },
  toDateTime(fmt) {
    const n = Number(this);
    if (fmt === undefined || fmt === 'ms') return DateTime.fromMillis(n);
    if (fmt === 's') return DateTime.fromSeconds(n);
    if (fmt === 'excel') return DateTime.fromMillis(Math.round((n - 25569) * 86400000), { zone: 'UTC' });
    throw new Error('Invalid DateTime format ' + fmt);
  },
  format(locale, opts) {
    opts = opts || {};
    const n = Number(this);
    const minF = opts.minimumFractionDigits !== undefined ? opts.minimumFractionDigits : opts.style === 'currency' ? 2 : 0;
    const maxF = opts.maximumFractionDigits !== undefined ? Math.max(opts.maximumFractionDigits, minF) : Math.max(minF, opts.style === 'currency' ? 2 : 3);
    const sep = locale && /^(de|es|it|pt|nl|tr|id)/i.test(locale) ? { g: '.', d: ',' } : locale && /^(fr|ru|pl|cs|sv|nb)/i.test(locale) ? { g: ' ', d: ',' } : { g: ',', d: '.' };
    let s = Math.abs(n).toFixed(maxF);
    let [ip, fp = ''] = s.split('.');
    while (fp.length > minF && fp.endsWith('0')) fp = fp.slice(0, -1);
    if (opts.useGrouping !== false) ip = ip.replace(/\B(?=(\d{3})+(?!\d))/g, sep.g);
    s = ip + (fp ? sep.d + fp : '');
    if (opts.style === 'percent') return (n < 0 ? '-' : '') + (Math.abs(n) * 100).format(locale, { ...opts, style: undefined }) + '%';
    if (opts.style === 'currency') {
      const sym = { USD: '$', EUR: '€', GBP: '£', JPY: '¥' }[opts.currency] || opts.currency + ' ';
      return (n < 0 ? '-' : '') + sym + s;
    }
    return (n < 0 ? '-' : '') + s;
  },
});

defAll(Boolean.prototype, { toInt() { return this.valueOf() ? 1 : 0; } });

const isPlainObj = (o) => o !== null && typeof o === 'object' && !Array.isArray(o);
const objKeys = (o) => Object.keys(o);
defAll(Object.prototype, {
  isEmpty() { return objKeys(this).length === 0; },
  isNotEmpty() { return objKeys(this).length > 0; },
  hasField(name) { return Object.prototype.hasOwnProperty.call(this, name); },
  removeField(key) { const r = Object.assign({}, this); delete r[key]; return r; },
  removeFieldsContaining(value) {
    const r = {};
    for (const k of objKeys(this)) if (!(typeof this[k] === 'string' && this[k].includes(value))) r[k] = this[k];
    return r;
  },
  keepFieldsContaining(value) {
    const r = {};
    for (const k of objKeys(this)) if (typeof this[k] === 'string' && this[k].includes(value)) r[k] = this[k];
    return r;
  },
  compact() {
    const r = {};
    for (const k of objKeys(this)) {
      const v = this[k];
      if (v === null || v === undefined || v === 'nil' || v === '') continue;
      r[k] = isPlainObj(v) ? v.compact() : v;
    }
    return r;
  },
  urlEncode() { return objKeys(this).map((k) => encodeURIComponent(k) + '=' + encodeURIComponent(this[k])).join('&'); },
  toJsonString() { return JSON.stringify(this); },
  keys() { return objKeys(this); },
  values() { return objKeys(this).map((k) => this[k]); },
});
// Arrays inherit Object.prototype; native keys()/values() iterators must win again.
def(Array.prototype, 'keys', Array.prototype.keys);
def(Array.prototype, 'values', Array.prototype.values);
for (const [name, native] of [['keys', Array.prototype.keys], ['values', Array.prototype.values]]) {
  def(Array.prototype, name, native);
}

// Date + DateTime extensions
const dateExt = (wrap) => ({
  beginningOf(unit) { return wrap(this, toDT(this).startOf(unit)); },
  endOfMonth() { return wrap(this, toDT(this).endOf('month')); },
  extract(unit) {
    const dt = toDT(this);
    const u = unit === undefined ? 'day' : String(unit);
    const map = { year: dt.year, month: dt.month, week: dt.weekNumber, day: dt.day, hour: dt.hour, minute: dt.minute, second: dt.second, dayOfWeek: dt.weekday, yearDayNumber: dt.ordinal };
    if (!(u in map)) throw new Error('Invalid unit ' + u);
    return map[u];
  },
  format(fmt) { return toDT(this).toFormat(fmt === undefined ? 'MM/dd/yyyy' : fmt); },
  isBetween(a, b) {
    const t = toDT(this).ts, x = toDT(a).ts, y = toDT(b).ts;
    return t > Math.min(x, y) && t < Math.max(x, y);
  },
  isDst() { return toDT(this).isInDST; },
  isInLast(n, unit) {
    const dt = toDT(this);
    const now = DateTime.now();
    const from = now.minus({ [unit === undefined ? 'minutes' : unit]: n === undefined ? 0 : n });
    return dt.ts >= from.ts && dt.ts <= now.ts;
  },
  isWeekend() { return toDT(this).weekday >= 6; },
  plus(n, unit) { return wrap(this, toDT(this).plus({ [unit === undefined ? 'milliseconds' : unit]: n })); },
  minus(n, unit) { return wrap(this, toDT(this).minus({ [unit === undefined ? 'milliseconds' : unit]: n })); },
  diffTo(other, unit) {
    const u = unit === undefined ? 'days' : unit;
    return toDT(this).diff(toDT(other), u).get(u);
  },
  toDateTime() { return toDT(this); },
  toTimeFromNow() { return toDT(this).toRelative(); },
});
defAll(Date.prototype, dateExt((orig, dt) => dt.toJSDate()));
defAll(DateTime.prototype, dateExt((orig, dt) => dt));
// Luxon already has native plus/minus/toFormat semantics: keep them (Date ext above is separate).
def(DateTime.prototype, 'plus', DateTime.prototype.plus);
def(DateTime.prototype, 'minus', DateTime.prototype.minus);
