// std_shim.js - Standard Web API shims for rho extensions.
//
// Provides console, fetch, URL, URLSearchParams, Headers, Response,
// Request, btoa/atob, and timer stubs.
//
// Wrapped in a block scope to avoid `const` collisions with other scripts
// loaded in the same V8 context (e.g. host_shim.js).
{
const ops = Deno.core.ops;

// - TextEncoder / TextDecoder polyfill -------------------
// V8 typically provides these, but polyfill if missing (needed by btoa/atob).

if (typeof globalThis.TextEncoder === "undefined") {
  globalThis.TextEncoder = class TextEncoder {
    encode(str = "") {
      const bytes = [];
      for (let i = 0; i < str.length; i++) {
        const code = str.codePointAt(i);
        if (code < 0x80) {
          bytes.push(code);
        } else if (code < 0x800) {
          bytes.push(0xc0 | (code >> 6), 0x80 | (code & 63));
        } else if (code < 0x10000) {
          bytes.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 63), 0x80 | (code & 63));
        } else {
          bytes.push(
            0xf0 | (code >> 18),
            0x80 | ((code >> 12) & 63),
            0x80 | ((code >> 6) & 63),
            0x80 | (code & 63),
          );
        }
      }
      return Uint8Array.from(bytes);
    }
    get encoding() {
      return "utf-8";
    }
  };
}

if (typeof globalThis.TextDecoder === "undefined") {
  globalThis.TextDecoder = class TextDecoder {
    decode(input) {
      const bytes = input instanceof Uint8Array ? input : new Uint8Array(input);
      let str = "";
      let i = 0;
      while (i < bytes.length) {
        const b = bytes[i];
        if (b < 0x80) {
          str += String.fromCharCode(b);
          i++;
        } else if (b < 0xe0) {
          str += String.fromCharCode(((b & 31) << 6) | (bytes[i + 1] & 63));
          i += 2;
        } else if (b < 0xf0) {
          str += String.fromCharCode(
            ((b & 15) << 12) | ((bytes[i + 1] & 63) << 6) | (bytes[i + 2] & 63),
          );
          i += 3;
        } else {
          str += String.fromCodePoint(
            ((b & 7) << 18) |
              ((bytes[i + 1] & 63) << 12) |
              ((bytes[i + 2] & 63) << 6) |
              (bytes[i + 3] & 63),
          );
          i += 4;
        }
      }
      return str;
    }
    get encoding() {
      return "utf-8";
    }
  };
}

// - HTTP Status Texts ---------------------------

const STATUS_TEXTS = {
  100: "Continue",
  101: "Switching Protocols",
  200: "OK",
  201: "Created",
  202: "Accepted",
  203: "Non-Authoritative Information",
  204: "No Content",
  205: "Reset Content",
  206: "Partial Content",
  300: "Multiple Choices",
  301: "Moved Permanently",
  302: "Found",
  303: "See Other",
  304: "Not Modified",
  307: "Temporary Redirect",
  308: "Permanent Redirect",
  400: "Bad Request",
  401: "Unauthorized",
  402: "Payment Required",
  403: "Forbidden",
  404: "Not Found",
  405: "Method Not Allowed",
  406: "Not Acceptable",
  407: "Proxy Authentication Required",
  408: "Request Timeout",
  409: "Conflict",
  410: "Gone",
  411: "Length Required",
  412: "Precondition Failed",
  413: "Content Too Large",
  414: "URI Too Long",
  415: "Unsupported Media Type",
  416: "Range Not Satisfiable",
  417: "Expectation Failed",
  418: "I'm a Teapot",
  422: "Unprocessable Content",
  425: "Too Early",
  426: "Upgrade Required",
  428: "Precondition Required",
  429: "Too Many Requests",
  431: "Request Header Fields Too Large",
  451: "Unavailable For Legal Reasons",
  500: "Internal Server Error",
  501: "Not Implemented",
  502: "Bad Gateway",
  503: "Service Unavailable",
  504: "Gateway Timeout",
  505: "HTTP Version Not Supported",
  506: "Variant Also Negotiates",
  507: "Insufficient Storage",
  508: "Loop Detected",
  510: "Not Extended",
  511: "Network Authentication Required",
};

