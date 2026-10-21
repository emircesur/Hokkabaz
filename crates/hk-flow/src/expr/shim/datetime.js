// Luxon-like DateTime / Duration / Interval subset (en-US only), clean-room from the
// public Luxon API docs as exposed by n8n expressions. Shares one closure with
// ext.js and core.js (see engine.rs); `H` is the host dispatcher.

// Optional host args must be absent, not `undefined`, to take the `Opt<String>` path.
const hostCall = (op, ...args) => {
  while (args.length && args[args.length - 1] === undefined) args.pop();
  const r = H(op, ...args.map(String));
  if (r.charCodeAt(0) === 1) throw new Error(r.slice(1));
  return r;
};

// Set by core.js at the start of every evaluation.
const env = { tz: 'UTC', now: 0 };

const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
const WEEKDAYS = ['Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday', 'Sunday'];
const UNIT_ORDER = ['years', 'quarters', 'months', 'weeks', 'days', 'hours', 'minutes', 'seconds', 'milliseconds'];
const UNIT_ALIASES = {
  year: 'years', years: 'years', quarter: 'quarters', quarters: 'quarters', month: 'months', months: 'months',
  week: 'weeks', weeks: 'weeks', day: 'days', days: 'days', hour: 'hours', hours: 'hours',
  minute: 'minutes', minutes: 'minutes', second: 'seconds', seconds: 'seconds',
  millisecond: 'milliseconds', milliseconds: 'milliseconds',
};
function normUnit(u) {
  const k = UNIT_ALIASES[String(u).toLowerCase()];
  if (!k) throw new Error('Invalid unit ' + u);
  return k;
}
const pad = (n, w = 2) => {
  const s = String(Math.abs(Math.trunc(n))).padStart(w, '0');
  return n < 0 ? '-' + s : s;
};
const isNum = (x) => typeof x === 'number' && !Number.isNaN(x);
const isLeap = (y) => (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0;
const daysInMonth = (y, m) => [31, isLeap(y) ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][m - 1];

// ---- zones
class Zone {
  constructor(kind, name, mins) {
    this.kind = kind; // 'fixed' | 'iana' | 'invalid'
    this.name = name;
    this.mins = mins || 0;
  }
  get valid() { return this.kind !== 'invalid'; }
  offset(ts) {
    return this.kind === 'iana' ? Number(hostCall('tzOffset', this.name, String(Math.trunc(ts)))) : this.mins;
  }
}
const zoneCache = new Map();
function fixedName(mins) {
  if (mins === 0) return 'UTC';
  const s = mins < 0 ? '-' : '+';
  const a = Math.abs(mins);
  const h = Math.floor(a / 60), m = a % 60;
  return 'UTC' + s + h + (m ? ':' + pad(m) : '');
}
function makeZone(spec) {
  if (spec instanceof Zone) return spec;
  if (spec && typeof spec === 'object' && typeof spec.name === 'string') spec = spec.name;
  if (spec === undefined || spec === null) spec = 'local';
  const s = String(spec);
  const lower = s.toLowerCase();
  if (lower === 'local' || lower === 'system') return makeZone(env.tz);
  let z = zoneCache.get(s);
  if (z) return z;
  let m;
  if (lower === 'utc' || lower === 'gmt' || lower === 'z') z = new Zone('fixed', 'UTC', 0);
  else if ((m = /^(?:utc|gmt)?\s*([+-])(\d{1,2})(?::?(\d{2}))?$/i.exec(s))) {
    const mins = (m[1] === '-' ? -1 : 1) * (Number(m[2]) * 60 + Number(m[3] || 0));
    z = new Zone('fixed', fixedName(mins), mins);
  } else if (hostCall('tzValid', s) === '1') z = new Zone('iana', s, 0);
  else z = new Zone('invalid', s, 0);
  zoneCache.set(s, z);
  return z;
}

// local wall-clock components -> epoch ms in `zone` (Luxon's two-pass offset fix)
function localMs(c) {
  const d = new Date(0);
  d.setUTCFullYear(c.year, (c.month || 1) - 1, c.day || 1);
  d.setUTCHours(c.hour || 0, c.minute || 0, c.second || 0, c.millisecond || 0);
  return d.getTime();
}
function wallToTs(c, zone) {
  const local = localMs(c);
  const o = zone.offset(local);
  let guess = local - o * 60000;
  const o2 = zone.offset(guess);
  if (o === o2) return guess;
  guess -= (o2 - o) * 60000;
  const o3 = zone.offset(guess);
  if (o2 === o3) return guess;
  return local - Math.min(o2, o3) * 60000;
}

// ---- Duration
const CASUAL = {
  years: { quarters: 4, months: 12, weeks: 52, days: 365, hours: 8760, minutes: 525600, seconds: 31536000, milliseconds: 31536000000 },
  quarters: { months: 3, weeks: 13, days: 91, hours: 2184, minutes: 131040, seconds: 7862400, milliseconds: 7862400000 },
  months: { weeks: 4, days: 30, hours: 720, minutes: 43200, seconds: 2592000, milliseconds: 2592000000 },
  weeks: { days: 7, hours: 168, minutes: 10080, seconds: 604800, milliseconds: 604800000 },
  days: { hours: 24, minutes: 1440, seconds: 86400, milliseconds: 86400000 },
  hours: { minutes: 60, seconds: 3600, milliseconds: 3600000 },
  minutes: { seconds: 60, milliseconds: 60000 },
  seconds: { milliseconds: 1000 },
};
// how many `to` units in one `from` unit
function ratio(from, to) {
  if (from === to) return 1;
  const fi = UNIT_ORDER.indexOf(from), ti = UNIT_ORDER.indexOf(to);
  return fi < ti ? CASUAL[from][to] : 1 / CASUAL[to][from];
}

class Duration {
  constructor(values) {
    this.values = {};
    for (const u of UNIT_ORDER) if (values && isNum(values[u])) this.values[u] = values[u];
    Object.defineProperty(this, 'isLuxonDuration', { value: true });
  }
  static fromObject(o) {
    const v = {};
    for (const k of Object.keys(o || {})) v[normUnit(k)] = o[k];
    return new Duration(v);
  }
  static fromMillis(ms) { return new Duration({ milliseconds: ms }); }
  static fromDurationLike(x) {
    if (x instanceof Duration) return x;
    if (isNum(x)) return Duration.fromMillis(x);
    if (x && typeof x === 'object') return Duration.fromObject(x);
    throw new Error('Unknown duration argument ' + x);
  }
  static fromISO(s) {
    const m = /^(-)?P(?:(-?\d+(?:\.\d+)?)Y)?(?:(-?\d+(?:\.\d+)?)M)?(?:(-?\d+(?:\.\d+)?)W)?(?:(-?\d+(?:\.\d+)?)D)?(?:T(?:(-?\d+(?:\.\d+)?)H)?(?:(-?\d+(?:\.\d+)?)M)?(?:(-?\d+(?:\.\d+)?)S)?)?$/.exec(String(s));
    if (!m) throw new Error('Invalid ISO duration ' + s);
    const sign = m[1] ? -1 : 1;
    const keys = ['years', 'months', 'weeks', 'days', 'hours', 'minutes', 'seconds'];
    const v = {};
    keys.forEach((k, i) => { if (m[i + 2] !== undefined) v[k] = sign * Number(m[i + 2]); });
    return new Duration(v);
  }
  static isDuration(o) { return o instanceof Duration; }
  get isValid() { return true; }
  get years() { return this.values.years || 0; }
  get quarters() { return this.values.quarters || 0; }
  get months() { return this.values.months || 0; }
  get weeks() { return this.values.weeks || 0; }
  get days() { return this.values.days || 0; }
  get hours() { return this.values.hours || 0; }
  get minutes() { return this.values.minutes || 0; }
  get seconds() { return this.values.seconds || 0; }
  get milliseconds() { return this.values.milliseconds || 0; }
  get(unit) { return this.values[normUnit(unit)] || 0; }
  set(o) { return new Duration(Object.assign({}, this.values, Duration.fromObject(o).values)); }
  toObject() { return Object.assign({}, this.values); }
  shiftTo(...units) {
    units = (units.length === 1 && Array.isArray(units[0]) ? units[0] : units).map(normUnit);
    if (!units.length) return this;
    const ordered = UNIT_ORDER.filter((u) => units.includes(u));
    const smallest = ordered[ordered.length - 1];
    let total = 0;
    for (const u of UNIT_ORDER) if (this.values[u]) total += this.values[u] * ratio(u, smallest);
    const out = {};
    ordered.forEach((u, i) => {
      if (i === ordered.length - 1) { out[u] = total; return; }
      const r = ratio(u, smallest);
      const n = Math.trunc(total / r);
      out[u] = n;
      total -= n * r;
    });
    return new Duration(out);
  }
  shiftToAll() { return this.shiftTo(...UNIT_ORDER); }
  normalize() {
    const present = UNIT_ORDER.filter((u) => this.values[u] !== undefined);
    return present.length ? this.shiftTo(...present) : this;
  }
  rescale() { return this.normalize(); }
  as(unit) { return this.shiftTo(unit).get(unit); }
  toMillis() { return this.as('milliseconds'); }
  toISO() {
    const v = this.values, p = (n, c) => (n ? n + c : '');
    const date = p(v.years, 'Y') + p((v.months || 0) + (v.quarters || 0) * 3, 'M') + p(v.weeks, 'W') + p(v.days, 'D');
    const secs = (v.seconds || 0) + (v.milliseconds || 0) / 1000;
    const time = p(v.hours, 'H') + p(v.minutes, 'M') + p(secs, 'S');
    return 'P' + date + (time ? 'T' + time : '') || 'PT0S';
  }
  toJSON() { return this.toISO(); }
  toString() { return this.toISO(); }
  valueOf() { return this.toMillis(); }
  plus(o) {
    const d = Duration.fromDurationLike(o), r = Object.assign({}, this.values);
    for (const u of Object.keys(d.values)) r[u] = (r[u] || 0) + d.values[u];
    return new Duration(r);
  }
  minus(o) { return this.plus(Duration.fromDurationLike(o).negate()); }
  negate() {
    const r = {};
    for (const u of Object.keys(this.values)) r[u] = -this.values[u];
    return new Duration(r);
  }
  mapUnits(fn) {
    const r = {};
    for (const u of Object.keys(this.values)) r[u] = fn(this.values[u], u);
    return new Duration(r);
  }
  equals(o) { return o instanceof Duration && UNIT_ORDER.every((u) => (this.values[u] || 0) === (o.values[u] || 0)); }
  toFormat(fmt) {
    const map = { y: 'years', M: 'months', w: 'weeks', d: 'days', h: 'hours', m: 'minutes', s: 'seconds', S: 'milliseconds' };
    return String(fmt).replace(/y+|M+|w+|d+|h+|m+|s+|S+|'[^']*'/g, (t) => {
      if (t[0] === "'") return t.slice(1, -1);
      return pad(this.get(map[t[0]]), t.length);
    });
  }
}

