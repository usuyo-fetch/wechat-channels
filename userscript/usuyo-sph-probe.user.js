// ==UserScript==
// @name         usuyo 视频号探针
// @namespace    https://github.com/usuyo-fetch/wechat-channels
// @version      0.1.0
// @description  记录视频号网页版自己发出的请求和收到的回答的结构，不记录任何内容，也不向任何地方发送。
// @match        https://channels.weixin.qq.com/web/pages/*
// @run-at       document-start
// @grant        none
// @sandbox      raw
// @inject-into  page
// ==/UserScript==

(function () {
  "use strict";

  const PROBE = "usuyo-sph-probe";
  const VERSION = "0.1.0";

  const LIMITS = {
    depth: 24,
    nodes: 8000,
    arrayItems: 4,
    keys: 80,
    union: 4,
    exchanges: 60,
    // Extra room for list-interface entries, so an earlier busy page cannot push them out.
    listExchanges: 16,
    routes: 20,
    textBytes: 4000000,
  };

  // The list interfaces, matched as lowercase tokens in a bridge cgi path or an HTTP path.
  // FinderGetPlayHistory goes to .../findergetplaylist; the 赞和收藏 page uses FinderGetInteractionedFeedList.
  const LISTS = [
    { name: "FinderGetFavList", token: "findergetfavlist", label: "收藏" },
    { name: "FinderGetLikedList", token: "findergetlikedlist", label: "赞" },
    { name: "FinderGetPlayHistory", token: "findergetplaylist", label: "浏览记录" },
    { name: "FinderGetInteractionedFeedList", token: "findergetinteractionedfeedlist", label: "赞和收藏页" },
  ];

  // Only these user-agent tokens are reported, as names; the user-agent string itself never is.
  const UA_TOKENS = ["unifiedpcwindows", "unifiedpcmac", "unifiedpclinux", "unifiedpcohos", "windowswechat", "macwechat", "wxwork", "micromessenger", "xweb"];

  const RESPONSE_DIRECTIONS = new Set(["response", "receive", "from-worker"]);

  // ---------- schema reducer ----------

  function safeKey(key) {
    if (!/^[A-Za-z_$][A-Za-z0-9_$]{0,39}$/.test(key)) return false;
    if ((key.match(/[0-9]/g) || []).length > 3) return false;
    return !/^[0-9a-f]{12,}$/i.test(key);
  }

  function safeName(name) {
    if (typeof name !== "string" || !/^[A-Za-z][A-Za-z0-9_]{0,63}$/.test(name)) return null;
    return (name.match(/[0-9]/g) || []).length > 3 ? null : name;
  }

  function tryJson(text) {
    try {
      return { ok: true, value: JSON.parse(text) };
    } catch (_) {
      return { ok: false };
    }
  }

  function looksJson(text) {
    if (text.length < 2 || text.length > LIMITS.textBytes) return false;
    const c = text.trimStart()[0];
    return c === "{" || c === "[";
  }

  function binaryShape(v) {
    const tag = Object.prototype.toString.call(v);
    if (tag === "[object ArrayBuffer]" || tag === "[object SharedArrayBuffer]") return "binary:" + v.byteLength;
    if (ArrayBuffer.isView(v)) return "binary:" + v.byteLength;
    if (tag === "[object Blob]" || tag === "[object File]") return "blob:" + v.size;
    return null;
  }

  // Reduces a value to its structure: key paths, value types and lengths. Strings become their
  // length, numbers their type. A string holding JSON is opened `jsonLeft` more levels.
  function shapeOf(v, depth, budget, jsonLeft) {
    depth = depth || 0;
    budget = budget || { n: 0 };
    jsonLeft = jsonLeft === undefined ? 1 : jsonLeft;
    if (++budget.n > LIMITS.nodes) return "…";
    if (v === null) return "null";
    switch (typeof v) {
      case "undefined":
      case "boolean":
      case "number":
      case "bigint":
      case "function":
      case "symbol":
        return typeof v;
      case "string":
        if (jsonLeft > 0 && looksJson(v)) {
          const parsed = tryJson(v);
          if (parsed.ok) return { $json: v.length, $value: shapeOf(parsed.value, depth + 1, budget, jsonLeft - 1) };
        }
        return "string:" + v.length;
      default:
        break;
    }
    if (depth >= LIMITS.depth) return "…";
    const bin = binaryShape(v);
    if (bin) return bin;
    if (Array.isArray(v)) {
      if (v.length === 0) return { $array: 0 };
      let items;
      const n = Math.min(v.length, LIMITS.arrayItems);
      for (let i = 0; i < n; i++) {
        const s = shapeOf(v[i], depth + 1, budget, jsonLeft);
        items = i === 0 ? s : mergeShapes(items, s);
      }
      return { $array: v.length, $items: items };
    }
    const tag = Object.prototype.toString.call(v);
    if (tag === "[object Date]") return "date";
    if (tag === "[object Map]" || tag === "[object Set]") return tag.slice(8, -1).toLowerCase() + ":" + v.size;
    const out = {};
    let other = null;
    let otherCount = 0;
    let keys;
    try {
      keys = Object.keys(v);
    } catch (_) {
      return "object";
    }
    let taken = 0;
    for (const k of keys) {
      let child;
      try {
        child = shapeOf(v[k], depth + 1, budget, jsonLeft);
      } catch (_) {
        child = "unreadable";
      }
      if (safeKey(k) && taken < LIMITS.keys) {
        out[k] = child;
        taken++;
      } else {
        otherCount++;
        other = other === null ? child : mergeShapes(other, child);
      }
    }
    if (otherCount) out.$otherKeys = { $count: otherCount, $value: other };
    return out;
  }

  function isRecord(s) {
    return s !== null && typeof s === "object" && !Array.isArray(s);
  }

  function mergeRange(a, b) {
    const lo = Math.min(Array.isArray(a) ? a[0] : a, Array.isArray(b) ? b[0] : b);
    const hi = Math.max(Array.isArray(a) ? a[1] : a, Array.isArray(b) ? b[1] : b);
    return lo === hi ? lo : [lo, hi];
  }

  // Merges two shapes produced by shapeOf, so repeated answers collapse into one schema.
  function mergeShapes(a, b) {
    if (a === undefined || a === "…") return b;
    if (b === undefined || b === "…") return a;
    if (JSON.stringify(a) === JSON.stringify(b)) return a;
    if (typeof a === "string" && typeof b === "string") {
      const ma = /^(string|binary|blob):(\d+)(?:\.\.(\d+))?$/.exec(a);
      const mb = /^(string|binary|blob):(\d+)(?:\.\.(\d+))?$/.exec(b);
      if (ma && mb && ma[1] === mb[1]) {
        const lo = Math.min(+ma[2], +mb[2]);
        const hi = Math.max(+(ma[3] || ma[2]), +(mb[3] || mb[2]));
        return lo === hi ? ma[1] + ":" + lo : ma[1] + ":" + lo + ".." + hi;
      }
    }
    if (isRecord(a) && isRecord(b) && !("$oneOf" in a) && !("$oneOf" in b)) {
      if ("$array" in a && "$array" in b) {
        const out = { $array: mergeRange(a.$array, b.$array) };
        const items = mergeShapes(a.$items, b.$items);
        if (items !== undefined) out.$items = items;
        return out;
      }
      if ("$json" in a && "$json" in b) {
        return { $json: mergeRange(a.$json, b.$json), $value: mergeShapes(a.$value, b.$value) };
      }
      if (!("$array" in a) && !("$array" in b) && !("$json" in a) && !("$json" in b)) {
        const out = {};
        for (const k of Object.keys(a)) out[k] = k in b ? mergeShapes(a[k], b[k]) : a[k];
        for (const k of Object.keys(b)) if (!(k in a)) out[k] = b[k];
        if (a.$otherKeys && b.$otherKeys) {
          out.$otherKeys = {
            $count: Math.max(a.$otherKeys.$count, b.$otherKeys.$count),
            $value: mergeShapes(a.$otherKeys.$value, b.$otherKeys.$value),
          };
        }
        return out;
      }
    }
    const options = [];
    for (const s of [].concat(isRecord(a) && a.$oneOf ? a.$oneOf : [a], isRecord(b) && b.$oneOf ? b.$oneOf : [b])) {
      if (!options.some((o) => JSON.stringify(o) === JSON.stringify(s))) options.push(s);
    }
    return { $oneOf: options.length > LIMITS.union ? options.slice(0, LIMITS.union).concat("…") : options };
  }

  // ---------- paths ----------

  function safeSegment(seg) {
    if (!/^[A-Za-z0-9_.-]{1,40}$/.test(seg)) return ":x";
    if ((seg.match(/[0-9]/g) || []).length > 6) return ":x";
    if (/^[0-9a-f]{16,}$/i.test(seg)) return ":x";
    return seg;
  }

  // Pathname only, with id-like segments replaced; query and hash are dropped.
  function safePath(pathname) {
    return pathname
      .split("/")
      .map((seg, i) => (i === 0 || seg === "" ? seg : safeSegment(seg)))
      .join("/");
  }

  // ---------- probe ----------

  function install(win) {
    const doc = win.document;
    const origin = win.location.origin;
    const state = {
      total: 0,
      dropped: 0,
      hookErrors: 0,
      otherOrigin: { fetch: 0, xhr: 0, websocket: 0, worker: 0 },
      exchanges: [],
      byKey: new Map(),
      kept: { list: 0, other: 0 },
      answered: {},
      readyStateAtInstall: doc && typeof doc.readyState === "string" ? doc.readyState : "unknown",
      routes: [],
      bridgeReady: false,
      bridges: { WeixinJSBridge: false, IlinkJSBridge: false },
    };

    function guard(fn) {
      try {
        fn();
      } catch (_) {
        state.hookErrors++;
      }
    }

    function noteRoute() {
      const p = safePath(win.location.pathname || "");
      if (!state.routes.includes(p) && state.routes.length < LIMITS.routes) state.routes.push(p);
    }

    function listOf(entry) {
      const hay = ((entry.name || "") + " " + (entry.path || "")).toLowerCase();
      return LISTS.find((l) => hay.includes(l.token)) || null;
    }

    function record(entry) {
      noteRoute();
      state.total++;
      const key = [entry.transport, entry.direction, entry.name || "", entry.path || ""].join(" ");
      const found = state.byKey.get(key);
      if (found) {
        found.count++;
        if (entry.status !== undefined && !found.statuses.includes(entry.status)) found.statuses.push(entry.status);
        if (entry.json !== undefined && found.json !== entry.json) found.json = found.json === undefined ? entry.json : "mixed";
        found.schema = mergeShapes(found.schema, entry.schema);
        return;
      }
      const list = listOf(entry);
      if (list && RESPONSE_DIRECTIONS.has(entry.direction)) state.answered[list.name] = true;
      const bucket = list ? "list" : "other";
      if (state.kept[bucket] >= (list ? LIMITS.listExchanges : LIMITS.exchanges)) {
        state.dropped++;
        return;
      }
      state.kept[bucket]++;
      const item = {
        transport: entry.transport,
        direction: entry.direction,
        name: entry.name || null,
        path: entry.path || null,
        count: 1,
        statuses: entry.status === undefined ? [] : [entry.status],
        json: entry.json,
        schema: entry.schema,
      };
      state.byKey.set(key, item);
      state.exchanges.push(item);
    }

    // Same-origin URL to a safe pathname, or null for any other origin.
    function sameOriginPath(url, transport) {
      let u;
      try {
        u = new URL(String(url), win.location.href);
      } catch (_) {
        return null;
      }
      const wsOrigin = u.protocol === "ws:" || u.protocol === "wss:" ? (u.protocol === "wss:" ? "https:" : "http:") + "//" + u.host : null;
      if (u.origin !== origin && wsOrigin !== origin) {
        state.otherOrigin[transport]++;
        return null;
      }
      return safePath(u.pathname);
    }

    // Shape of a text body: parsed as JSON when it is JSON, otherwise its length.
    function textShape(text) {
      if (looksJson(text)) {
        const parsed = tryJson(text);
        if (parsed.ok) return { json: true, schema: shapeOf(parsed.value) };
      }
      return { json: false, schema: "string:" + text.length };
    }

    function bodyShape(body) {
      if (body === undefined || body === null) return { json: undefined, schema: "none" };
      if (typeof body === "string") return textShape(body);
      const tag = Object.prototype.toString.call(body);
      if (tag === "[object URLSearchParams]") return { json: false, schema: "form" };
      if (tag === "[object FormData]") return { json: false, schema: "formdata" };
      if (tag === "[object ReadableStream]") return { json: false, schema: "stream" };
      const bin = binaryShape(body);
      if (bin) return { json: false, schema: bin };
      return { json: false, schema: "object" };
    }

    function messageShape(data) {
      if (typeof data === "string") return textShape(data);
      return { json: false, schema: shapeOf(data) };
    }

    // ----- fetch -----
    if (typeof win.fetch === "function") {
      const origFetch = win.fetch;
      win.fetch = function fetch(input, init) {
        let path = null;
        guard(() => {
          const isRequest = input && typeof input === "object" && typeof input.url === "string" && typeof input.clone === "function";
          path = sameOriginPath(isRequest ? input.url : input, "fetch");
          if (path === null) return;
          if (init && init.body !== undefined) {
            record({ transport: "fetch", direction: "request", path, ...bodyShape(init.body) });
          } else if (isRequest && input.body) {
            input
              .clone()
              .text()
              .then((t) => guard(() => record({ transport: "fetch", direction: "request", path, ...textShape(t) })))
              .catch(() => {});
          } else {
            record({ transport: "fetch", direction: "request", path, json: undefined, schema: "none" });
          }
        });
        const result = origFetch.apply(this, arguments);
        if (path !== null && result && typeof result.then === "function") {
          result.then(
            (res) =>
              guard(() => {
                const type = String((res.headers && res.headers.get && res.headers.get("content-type")) || "");
                const readable = res.type !== "opaque" && res.type !== "opaqueredirect" && res.body && /json|text/.test(type) && !/event-stream/.test(type);
                const len = Number((res.headers && res.headers.get && res.headers.get("content-length")) || 0);
                if (!readable || len > LIMITS.textBytes) {
                  record({ transport: "fetch", direction: "response", path, status: res.status, json: false, schema: type ? "unread" : "none" });
                  return;
                }
                res
                  .clone()
                  .text()
                  .then((t) => guard(() => record({ transport: "fetch", direction: "response", path, status: res.status, ...textShape(t) })))
                  .catch(() => {
                    state.hookErrors++;
                  });
              }),
            () => guard(() => record({ transport: "fetch", direction: "response", path, status: "failed", json: undefined, schema: "none" })),
          );
        }
        return result;
      };
    }

    // ----- XMLHttpRequest -----
    if (typeof win.XMLHttpRequest === "function" && win.XMLHttpRequest.prototype) {
      const proto = win.XMLHttpRequest.prototype;
      const origOpen = proto.open;
      const origSend = proto.send;
      const paths = new WeakMap();
      proto.open = function open(method, url) {
        guard(() => paths.set(this, sameOriginPath(url, "xhr")));
        return origOpen.apply(this, arguments);
      };
      proto.send = function send(body) {
        guard(() => {
          const path = paths.get(this);
          if (!path) return;
          record({ transport: "xhr", direction: "request", path, ...bodyShape(body) });
          const xhr = this;
          xhr.addEventListener("loadend", function () {
            guard(() => {
              const rt = xhr.responseType;
              let shape;
              if (rt === "" || rt === "text") shape = textShape(String(xhr.responseText || ""));
              else if (rt === "json") shape = { json: xhr.response !== null, schema: shapeOf(xhr.response) };
              else if (rt === "arraybuffer" || rt === "blob") shape = { json: false, schema: binaryShape(xhr.response) || "none" };
              else shape = { json: false, schema: rt };
              record({ transport: "xhr", direction: "response", path, status: xhr.status, ...shape });
            });
          });
        });
        return origSend.apply(this, arguments);
      };
    }

    // ----- WebSocket and Workers: constructors wrapped with a Proxy so statics and instanceof keep working -----
    function wrapConstructor(name, onCreate) {
      const Orig = win[name];
      if (typeof Orig !== "function") return;
      win[name] = new Proxy(Orig, {
        construct(target, args, newTarget) {
          const instance = Reflect.construct(target, args, newTarget === win[name] ? target : newTarget);
          guard(() => onCreate(instance, args));
          return instance;
        },
      });
    }

    function wrapSend(obj, method, onSend) {
      const orig = obj[method];
      if (typeof orig !== "function") return;
      obj[method] = function () {
        guard(() => onSend(arguments[0]));
        return orig.apply(this, arguments);
      };
    }

    wrapConstructor("WebSocket", (ws, args) => {
      const path = sameOriginPath(args[0], "websocket");
      if (path === null) return;
      wrapSend(ws, "send", (data) => record({ transport: "websocket", direction: "send", path, ...messageShape(data) }));
      ws.addEventListener("message", (ev) => guard(() => record({ transport: "websocket", direction: "receive", path, ...messageShape(ev.data) })));
    });

    function workerPath(url) {
      const text = String(url);
      if (/^blob:/i.test(text)) return "blob";
      if (/^data:/i.test(text)) return "data";
      return sameOriginPath(text, "worker");
    }

    function workerMessage(direction, path, data) {
      const name = data && typeof data === "object" ? safeName(data.apiName) : null;
      record({ transport: "worker", direction, path, name, ...messageShape(data) });
    }

    wrapConstructor("Worker", (w, args) => {
      const path = workerPath(args[0]);
      if (path === null) return;
      wrapSend(w, "postMessage", (data) => workerMessage("to-worker", path, data));
      w.addEventListener("message", (ev) => guard(() => workerMessage("from-worker", path, ev.data)));
    });

    wrapConstructor("SharedWorker", (w, args) => {
      const path = workerPath(args[0]);
      if (path === null || !w.port) return;
      wrapSend(w.port, "postMessage", (data) => workerMessage("to-worker", path, data));
      w.port.addEventListener("message", (ev) => guard(() => workerMessage("from-worker", path, ev.data)));
    });

    // ----- WeChat client bridges: the list interfaces go through bridge.invoke("finderH5ExtTransfer", {url, req_json}) -----
    function wrapBridge(label, bridge) {
      if (!bridge || typeof bridge.invoke !== "function" || bridge.invoke.__usuyoProbe) return;
      state.bridges[label] = true;
      const origInvoke = bridge.invoke;
      const wrapped = function invoke(api, params, callback) {
        const args = Array.prototype.slice.call(arguments);
        guard(() => {
          const name = safeName(api) || "?";
          const path = params && typeof params.url === "string" ? safePath(params.url.split(/[?#]/)[0]) : null;
          record({ transport: "bridge:" + label, direction: "request", name, path, json: undefined, schema: shapeOf(params) });
          if (typeof callback === "function") {
            args[2] = function (resp) {
              guard(() => record({ transport: "bridge:" + label, direction: "response", name, path, json: undefined, schema: shapeOf(resp) }));
              return callback.apply(this, arguments);
            };
          }
        });
        return origInvoke.apply(this, args);
      };
      wrapped.__usuyoProbe = true;
      try {
        bridge.invoke = wrapped;
      } catch (_) {
        state.hookErrors++;
      }
    }

    for (const label of ["WeixinJSBridge", "IlinkJSBridge"]) {
      guard(() => {
        if (win[label]) {
          wrapBridge(label, win[label]);
          return;
        }
        let value;
        Object.defineProperty(win, label, {
          configurable: true,
          enumerable: true,
          get() {
            return value;
          },
          set(v) {
            value = v;
            guard(() => wrapBridge(label, v));
          },
        });
      });
    }

    if (doc && typeof doc.addEventListener === "function") {
      doc.addEventListener("WeixinJSBridgeReady", () => {
        state.bridgeReady = true;
        guard(() => wrapBridge("WeixinJSBridge", win.WeixinJSBridge));
      });
    }

    // ----- report -----
    function report() {
      noteRoute();
      const ua = String((win.navigator && win.navigator.userAgent) || "").toLowerCase();
      const answered = {};
      for (const l of LISTS) answered[l.name] = Boolean(state.answered[l.name]);
      return {
        probe: PROBE,
        version: VERSION,
        env: {
          readyStateAtInstall: state.readyStateAtInstall,
          weixinJSBridge: state.bridges.WeixinJSBridge || Boolean(win.WeixinJSBridge),
          ilinkJSBridge: state.bridges.IlinkJSBridge || Boolean(win.IlinkJSBridge),
          weixinJSBridgeReady: state.bridgeReady,
          xwebWorker: Boolean(win.xweb && win.xweb.worker),
          uaTokens: UA_TOKENS.filter((t) => ua.includes(t)),
        },
        routes: state.routes.slice(),
        lists: answered,
        totals: {
          exchanges: state.total,
          distinct: state.exchanges.length,
          dropped: state.dropped,
          otherOrigin: Object.assign({}, state.otherOrigin),
          hookErrors: state.hookErrors,
        },
        exchanges: state.exchanges.map((e) => ({
          transport: e.transport,
          direction: e.direction,
          name: e.name,
          path: e.path,
          count: e.count,
          statuses: e.statuses.slice(),
          json: e.json === undefined ? null : e.json,
          schema: e.schema,
        })),
      };
    }

    // ----- panel -----
    function mountPanel() {
      if (!doc.body || doc.getElementById("usuyo-sph-probe")) return;
      const dark = Boolean(win.matchMedia && win.matchMedia("(prefers-color-scheme: dark)").matches);
      const fg = dark ? "#e8e8e8" : "#1f1f1f";
      const muted = dark ? "#9a9a9a" : "#6b6b6b";
      const host = doc.createElement("div");
      host.id = "usuyo-sph-probe";
      Object.assign(host.style, { position: "fixed", right: "16px", bottom: "16px", zIndex: "2147483647" });
      const root = host.attachShadow ? host.attachShadow({ mode: "closed" }) : host;
      const box = doc.createElement("div");
      Object.assign(box.style, {
        width: "260px",
        padding: "12px 14px",
        borderRadius: "8px",
        background: dark ? "#262626" : "#ffffff",
        color: fg,
        border: "1px solid " + (dark ? "#3a3a3a" : "#e2e2e2"),
        boxShadow: "0 2px 10px rgba(0,0,0,0.12)",
        font: "13px/1.5 -apple-system, BlinkMacSystemFont, 'PingFang SC', sans-serif",
      });
      const title = doc.createElement("div");
      title.textContent = "usuyo 视频号探针";
      Object.assign(title.style, { fontWeight: "600", marginBottom: "4px" });
      const count = doc.createElement("div");
      const bridge = doc.createElement("div");
      bridge.style.color = muted;
      const rows = doc.createElement("div");
      rows.style.margin = "6px 0";
      const note = doc.createElement("div");
      note.textContent = "报告只有接口名和数据的结构（字段名、类型、长度），不含任何内容。";
      Object.assign(note.style, { color: muted, fontSize: "12px", marginBottom: "8px" });
      const button = doc.createElement("button");
      button.type = "button";
      button.textContent = "复制报告";
      Object.assign(button.style, {
        font: "inherit",
        padding: "4px 12px",
        borderRadius: "6px",
        border: "1px solid " + (dark ? "#4a4a4a" : "#cfcfcf"),
        background: dark ? "#333333" : "#f5f5f5",
        color: fg,
        cursor: "pointer",
      });
      const status = doc.createElement("div");
      Object.assign(status.style, { color: muted, fontSize: "12px", marginTop: "6px" });
      const fallback = doc.createElement("textarea");
      fallback.readOnly = true;
      Object.assign(fallback.style, { display: "none", width: "100%", height: "80px", marginTop: "6px", boxSizing: "border-box", fontSize: "11px" });

      function refresh() {
        const r = report();
        count.textContent = "已记录 " + r.totals.exchanges + " 次交换（" + r.totals.distinct + " 种）";
        bridge.textContent = r.env.weixinJSBridge || r.env.ilinkJSBridge ? "页面有微信客户端桥接" : "页面没有微信客户端桥接";
        rows.textContent = "";
        for (const l of LISTS) {
          const row = doc.createElement("div");
          row.textContent = l.label + "（" + l.name + "）：" + (r.lists[l.name] ? "有回答" : "未见");
          row.style.color = r.lists[l.name] ? fg : muted;
          row.style.fontSize = "12px";
          rows.appendChild(row);
        }
      }

      button.addEventListener("click", () => {
        const text = JSON.stringify(report(), null, 2);
        const showFallback = () => {
          fallback.value = text;
          fallback.style.display = "block";
          fallback.select();
          status.textContent = "无法写入剪贴板，请手动复制下面的文字。";
        };
        if (win.navigator.clipboard && win.navigator.clipboard.writeText) {
          win.navigator.clipboard.writeText(text).then(() => {
            status.textContent = "已复制，粘贴给我们即可。";
          }, showFallback);
        } else {
          showFallback();
        }
      });

      box.append(title, count, bridge, rows, note, button, status, fallback);
      root.appendChild(box);
      doc.body.appendChild(host);
      refresh();
      win.setInterval(refresh, 1000);
    }

    if (doc && typeof doc.addEventListener === "function") {
      if (doc.readyState === "loading") doc.addEventListener("DOMContentLoaded", mountPanel);
      else mountPanel();
    }

    return { report };
  }

  // In a page this installs; under Node (the tests) it only exports.
  if (typeof window !== "undefined") {
    install(window);
  } else {
    module.exports = { install, shapeOf, safePath };
  }
})();