// - Console --------------------------------

const console = globalThis.console || {};

const _consoleLevels = {
  log: "info",
  debug: "debug",
  info: "info",
  warn: "warn",
  error: "error",
  trace: "trace",
};

for (const [method, level] of Object.entries(_consoleLevels)) {
  console[method] = (...args) => {
    const msg = args.map((a) => {
      if (typeof a === "string") return a;
      if (a === undefined) return "undefined";
      if (a === null) return "null";
      try {
        return JSON.stringify(a);
      } catch {
        return String(a);
      }
    }).join(" ");
    ops.op_rho_log(level, msg);
  };
}

console.assert = (condition, ...args) => {
  if (!condition) {
    console.error("Assertion failed:", ...args);
  }
};

console.clear = () => {};

console.count = (() => {
  const counters = {};
  return (label = "default") => {
    counters[label] = (counters[label] || 0) + 1;
    console.log(`${label}: ${counters[label]}`);
  };
})();

console.countReset = (() => {
  const counters = {};
  return (label = "default") => {
    counters[label] = 0;
    console.log(`${label}: 0`);
  };
})();

console.dir = (obj) => console.log(obj);
console.table = (data) => console.log(JSON.stringify(data, null, 2));

console.time = (() => {
  const timers = {};
  return (label = "default") => {
    timers[label] = Date.now();
  };
})();

console.timeEnd = (() => {
  const timers = {};
  return (label = "default") => {
    if (label in timers) {
      console.log(`${label}: ${Date.now() - timers[label]}ms`);
      delete timers[label];
    }
  };
})();

console.timeLog = () => {};

console.group = (...args) => console.log(...args);
console.groupEnd = () => {};
console.groupCollapsed = (...args) => console.log(...args);

globalThis.console = console;

// - Headers --------------------------------