// ---- DateTime
class DateTime {
  constructor(ts, zone, invalid) {
    this.invalid = invalid || null;
    this.ts = ts;
    this.zone = zone;
    this.o = this.invalid ? NaN : zone.offset(ts);
    this._c = null;
    Object.defineProperty(this, 'isLuxonDateTime', { value: true });
  }
  static invalid(reason) { return new DateTime(NaN, makeZone('UTC'), reason || 'invalid input'); }
  static isDateTime(o) { return o instanceof DateTime; }
  static now() { return new DateTime(env.now, makeZone('local')); }
  static local(...a) {
    if (!a.length) return DateTime.now();
    const [year, month = 1, day = 1, hour = 0, minute = 0, second = 0, millisecond = 0] = a;
    return DateTime.fromComponents({ year, month, day, hour, minute, second, millisecond }, makeZone('local'));
  }
  static utc(...a) {
    if (!a.length) return new DateTime(env.now, makeZone('UTC'));
    const [year, month = 1, day = 1, hour = 0, minute = 0, second = 0, millisecond = 0] = a;
    return DateTime.fromComponents({ year, month, day, hour, minute, second, millisecond }, makeZone('UTC'));
  }
  static fromComponents(c, zone) {
    const z = makeZone(zone);
    if (!z.valid) return DateTime.invalid('unsupported zone');
    for (const k of ['year', 'month', 'day', 'hour', 'minute', 'second', 'millisecond']) {
      if (c[k] !== undefined && !isNum(c[k])) return DateTime.invalid('invalid ' + k);
    }
    // Luxon rejects out-of-range fields instead of rolling them over
    if (c.month < 1 || c.month > 12 || c.day < 1 || c.day > daysInMonth(c.year, c.month) ||
        c.hour > 24 || c.hour < 0 || c.minute > 59 || c.minute < 0 || c.second > 59 || c.second < 0) {
      return DateTime.invalid('unit out of range');
    }
    return new DateTime(wallToTs(c, z), z);
  }
  static fromMillis(ms, o) {
    return isNum(ms) ? new DateTime(ms, makeZone(o && o.zone)) : DateTime.invalid('millis is not a number');
  }
  static fromSeconds(s, o) {
    return isNum(s) ? new DateTime(s * 1000, makeZone(o && o.zone)) : DateTime.invalid('seconds is not a number');
  }
  static fromJSDate(d, o) {
    const t = d.getTime();
    return isNum(t) ? new DateTime(t, makeZone(o && o.zone)) : DateTime.invalid('invalid date');
  }
  static fromObject(obj, o) {
    obj = obj || {};
    const zone = makeZone(o && o.zone);
    if (!zone.valid) return DateTime.invalid('unsupported zone');
    const keys = ['year', 'month', 'day', 'hour', 'minute', 'second', 'millisecond'];
    const given = {};
    for (const k of Object.keys(obj)) {
      const u = normUnit(k);
      given[u.replace(/s$/, '')] = obj[k];
    }
    const nowC = new DateTime(env.now, zone).c;
    const idx = keys.filter((k) => given[k] !== undefined).map((k) => keys.indexOf(k));
    const lo = idx.length ? Math.min(...idx) : keys.length;
    const defaults = { month: 1, day: 1, hour: 0, minute: 0, second: 0, millisecond: 0 };
    const c = {};
    keys.forEach((k, i) => { c[k] = given[k] !== undefined ? given[k] : i < lo ? nowC[k] : defaults[k]; });
    return DateTime.fromComponents(c, zone);
  }
  static fromISO(text, o) {
    const m = /^(-?\d{4})(?:-?(\d{2})(?:-?(\d{2}))?)?(?:[T ](\d{2})(?::?(\d{2})(?::?(\d{2})(?:[.,](\d+))?)?)?)?\s*(Z|[+-]\d{2}(?::?\d{2})?)?$/i.exec(String(text).trim());
    if (!m) return DateTime.invalid('unparsable');
    const c = {
      year: +m[1], month: m[2] ? +m[2] : 1, day: m[3] ? +m[3] : 1,
      hour: m[4] ? +m[4] : 0, minute: m[5] ? +m[5] : 0, second: m[6] ? +m[6] : 0,
      millisecond: m[7] ? Math.floor(Number('0.' + m[7]) * 1000 + 1e-9) : 0,
    };
    return DateTime.finishParse(c, m[8], o);
  }
  static fromSQL(text, o) { return DateTime.fromISO(text, o); }
  static finishParse(c, offStr, o) {
    const zone = makeZone(o && o.zone);
    if (offStr === undefined || offStr === null) return DateTime.fromComponents(c, zone);
    let mins = 0;
    if (!/^z$/i.test(offStr)) {
      const mm = /^([+-])(\d{2}):?(\d{2})?$/.exec(offStr);
      if (!mm) return DateTime.invalid('bad offset');
      mins = (mm[1] === '-' ? -1 : 1) * (+mm[2] * 60 + +(mm[3] || 0));
    }
    const probe = DateTime.fromComponents(c, makeZone('UTC'));
    if (!probe.isValid) return probe;
    const ts = probe.ts - mins * 60000;
    return new DateTime(ts, o && o.setZone ? makeZone(mins === 0 ? 'UTC' : 'UTC' + (mins < 0 ? '-' : '+') + pad(Math.abs(mins) / 60 | 0) + ':' + pad(Math.abs(mins) % 60)) : zone);
  }
  static fromFormat(text, fmt, o) { return parseWithFormat(String(text), String(fmt), o || {}); }
  static max(...a) { return a.reduce((p, c) => (p === null || c.ts > p.ts ? c : p), null); }
  static min(...a) { return a.reduce((p, c) => (p === null || c.ts < p.ts ? c : p), null); }

  get isValid() { return !this.invalid; }
  get invalidReason() { return this.invalid; }
  get c() {
    if (!this._c) {
      const d = new Date(this.ts + this.o * 60000);
      this._c = {
        year: d.getUTCFullYear(), month: d.getUTCMonth() + 1, day: d.getUTCDate(),
        hour: d.getUTCHours(), minute: d.getUTCMinutes(), second: d.getUTCSeconds(), millisecond: d.getUTCMilliseconds(),
      };
    }
    return this._c;
  }
  get year() { return this.isValid ? this.c.year : NaN; }
  get month() { return this.isValid ? this.c.month : NaN; }
  get day() { return this.isValid ? this.c.day : NaN; }
  get hour() { return this.isValid ? this.c.hour : NaN; }
  get minute() { return this.isValid ? this.c.minute : NaN; }
  get second() { return this.isValid ? this.c.second : NaN; }
  get millisecond() { return this.isValid ? this.c.millisecond : NaN; }
  get weekday() { return this.isValid ? new Date(localMs(this.c)).getUTCDay() || 7 : NaN; }
  get quarter() { return this.isValid ? Math.ceil(this.c.month / 3) : NaN; }
  get ordinal() { return this.isValid ? Math.round((localMs({ ...this.c, hour: 0, minute: 0, second: 0, millisecond: 0 }) - localMs({ year: this.c.year, month: 1, day: 1 })) / 86400000) + 1 : NaN; }
  get weekYear() { return this.isValid ? isoWeek(this.c).weekYear : NaN; }
  get weekNumber() { return this.isValid ? isoWeek(this.c).week : NaN; }
  get weeksInWeekYear() { return isoWeeksIn(this.weekYear); }
  get daysInMonth() { return daysInMonth(this.c.year, this.c.month); }
  get daysInYear() { return isLeap(this.c.year) ? 366 : 365; }
  get isInLeapYear() { return isLeap(this.c.year); }
  get monthShort() { return MONTHS[this.c.month - 1].slice(0, 3); }
  get monthLong() { return MONTHS[this.c.month - 1]; }
  get weekdayShort() { return WEEKDAYS[this.weekday - 1].slice(0, 3); }
  get weekdayLong() { return WEEKDAYS[this.weekday - 1]; }
  get zoneName() { return this.zone.name; }
  get offset() { return this.o; }
  get isOffsetFixed() { return this.zone.kind === 'fixed'; }
  get isInDST() {
    if (this.zone.kind !== 'iana') return false;
    const y = this.c.year;
    const jan = this.zone.offset(Date.UTC(y, 0, 1)), jul = this.zone.offset(Date.UTC(y, 6, 1));
    return jan !== jul && this.o === Math.max(jan, jul);
  }
  get locale() { return 'en-US'; }
  get offsetNameShort() { return this.zone.kind === 'fixed' ? this.zone.name : 'UTC' + fmtOffset(this.o, 'short'); }
  get offsetNameLong() { return this.offsetNameShort; }