globalThis.Headers = class Headers {
  #entries;

  constructor(init) {
    this.#entries = new Map();
    if (init) {
      if (init instanceof Headers) {
        this.#entries = new Map(init.#entries);
      } else if (typeof init === "object") {
        for (const [key, value] of Object.entries(init)) {
          this.#entries.set(key.toLowerCase(), value);
        }
      }
    }
  }

  get(name) {
    return this.#entries.get(name.toLowerCase()) ?? null;
  }

  set(name, value) {
    this.#entries.set(name.toLowerCase(), String(value));
  }

  append(name, value) {
    const key = name.toLowerCase();
    if (this.#entries.has(key)) {
      this.#entries.set(key, this.#entries.get(key) + ", " + value);
    } else {
      this.#entries.set(key, String(value));
    }
  }

  delete(name) {
    this.#entries.delete(name.toLowerCase());
  }

  has(name) {
    return this.#entries.has(name.toLowerCase());
  }

  *keys() {
    yield* this.#entries.keys();
  }

  *values() {
    yield* this.#entries.values();
  }

  *entries() {
    yield* this.#entries.entries();
  }

  forEach(cb, thisArg) {
    this.#entries.forEach((v, k) => cb.call(thisArg, v, k, this));
  }

  get [Symbol.iterator]() {
    return this.entries();
  }

  toJSON() {
    return Object.fromEntries(this.#entries);
  }
}

// - Response --------------------------------

globalThis.Response = class Response {
  #status;
  #statusText;
  #headers;
  #body;

  constructor(body = "", init = {}) {
    this.#body = body;
    this.#status = init.status ?? 200;
    this.#statusText = init.statusText || STATUS_TEXTS[this.#status] || "";
    this.#headers =
      init.headers instanceof Headers
        ? init.headers
        : new Headers(init.headers || {});
  }

  get ok() {
    return this.#status >= 200 && this.#status < 300;
  }
  get status() {
    return this.#status;
  }
  get statusText() {
    return this.#statusText;
  }
  get headers() {
    return this.#headers;
  }
  get body() {
    return this.#body;
  }

  text() {
    return Promise.resolve(this.#body);
  }

  json() {
    return Promise.resolve(JSON.parse(this.#body));
  }

  static json(data, init = {}) {
    return new Response(JSON.stringify(data), {
      ...init,
      headers: {
        "content-type": "application/json",
        ...(init.headers || {}),
      },
    });
  }

  static error() {
    return new Response("", { status: 0, statusText: "" });
  }

  static redirect(url, status = 302) {
    return new Response("", { status, headers: { location: url } });
  }
}

// - Request ---------------------------------

globalThis.Request = class Request {
  constructor(input, init = {}) {
    this.url = typeof input === "string" ? input : input.url;
    this.method = (init.method || "GET").toUpperCase();
    this.headers =
      init.headers instanceof Headers
        ? init.headers
        : new Headers(init.headers || {});
    this.body = init.body || null;
  }
}

// - Fetch ---------------------------------

/**
 * Helper: unwrap an op result, throwing if it's an error.
 */
function unwrapOpResult(result) {
  if (typeof result === "string" && result.startsWith("__ERROR__")) {
    throw new TypeError(result.slice("__ERROR__".length));
  }
  return result;
}

/**
 * Global fetch() - standard Web Fetch API.
 *
 * Wraps rho's fetchUrl host op. Requires the `network = true` permission.
 *
 * @param {string | Request | URL} input - The resource URL.
 * @param {RequestInit} [init] - Optional init object (method, headers, body, etc).
 * @returns {Promise<Response>}
 */
globalThis.fetch = async function (input, init = {}) {
  if (input instanceof Request) {
    init = { ...input, ...init };
    input = input.url;
  }

  const url = typeof input === "string" ? input : String(input);
  const method = (init.method || "GET").toUpperCase();

  const headers = {};
  if (init.headers) {
    if (init.headers instanceof Headers) {
      for (const [k, v] of init.headers.entries()) headers[k] = v;
    } else {
      Object.assign(headers, init.headers);
    }
  }

  let body = "";
  if (init.body) {
    body = typeof init.body === "string" ? init.body : JSON.stringify(init.body);
  }

  const optsJson = JSON.stringify({
    url,
    method,
    ...(Object.keys(headers).length > 0 ? { headers } : {}),
    ...(body ? { body } : {}),
  });

  const resultJson = unwrapOpResult(ops.op_rho_fetch_url(optsJson));
  const result = JSON.parse(resultJson);

  return new Response(result.body, {
    status: result.status,
    statusText: STATUS_TEXTS[result.status] || "",
    headers: result.headers,
  });
};

// - URL ----------------------------------

globalThis.URL = class URL {
  #components;
  #searchParams;

  /**
   * @param {string} url
   * @param {string | URL} [base]
   */
  constructor(url, base) {
    const baseStr = base instanceof URL ? base.href : base || "";
    const json = unwrapOpResult(ops.op_rho_url_parse(url, baseStr));
    this.#components = JSON.parse(json);
  }

  get href() {
    return this.#components.href;
  }
  get protocol() {
    return this.#components.protocol;
  }
  get username() {
    return this.#components.username;
  }
  get password() {
    return this.#components.password;
  }
  get hostname() {
    return this.#components.hostname;
  }
  get port() {
    return this.#components.port;
  }
  get pathname() {
    return this.#components.pathname;
  }
  get search() {
    return this.#components.search;
  }
  get hash() {
    return this.#components.hash;
  }
  get origin() {
    return this.#components.origin;
  }
  get host() {
    return this.#components.host;
  }
  get searchParams() {
    if (!this.#searchParams) {
      this.#searchParams = new URLSearchParams(this.search);
    }
    return this.#searchParams;
  }

  toString() {
    return this.href;
  }

  toJSON() {
    return this.href;
  }
}

// - URLSearchParams -----------------------------

globalThis.URLSearchParams = class URLSearchParams {
  #pairs;

  /**
   * @param {string | URLSearchParams | [string, string][] | Record<string, string>} init
   */
  constructor(init) {
    if (typeof init === "string") {
      this.#pairs = JSON.parse(ops.op_rho_url_parse_search_params(init));
    } else if (init instanceof URLSearchParams) {
      this.#pairs = [...init.#pairs];
    } else if (Array.isArray(init)) {
      this.#pairs = init.map(([k, v]) => [String(k), String(v)]);
    } else if (init && typeof init === "object") {
      this.#pairs = Object.entries(init).map(([k, v]) => [k, String(v)]);
    } else {
      this.#pairs = [];
    }
  }

  get(name) {
    for (const [k, v] of this.#pairs) {
      if (k === name) return v;
    }
    return null;
  }

  getAll(name) {
    return this.#pairs.filter(([k]) => k === name).map(([, v]) => v);
  }

  has(name) {
    return this.#pairs.some(([k]) => k === name);
  }

  set(name, value) {
    const str = String(value);
    const idx = this.#pairs.findIndex(([k]) => k === name);
    if (idx >= 0) {
      this.#pairs[idx] = [name, str];
    } else {
      this.#pairs.push([name, str]);
    }
  }

  append(name, value) {
    this.#pairs.push([name, String(value)]);
  }

  delete(name) {
    this.#pairs = this.#pairs.filter(([k]) => k !== name);
  }

  get size() {
    return this.#pairs.length;
  }

  toString() {
    return ops.op_rho_url_serialize_search_params(JSON.stringify(this.#pairs));
  }

  *entries() {
    yield* this.#pairs;
  }

  *keys() {
    for (const [k] of this.#pairs) yield k;
  }

  *values() {
    for (const [, v] of this.#pairs) yield v;
  }

  forEach(cb, thisArg) {
    for (const [k, v] of this.#pairs) cb.call(thisArg, v, k, this);
  }

  get [Symbol.iterator]() {
    return this.entries();
  }

  sort() {
    this.#pairs.sort((a, b) => a[0].localeCompare(b[0]));
  }
}

// - btoa / atob ------------------------------

globalThis.btoa = function (str) {
  const bytes = new TextEncoder().encode(str);
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  const chars =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let result = "";
  for (let i = 0; i < binary.length; i += 3) {
    const a = binary.charCodeAt(i);
    const b = i + 1 < binary.length ? binary.charCodeAt(i + 1) : 0;
    const c = i + 2 < binary.length ? binary.charCodeAt(i + 2) : 0;
    const triplet = (a << 16) | (b << 8) | c;
    result += chars[(triplet >> 18) & 63] + chars[(triplet >> 12) & 63];
    result +=
      i + 1 < binary.length ? chars[(triplet >> 6) & 63] : "=";
    result += i + 2 < binary.length ? chars[triplet & 63] : "=";
  }
  return result;
};

globalThis.atob = function (str) {
  const chars =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const lookup = {};
  for (let i = 0; i < chars.length; i++) lookup[chars[i]] = i;

  let binary = "";
  for (let i = 0; i < str.length; i += 4) {
    const a = lookup[str[i]] || 0;
    const b = lookup[str[i + 1]] || 0;
    const c = str[i + 2] === "=" ? 0 : lookup[str[i + 2]] || 0;
    const d = str[i + 3] === "=" ? 0 : lookup[str[i + 3]] || 0;
    const triplet = (a << 18) | (b << 12) | (c << 6) | d;
    binary += String.fromCharCode((triplet >> 16) & 255);
    if (str[i + 2] !== "=") binary += String.fromCharCode((triplet >> 8) & 255);
    if (str[i + 3] !== "=") binary += String.fromCharCode(triplet & 255);
  }
  return new TextDecoder().decode(
    Uint8Array.from({ length: binary.length }, (_, i) => binary.charCodeAt(i)),
  );
};

// - setTimeout / setInterval (stub) --------------------
// Full timer support requires integration with deno_core's event loop timer
// infrastructure. These stubs log a warning so extension authors know to use
// async/await patterns instead.

let _stubTimerWarned = false;

function _warnTimerStub(fn) {
  if (!_stubTimerWarned) {
    ops.op_rho_log(
      "warn",
      `rho-ext: ${fn} is a stub - callback will not fire. Use async/await patterns instead.`,
    );
    _stubTimerWarned = true;
  }
}

globalThis.setTimeout = function (_callback, _delay = 0, ..._args) {
  _warnTimerStub("setTimeout");
  return -1;
};

globalThis.clearTimeout = function () {};

globalThis.setInterval = function (_callback, _delay = 0, ..._args) {
  _warnTimerStub("setInterval");
  return -1;
};

globalThis.clearInterval = function () {};

// - StructuredClone stub -------------------------

globalThis.structuredClone = function (value) {
  return JSON.parse(JSON.stringify(value));
};
}
// end std_shim.js block scope