  toMillis() { return this.ts; }
  toSeconds() { return this.ts / 1000; }
  toUnixInteger() { return Math.floor(this.ts / 1000); }
  valueOf() { return this.ts; }
  toJSDate() { return new Date(this.ts); }
  toObject() { return this.isValid ? Object.assign({}, this.c) : {}; }
  toJSON() { return this.toISO(); }
  toString() { return this.isValid ? this.toISO() : 'Invalid DateTime'; }
  toISO(opts) {
    if (!this.isValid) return null;
    opts = opts || {};
    return this.toISODate() + 'T' + this.toISOTime(opts);
  }
  toISODate() {
    if (!this.isValid) return null;
    const y = this.c.year;
    return (y > 9999 ? '+' + pad(y, 6) : y < 0 ? '-' + pad(-y, 6) : pad(y, 4)) + '-' + pad(this.c.month) + '-' + pad(this.c.day);
  }
  toISOTime(opts) {
    if (!this.isValid) return null;
    opts = opts || {};
    const c = this.c;
    let s = pad(c.hour) + ':' + pad(c.minute);
    if (!(opts.suppressSeconds && c.second === 0 && c.millisecond === 0)) {
      s += ':' + pad(c.second);
      if (!(opts.suppressMilliseconds && c.millisecond === 0)) s += '.' + pad(c.millisecond, 3);
    }
    if (opts.includeOffset !== false) s += this.zone.kind === 'fixed' && this.o === 0 ? 'Z' : fmtOffset(this.o, 'narrow-colon');
    return (opts.includePrefix ? 'T' : '') + s;
  }
  toSQLDate() { return this.toISODate(); }
  toSQLTime(opts) { return this.isValid ? this.toISOTime({ includeOffset: false, ...(opts || {}) }) : null; }
  toSQL(opts) { return this.isValid ? this.toSQLDate() + ' ' + this.toSQLTime(opts) : null; }
  toFormat(fmt) { return this.isValid ? formatDT(this, String(fmt)) : 'Invalid DateTime'; }
  toLocaleString(opts) { return this.isValid ? localeString(this, opts || LOCALE_PRESETS.DATE_SHORT) : 'Invalid DateTime'; }
  toRelative(o) {
    const base = (o && o.base) || DateTime.now();
    const diffMs = this.ts - base.ts;
    const abs = Math.abs(diffMs);
    const table = [['year', 31536000000], ['month', 2592000000], ['day', 86400000], ['hour', 3600000], ['minute', 60000], ['second', 1000]];
    for (const [u, ms] of table) {
      const n = Math.trunc(abs / ms);
      if (n >= 1) {
        const label = n + ' ' + u + (n === 1 ? '' : 's');
        return diffMs < 0 ? label + ' ago' : 'in ' + label;
      }
    }
    return 'in 0 seconds';
  }
  toUTC(offset, o) { return this.setZone(offset ? 'UTC' + (offset < 0 ? '-' : '+') + Math.abs(offset) : 'UTC', o); }
  toLocal() { return this.setZone('local'); }
  setZone(zone, o) {
    const z = makeZone(zone);
    if (!z.valid) return DateTime.invalid('unsupported zone');
    if (o && o.keepLocalTime) return DateTime.fromComponents(this.c, z);
    return new DateTime(this.ts, z);
  }
  setLocale() { return this; }
  reconfigure() { return this; }
  get(unit) { return this[normUnit(unit).replace(/s$/, '')]; }
  set(values) {
    const c = Object.assign({}, this.c);
    let dayGiven = false;
    for (const k of Object.keys(values)) {
      if (k === 'weekday') continue;
      const u = normUnit(k).replace(/s$/, '');
      if (u === 'day') dayGiven = true;
      c[u] = values[k];
    }
    if (values.weekday !== undefined) {
      const shift = values.weekday - this.weekday;
      c.day = this.c.day + shift;
      dayGiven = true;
    }
    if (!dayGiven) c.day = Math.min(c.day, daysInMonth(c.year, c.month));
    return new DateTime(wallToTs(c, this.zone), this.zone);
  }
  // n8n docs also show `plus(1, 'day')`; plain Luxon would read the number as milliseconds.
  plus(dur, unit) { return this.shift(Duration.fromDurationLike(typeof unit === 'string' ? { [unit]: dur } : dur), 1); }
  minus(dur, unit) { return this.shift(Duration.fromDurationLike(typeof unit === 'string' ? { [unit]: dur } : dur), -1); }
  shift(dur, sign) {
    if (!this.isValid) return this;
    const v = dur.values;
    const g = (u) => (v[u] || 0) * sign;
    const c = Object.assign({}, this.c);
    const months = g('years') * 12 + g('quarters') * 3 + g('months');
    const total = c.year * 12 + (c.month - 1) + months;
    c.year = Math.floor(total / 12);
    c.month = ((total % 12) + 12) % 12 + 1;
    c.day = Math.min(c.day, daysInMonth(c.year, c.month)) + g('days') + g('weeks') * 7;
    const ts = wallToTs(c, this.zone);
    const ms = g('hours') * 3600000 + g('minutes') * 60000 + g('seconds') * 1000 + g('milliseconds');
    return new DateTime(ts + ms, this.zone);
  }
  startOf(unit) {
    if (!this.isValid) return this;
    const u = normUnit(unit).replace(/s$/, '');
    const c = Object.assign({}, this.c);
    const reset = (from) => {
      const order = ['month', 'day', 'hour', 'minute', 'second', 'millisecond'];
      const dflt = { month: 1, day: 1, hour: 0, minute: 0, second: 0, millisecond: 0 };
      for (const k of order.slice(order.indexOf(from))) c[k] = dflt[k];
    };
    switch (u) {
      case 'year': reset('month'); break;
      case 'quarter': c.month = Math.floor((c.month - 1) / 3) * 3 + 1; reset('day'); break;
      case 'month': reset('day'); break;
      case 'week': reset('hour'); c.day -= this.weekday - 1; break;
      case 'day': reset('hour'); break;
      case 'hour': reset('minute'); break;
      case 'minute': reset('second'); break;
      case 'second': c.millisecond = 0; break;
      case 'millisecond': break;
      default: throw new Error('Invalid unit ' + unit);
    }
    return new DateTime(wallToTs(c, this.zone), this.zone);
  }
  endOf(unit) {
    if (!this.isValid) return this;
    const u = normUnit(unit);
    return this.startOf(u).plus({ [u]: 1 }).minus({ milliseconds: 1 });
  }
  hasSame(other, unit) {
    const u = normUnit(unit);
    const o = other.setZone(this.zone, { keepLocalTime: false });
    return this.startOf(u).ts === o.startOf(u).ts;
  }
  equals(o) { return o instanceof DateTime && o.ts === this.ts && o.zone.name === this.zone.name; }
  diff(other, units, opts) {
    const list = (units === undefined ? ['milliseconds'] : Array.isArray(units) ? units : [units]).map(normUnit);
    const ordered = UNIT_ORDER.filter((u) => list.includes(u));
    const neg = other.ts > this.ts;
    const [a, b] = neg ? [this, other] : [other, this]; // a <= b
    const out = {};
    let cursor = a;
    ordered.forEach((u, i) => {
      const last = i === ordered.length - 1;
      if (['years', 'quarters', 'months', 'weeks', 'days'].includes(u)) {
        // lower bound from the longest possible unit, then climb to the exact count
        const longest = { years: 366, quarters: 92, months: 31, weeks: 7, days: 1 }[u];
        let n = Math.max(0, Math.floor((b.ts - cursor.ts) / (longest * 86400000)));
        while (cursor.plus({ [u]: n + 1 }).ts <= b.ts) n++;
        const next = cursor.plus({ [u]: n });
        if (last) {
          const span = cursor.plus({ [u]: n + 1 }).ts - next.ts;
          out[u] = n + (b.ts - next.ts) / span;
        } else out[u] = n;
        cursor = next;
      } else {
        let rem = b.ts - cursor.ts;
        const unitMs = { hours: 3600000, minutes: 60000, seconds: 1000, milliseconds: 1 }[u];
        if (last) { out[u] = rem / unitMs; rem = 0; } else { out[u] = Math.floor(rem / unitMs); rem -= out[u] * unitMs; }
        cursor = new DateTime(b.ts - rem, b.zone);
      }
    });
    let d = new Duration(out);
    if (neg) d = d.negate();
    return d;
  }
  diffNow(units, opts) { return this.diff(DateTime.now(), units, opts); }
  until(other) { return Interval.fromDateTimes(this, other); }
}

function fmtOffset(mins, style) {
  const sign = mins < 0 ? '-' : '+';
  const a = Math.abs(mins);
  const h = Math.trunc(a / 60), m = Math.trunc(a % 60);
  if (style === 'short') return sign + h + (m ? ':' + pad(m) : '');
  if (style === 'techie') return sign + pad(h) + pad(m);
  return sign + pad(h) + ':' + pad(m);
}

function isoWeek(c) {
  const d = new Date(Date.UTC(c.year, c.month - 1, c.day));
  const day = d.getUTCDay() || 7;
  d.setUTCDate(d.getUTCDate() + 4 - day);
  const y = d.getUTCFullYear();
  const week = Math.ceil(((d - Date.UTC(y, 0, 1)) / 86400000 + 1) / 7);
  return { weekYear: y, week };
}
function isoWeeksIn(y) {
  return isoWeek({ year: y, month: 12, day: 28 }).week;
}

const LOCALE_PRESETS = {
  DATE_SHORT: { year: 'numeric', month: 'numeric', day: 'numeric' },
  DATE_MED: { year: 'numeric', month: 'short', day: 'numeric' },
  DATE_MED_WITH_WEEKDAY: { year: 'numeric', month: 'short', day: 'numeric', weekday: 'short' },
  DATE_FULL: { year: 'numeric', month: 'long', day: 'numeric' },
  DATE_HUGE: { year: 'numeric', month: 'long', day: 'numeric', weekday: 'long' },
  TIME_SIMPLE: { hour: 'numeric', minute: '2-digit' },
  TIME_WITH_SECONDS: { hour: 'numeric', minute: '2-digit', second: '2-digit' },
  TIME_24_SIMPLE: { hour: 'numeric', minute: '2-digit', hourCycle: 'h23' },
  TIME_24_WITH_SECONDS: { hour: 'numeric', minute: '2-digit', second: '2-digit', hourCycle: 'h23' },
  DATETIME_SHORT: { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: '2-digit' },
  DATETIME_SHORT_WITH_SECONDS: { year: 'numeric', month: 'numeric', day: 'numeric', hour: 'numeric', minute: '2-digit', second: '2-digit' },
  DATETIME_MED: { year: 'numeric', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' },
  DATETIME_MED_WITH_SECONDS: { year: 'numeric', month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit', second: '2-digit' },
  DATETIME_FULL: { year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' },
  DATETIME_HUGE: { year: 'numeric', month: 'long', day: 'numeric', weekday: 'long', hour: 'numeric', minute: '2-digit' },
};
for (const k of Object.keys(LOCALE_PRESETS)) DateTime[k] = LOCALE_PRESETS[k];

// en-US Intl.DateTimeFormat subset
function localeString(dt, o) {
  const c = dt.c;
  const hasDate = o.year || o.month || o.day || o.weekday;
  const hasTime = o.hour || o.minute || o.second;
  const dparts = [];
  if (hasDate) {
    const monthText = o.month === 'long' || o.month === 'short';
    const year = o.year === '2-digit' ? pad(c.year % 100) : o.year ? String(c.year) : '';
    const day = o.day ? (o.day === '2-digit' ? pad(c.day) : String(c.day)) : '';
    const wd = o.weekday ? (o.weekday === 'long' ? dt.weekdayLong : dt.weekdayShort) : '';
    let s;
    if (monthText) {
      const mon = o.month === 'long' ? dt.monthLong : dt.monthShort;
      s = mon + (day ? ' ' + day : '') + (year ? (day ? ', ' : ' ') + year : '');
    } else {
      const mon = o.month ? (o.month === '2-digit' ? pad(c.month) : String(c.month)) : '';
      s = [mon, day, year].filter(Boolean).join('/');
    }
    dparts.push(wd ? (s ? wd + ', ' + s : wd) : s);
  }
  if (hasTime) {
    const h24 = o.hourCycle === 'h23' || o.hour12 === false;
    let h = c.hour;
    let s = '';
    if (o.hour) {
      const hh = h24 ? h : h % 12 || 12;
      s += o.hour === '2-digit' || h24 ? pad(hh) : String(hh);
    }
    if (o.minute) s += (s ? ':' : '') + pad(c.minute);
    if (o.second) s += (s ? ':' : '') + pad(c.second);
    if (o.hour && !h24) s += ' ' + (h < 12 ? 'AM' : 'PM');
    dparts.push(s);
  }
  return dparts.join(', ');
}

const FORMAT_TOKEN_RE = /'[^']*'|(.)\1*/gs;
function formatDT(dt, fmt) {
  const c = dt.c;
  return fmt.replace(FORMAT_TOKEN_RE, (t) => {
    if (t[0] === "'") return t.length === 2 && t === "''" ? "'" : t.slice(1, -1);
    const n = t.length;
    switch (t[0]) {
      case 'S': return n === 1 ? String(c.millisecond) : pad(c.millisecond, 3);
      case 'u': return pad(c.millisecond, 3).slice(0, n === 1 ? 1 : 3);
      case 's': return n === 1 ? String(c.second) : pad(c.second);
      case 'm': return n === 1 ? String(c.minute) : pad(c.minute);
      case 'h': return n === 1 ? String(c.hour % 12 || 12) : pad(c.hour % 12 || 12);
      case 'H': return n === 1 ? String(c.hour) : pad(c.hour);
      case 'a': return c.hour < 12 ? 'AM' : 'PM';
      case 'd': return n === 1 ? String(c.day) : pad(c.day);
      case 'c': case 'E':
        return n === 1 ? String(dt.weekday) : n === 2 ? pad(dt.weekday) : n === 3 ? dt.weekdayShort : n === 4 ? dt.weekdayLong : dt.weekdayLong[0];
      case 'L': case 'M':
        return n === 1 ? String(c.month) : n === 2 ? pad(c.month) : n === 3 ? dt.monthShort : n === 4 ? dt.monthLong : dt.monthLong[0];
      case 'y': return n === 2 ? pad(c.year % 100) : n === 1 ? String(c.year) : pad(c.year, n);
      case 'k': return n === 2 ? pad(dt.weekYear % 100) : pad(dt.weekYear, n === 1 ? 1 : 4);
      case 'W': return n === 1 ? String(dt.weekNumber) : pad(dt.weekNumber);
      case 'o': return n === 1 ? String(dt.ordinal) : pad(dt.ordinal, 3);
      case 'q': return n === 1 ? String(dt.quarter) : pad(dt.quarter);
      case 'Z': return n === 1 ? fmtOffset(dt.o, 'short') : n === 2 ? fmtOffset(dt.o, 'narrow-colon') : fmtOffset(dt.o, 'techie');
      case 'z': return dt.zone.name;
      case 'X': return String(Math.floor(dt.ts / 1000));
      case 'x': return String(dt.ts);
      case 'D': return localeString(dt, [LOCALE_PRESETS.DATE_SHORT, LOCALE_PRESETS.DATE_MED, LOCALE_PRESETS.DATE_FULL, LOCALE_PRESETS.DATE_HUGE][Math.min(n, 4) - 1]);
      case 't': return localeString(dt, n === 1 ? LOCALE_PRESETS.TIME_SIMPLE : LOCALE_PRESETS.TIME_WITH_SECONDS) + (n >= 3 ? ' ' + (dt.zone.kind === 'fixed' ? dt.zone.name : dt.zone.name) : '');
      case 'T': return localeString(dt, n === 1 ? LOCALE_PRESETS.TIME_24_SIMPLE : LOCALE_PRESETS.TIME_24_WITH_SECONDS) + (n >= 3 ? ' ' + dt.zone.name : '');
      case 'f': return localeString(dt, [LOCALE_PRESETS.DATETIME_SHORT, LOCALE_PRESETS.DATETIME_MED, LOCALE_PRESETS.DATETIME_FULL, LOCALE_PRESETS.DATETIME_HUGE][Math.min(n, 4) - 1]);
      case 'F': return localeString(dt, [LOCALE_PRESETS.DATETIME_SHORT_WITH_SECONDS, LOCALE_PRESETS.DATETIME_MED_WITH_SECONDS, LOCALE_PRESETS.DATETIME_MED_WITH_SECONDS, LOCALE_PRESETS.DATETIME_MED_WITH_SECONDS][Math.min(n, 4) - 1]);
      default: return t;
    }
  });
}

// DateTime.fromFormat: token -> regex + setter
function parseWithFormat(text, fmt, o) {
  const zone = makeZone(o.zone);
  const re = [];
  const setters = [];
  const names = (arr, short) => arr.map((x) => (short ? x.slice(0, 3) : x));
  const monthIdx = (s) => MONTHS.findIndex((m) => m.toLowerCase() === s.toLowerCase() || m.slice(0, 3).toLowerCase() === s.toLowerCase()) + 1;
  const toks = fmt.match(FORMAT_TOKEN_RE) || [];
  for (const t of toks) {
    const n = t.length;
    const add = (src, fn) => { re.push('(' + src + ')'); setters.push(fn); };
    if (t[0] === "'") { re.push(t.slice(1, -1).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')); continue; }
    switch (t[0]) {
      case 'y': add(n === 2 ? '\\d{2}' : n === 4 ? '\\d{4}' : n === 6 ? '[+-]?\\d{6}' : '\\d{1,6}', (s, v) => { v.year = n === 2 ? (+s > 60 ? 1900 + +s : 2000 + +s) : +s; }); break;
      case 'M': case 'L':
        if (n <= 2) add(n === 1 ? '\\d{1,2}' : '\\d{2}', (s, v) => { v.month = +s; });
        else add((n === 3 ? names(MONTHS, true) : MONTHS).join('|'), (s, v) => { v.month = monthIdx(s); });
        break;
      case 'd': add(n === 1 ? '\\d{1,2}' : '\\d{2}', (s, v) => { v.day = +s; }); break;
      case 'H': add(n === 1 ? '\\d{1,2}' : '\\d{2}', (s, v) => { v.hour = +s; }); break;
      case 'h': add(n === 1 ? '\\d{1,2}' : '\\d{2}', (s, v) => { v.hour12 = +s; }); break;
      case 'm': add(n === 1 ? '\\d{1,2}' : '\\d{2}', (s, v) => { v.minute = +s; }); break;
      case 's': add(n === 1 ? '\\d{1,2}' : '\\d{2}', (s, v) => { v.second = +s; }); break;
      case 'S': add(n === 1 ? '\\d{1,3}' : '\\d{3}', (s, v) => { v.millisecond = +s; }); break;
      case 'u': add('\\d{1,9}', (s, v) => { v.millisecond = Math.floor(Number('0.' + s) * 1000); }); break;
      case 'a': add('AM|PM|am|pm', (s, v) => { v.pm = /pm/i.test(s); }); break;
      case 'c': case 'E':
        if (n <= 2) add('\\d', () => {});
        else add((n === 3 ? names(WEEKDAYS, true) : WEEKDAYS).join('|'), () => {});
        break;
      case 'o': add('\\d{1,3}', (s, v) => { v.ordinal = +s; }); break;
      case 'Z': add('[+-]\\d{1,2}(?::?\\d{2})?|Z|UTC|GMT', (s, v) => { v.offset = s; }); break;
      case 'z': add('[A-Za-z_]+(?:/[A-Za-z_+\\-0-9]+)*|UTC', (s, v) => { v.zoneName = s; }); break;
      case 'X': add('\\d+', (s, v) => { v.ts = +s * 1000; }); break;
      case 'x': add('\\d+', (s, v) => { v.ts = +s; }); break;
      default: re.push(t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'));
    }
  }
  const m = new RegExp('^' + re.join('') + '$', 'i').exec(text);
  if (!m) return DateTime.invalid('unparsable');
  const v = {};
  setters.forEach((fn, i) => fn(m[i + 1], v));
  if (v.ts !== undefined) return new DateTime(v.ts, zone);
  if (v.hour12 !== undefined) v.hour = (v.hour12 % 12) + (v.pm ? 12 : 0);
  else if (v.pm && v.hour !== undefined && v.hour < 12) v.hour += 12;
  let z = zone;
  if (v.zoneName) z = makeZone(v.zoneName);
  if (v.ordinal !== undefined) {
    const d = new Date(Date.UTC(v.year === undefined ? new Date(env.now).getUTCFullYear() : v.year, 0, v.ordinal));
    v.year = d.getUTCFullYear(); v.month = d.getUTCMonth() + 1; v.day = d.getUTCDate();
  }
  const nowC = new DateTime(env.now, z).c;
  const c = {
    year: v.year !== undefined ? v.year : nowC.year,
    month: v.month !== undefined ? v.month : v.year !== undefined ? 1 : nowC.month,
    day: v.day !== undefined ? v.day : (v.month !== undefined || v.year !== undefined) ? 1 : nowC.day,
    hour: v.hour || 0, minute: v.minute || 0, second: v.second || 0, millisecond: v.millisecond || 0,
  };
  if (v.offset !== undefined) {
    const off = /^(utc|gmt)$/i.test(v.offset) ? 'Z' : v.offset;
    return DateTime.finishParse(c, off, o);
  }
  return DateTime.fromComponents(c, z);
}

class Interval {
  constructor(s, e) { this.s = s; this.e = e; }
  static fromDateTimes(s, e) { return new Interval(s, e); }
  get start() { return this.s; }
  get end() { return this.e; }
  get isValid() { return this.s.isValid && this.e.isValid && this.s.ts <= this.e.ts; }
  length(unit) { return this.toDuration(unit || 'milliseconds').get(unit || 'milliseconds'); }
  count(unit) { return Math.floor(this.e.startOf(unit || 'milliseconds').diff(this.s.startOf(unit || 'milliseconds'), unit || 'milliseconds').get(unit || 'milliseconds')) + 1; }
  toDuration(units) { return this.e.diff(this.s, units); }
  contains(dt) { return this.s.ts <= dt.ts && dt.ts < this.e.ts; }
  toISO() { return this.s.toISO() + '/' + this.e.toISO(); }
  toString() { return '[' + this.s.toISO() + ' – ' + this.e.toISO() + ')'; }
}
