"use strict";

// Runs the probe against fake page objects: no network, no browser.
// node --test userscript/test/

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");

const SCRIPT = path.join(__dirname, "..", "usuyo-sph-probe.user.js");
const { install, shapeOf, safePath } = require(SCRIPT);

const ORIGIN = "https://channels.weixin.qq.com";

function tick() {
  return new Promise((resolve) => setImmediate(resolve));
}

async function settle() {
  for (let i = 0; i < 5; i++) await tick();
}

class FakeEmitter {
  constructor() {
    this.listeners = {};
  }
  addEventListener(type, fn) {
    (this.listeners[type] = this.listeners[type] || []).push(fn);
  }
  emit(type, ev) {
    for (const fn of this.listeners[type] || []) fn.call(this, ev);
  }
}

function fakeWindow(fetchImpl) {
  class FakeXHR extends FakeEmitter {
    constructor() {
      super();
      this.responseType = "";
      this.responseText = "";
      this.response = null;
      this.status = 0;
    }
    open(method, url) {
      this.opened = { method, url };
    }
    send(body) {
      this.sent = body;
    }
    respond(status, text) {
      this.status = status;
      this.responseText = text;
      this.emit("loadend", {});
    }
  }
  class FakeWebSocket extends FakeEmitter {
    constructor(url) {
      super();
      this.url = url;
      this.sentFrames = [];
    }
    send(data) {
      this.sentFrames.push(data);
    }
  }
  FakeWebSocket.OPEN = 1;
  class FakeWorker extends FakeEmitter {
    constructor(url) {
      super();
      this.url = url;
      this.posted = [];
    }
    postMessage(data) {
      this.posted.push(data);
    }
  }
  const doc = new FakeEmitter();
  doc.readyState = "loading";
  return {
    document: doc,
    location: { origin: ORIGIN, href: ORIGIN + "/web/pages/account/like", pathname: "/web/pages/account/like" },
    navigator: { userAgent: "Mozilla/5.0 (Macintosh) Chrome/142.0 Safari/537.36" },
    fetch: fetchImpl || (async () => new Response("{}", { status: 200, headers: { "content-type": "application/json" } })),
    XMLHttpRequest: FakeXHR,
    WebSocket: FakeWebSocket,
    Worker: FakeWorker,
  };
}

// ---------- marker property test ----------

// mulberry32: small seeds still give well-spread first values.
function rng(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 2 ** 32;
  };
}

// Strings that stand for content: ids, nonce ids, urls, names, tokens, text.
function markerString(r, i) {
  const kinds = [
    () => "MARK" + i + "x" + Math.floor(r() * 1e9),
    () => "https://mark" + i + ".example.invalid/v?token=MARK" + i,
    () => String(14545102784038246591n + BigInt(i)),
    () => "v2_0600002310" + i + "MARK@finder",
    () => "标记内容" + i + " MARK" + i,
    () => '{"inner":"MARKJSON' + i + '","n":' + i + "}",
  ];
  return kinds[Math.floor(r() * kinds.length)]();
}

function randomValue(r, depth, markers) {
  const pick = r();
  if (depth > 4 || pick < 0.35) {
    const m = markerString(r, markers.length);
    markers.push(m);
    return m;
  }
  if (pick < 0.45) {
    const n = 900000000 + Math.floor(r() * 99999999);
    markers.push(String(n));
    return n;
  }
  if (pick < 0.7) {
    const len = Math.floor(r() * 4);
    return Array.from({ length: len }, () => randomValue(r, depth + 1, markers));
  }
  const obj = {};
  const n = 1 + Math.floor(r() * 4);
  for (let i = 0; i < n; i++) {
    // Field-name keys, and content-like keys (ids, Chinese text, urls) that must not be emitted.
    if (r() < 0.5) {
      obj["field" + String.fromCharCode(97 + i)] = randomValue(r, depth + 1, markers);
    } else {
      const k = markerString(r, markers.length);
      markers.push(k);
      obj[k] = randomValue(r, depth + 1, markers);
    }
  }
  return obj;
}

test("schema reducer never emits a string or number value from the input", () => {
  let objects = 0;
  for (let seed = 1; seed <= 300; seed++) {
    const r = rng(seed);
    const markers = [];
    // Top level is always an object holding random values, so content-like keys are exercised.
    const value = { root: randomValue(r, 1, markers), [markerString(r, -1)]: randomValue(r, 1, markers) };
    markers.push(Object.keys(value)[1]);
    if (JSON.stringify(value).includes("field")) objects++;
    const out = JSON.stringify(shapeOf(value));
    for (const m of markers) {
      assert.ok(!out.includes(m), `seed ${seed}: marker ${JSON.stringify(m)} leaked into ${out}`);
    }
    assert.ok(!/MARK|标记|mark-?\d/i.test(out), `seed ${seed}: marker fragment leaked into ${out}`);
  }
  assert.ok(objects > 100, "generator produced nested objects");
});

test("schema reducer keeps key paths, types and lengths", () => {
  const s = shapeOf({ objectId: "123", count: 4, list: [{ desc: "hello" }, { desc: "hi", url: "x" }], resp_json: '{"a":[1,2]}' });
  assert.deepEqual(s, {
    objectId: "string:3",
    count: "number",
    list: { $array: 2, $items: { desc: "string:2..5", url: "string:1" } },
    resp_json: { $json: 11, $value: { a: { $array: 2, $items: "number" } } },
  });
});

test("path reducer drops query, hash and id-like segments", () => {
  assert.equal(safePath("/web/api/feed/14545102784038246591/detail"), "/web/api/feed/:x/detail");
  assert.equal(safePath("/cgi-bin/micromsg-bin/pc_findergetfavlist"), "/cgi-bin/micromsg-bin/pc_findergetfavlist");
  assert.equal(safePath("/a/MARK@finder/b"), "/a/:x/b");
});

// ---------- transports ----------

test("fetch hook records request and response with status", async () => {
  const win = fakeWindow(async () => new Response(JSON.stringify({ data: { object: [{ id: "MARKID" }] } }), { status: 200, headers: { "content-type": "application/json" } }));
  const probe = install(win);
  const res = await win.fetch("/web/api/feed/detail?id=MARKQ", { method: "POST", body: JSON.stringify({ objectId: "MARKBODY" }) });
  assert.equal(res.status, 200);
  assert.deepEqual(await res.json(), { data: { object: [{ id: "MARKID" }] } }, "page still reads the untouched body");
  await settle();
  const r = probe.report();
  const req = r.exchanges.find((e) => e.transport === "fetch" && e.direction === "request");
  const resp = r.exchanges.find((e) => e.transport === "fetch" && e.direction === "response");
  assert.equal(req.path, "/web/api/feed/detail");
  assert.deepEqual(req.schema, { objectId: "string:8" });
  assert.equal(resp.statuses[0], 200);
  assert.equal(resp.json, true);
  assert.deepEqual(resp.schema, { data: { object: { $array: 1, $items: { id: "string:6" } } } });
  assert.ok(!JSON.stringify(r).includes("MARK"));
});

test("fetch hook ignores other origins and only counts them", async () => {
  const win = fakeWindow();
  const probe = install(win);
  await win.fetch("https://elsewhere.invalid/x");
  await settle();
  const r = probe.report();
  assert.equal(r.totals.exchanges, 0);
  assert.equal(r.totals.otherOrigin.fetch, 1);
});

test("xhr hook records an exchange", () => {
  const win = fakeWindow();
  const probe = install(win);
  const xhr = new win.XMLHttpRequest();
  xhr.open("POST", "/web/api/util/ua");
  xhr.send("{}");
  xhr.respond(200, JSON.stringify({ errCode: 0, data: "MARKUA" }));
  const r = probe.report();
  const resp = r.exchanges.find((e) => e.transport === "xhr" && e.direction === "response");
  assert.equal(resp.path, "/web/api/util/ua");
  assert.deepEqual(resp.statuses, [200]);
  assert.deepEqual(resp.schema, { errCode: "number", data: "string:6" });
});

test("websocket hook records frames both ways", () => {
  const win = fakeWindow();
  const probe = install(win);
  const ws = new win.WebSocket("wss://channels.weixin.qq.com/ws/feed");
  assert.ok(ws instanceof win.WebSocket, "instanceof still holds");
  assert.equal(win.WebSocket.OPEN, 1, "statics still readable");
  ws.send(JSON.stringify({ cmd: "MARK" }));
  ws.emit("message", { data: new Uint8Array(12).buffer });
  assert.equal(ws.sentFrames.length, 1, "frame still sent");
  const r = probe.report();
  assert.ok(r.exchanges.some((e) => e.transport === "websocket" && e.direction === "send" && e.path === "/ws/feed"));
  const recv = r.exchanges.find((e) => e.transport === "websocket" && e.direction === "receive");
  assert.equal(recv.schema, "binary:12");
});

test("worker hook records messages both ways", () => {
  const win = fakeWindow();
  const probe = install(win);
  const w = new win.Worker("blob:https://channels.weixin.qq.com/abc");
  w.postMessage({ id: 1, apiName: "getSearchConfig", body: { q: "MARK" } });
  w.emit("message", { data: { id: 1, code: 0, data: { list: ["MARK"] } } });
  assert.equal(w.posted.length, 1);
  const r = probe.report();
  const out = r.exchanges.find((e) => e.direction === "to-worker");
  assert.equal(out.name, "getSearchConfig");
  assert.equal(out.path, "blob");
  assert.ok(r.exchanges.some((e) => e.direction === "from-worker"));
  assert.ok(!JSON.stringify(r).includes("MARK"));
});

test("bridge hook records a list answer assigned after install", () => {
  const win = fakeWindow();
  const probe = install(win);
  win.WeixinJSBridge = {
    invoke(api, params, cb) {
      cb({ jsapi_resp: { resp_json: JSON.stringify({ object: [{ id: "MARK1", objectNonceId: "MARK2" }], lastBuffer: "MARK3" }) } });
    },
  };
  let seen = null;
  win.WeixinJSBridge.invoke("finderH5ExtTransfer", { url: "/cgi-bin/micromsg-bin/pc_findergetinteractionedfeedlist", req_json: '{"tab_flag":7}' }, (resp) => {
    seen = resp;
  });
  assert.ok(seen.jsapi_resp.resp_json.includes("MARK1"), "page callback gets the untouched answer");
  const r = probe.report();
  assert.equal(r.env.weixinJSBridge, true);
  assert.equal(r.lists.FinderGetInteractionedFeedList, true);
  assert.equal(r.lists.FinderGetFavList, false);
  const resp = r.exchanges.find((e) => e.transport === "bridge:WeixinJSBridge" && e.direction === "response");
  assert.equal(resp.name, "finderH5ExtTransfer");
  assert.deepEqual(resp.schema.jsapi_resp.resp_json.$value.object, { $array: 1, $items: { id: "string:5", objectNonceId: "string:5" } });
  assert.ok(!JSON.stringify(r).includes("MARK"));
});

test("an empty run still says what the page has", () => {
  const win = fakeWindow();
  const r = install(win).report();
  assert.deepEqual(r.env, { readyStateAtInstall: "loading", weixinJSBridge: false, ilinkJSBridge: false, weixinJSBridgeReady: false, xwebWorker: false, uaTokens: [] });
  assert.deepEqual(r.routes, ["/web/pages/account/like"]);
  assert.ok(!JSON.stringify(r).includes("Chrome"), "user-agent string is not reported");
});

// ---------- collapsing and bound ----------

test("repeats of the same interface collapse into one entry", async () => {
  const win = fakeWindow(async () => new Response(JSON.stringify({ list: [] }), { status: 200, headers: { "content-type": "application/json" } }));
  const probe = install(win);
  for (let i = 0; i < 10; i++) await win.fetch("/web/api/feed/get_comment_list?page=" + i);
  await settle();
  const r = probe.report();
  const resp = r.exchanges.filter((e) => e.path === "/web/api/feed/get_comment_list" && e.direction === "response");
  assert.equal(resp.length, 1);
  assert.equal(resp[0].count, 10);
  assert.equal(r.totals.distinct, 2);
});

test("the number of distinct exchanges is bounded", async () => {
  const win = fakeWindow();
  const probe = install(win);
  for (let i = 0; i < 200; i++) await win.fetch("/web/api/p" + i);
  await settle();
  const r = probe.report();
  assert.equal(r.exchanges.length, 60);
  assert.equal(r.totals.distinct, 60);
  assert.equal(r.totals.dropped, 400 - 60);
  assert.equal(r.totals.exchanges, 400);
  assert.ok(JSON.stringify(r).length < 200000);
});

function bridgeWith(win, answer) {
  win.WeixinJSBridge = {
    invoke(api, params, cb) {
      cb({ jsapi_resp: { resp_json: JSON.stringify(answer) } });
    },
  };
}

test("a list answer after a busy page is still kept and flagged", async () => {
  const win = fakeWindow();
  const probe = install(win);
  for (let i = 0; i < 200; i++) await win.fetch("/web/api/p" + i);
  await settle();
  bridgeWith(win, { objects: [{ id: "MARK" }], lastBuffer: "MARK", continueFlag: 1 });
  win.WeixinJSBridge.invoke("finderH5ExtTransfer", { url: "/cgi-bin/micromsg-bin/pc_findergetplaylist", req_json: "{}" }, () => {});
  const r = probe.report();
  assert.equal(r.lists.FinderGetPlayHistory, true);
  assert.ok(r.exchanges.some((e) => e.path === "/cgi-bin/micromsg-bin/pc_findergetplaylist" && e.direction === "response"));
});

test("a page of large feed entries keeps its siblings and the deep media path", () => {
  const media = { url: "MARK", urlToken: "MARK", decodeKey: "MARK", coverUrl: "MARK", width: 1, height: 1, spec: [{ fileFormat: "MARK", bitRate: 1 }] };
  const entry = {
    id: "MARK",
    objectNonceId: "MARK",
    contact: { username: "MARK", nickname: "MARK", headUrl: "MARK" },
    objectDesc: { description: "MARK", mediaType: 4, media: [media] },
    attachmentList: { attachments: [{ type: 1, video: { video: { desc: { media: [media] } } } }] },
    likeList: Array.from({ length: 30 }, () => ({ username: "MARK", nickname: "MARK" })),
  };
  const win = fakeWindow();
  const probe = install(win);
  bridgeWith(win, { object: Array.from({ length: 20 }, () => entry), lastBuffer: "MARK", continueFlag: 1 });
  win.WeixinJSBridge.invoke("finderH5ExtTransfer", { url: "/cgi-bin/micromsg-bin/pc_findergetinteractionedfeedlist", req_json: "{}" }, () => {});
  const r = probe.report();
  const resp = r.exchanges.find((e) => e.direction === "response");
  const value = resp.schema.jsapi_resp.resp_json.$value;
  assert.equal(value.lastBuffer, "string:4");
  assert.equal(value.continueFlag, "number");
  const deep = value.object.$items.attachmentList.attachments.$items.video.video.desc.media.$items;
  assert.equal(deep.urlToken, "string:4");
  assert.deepEqual(deep.spec, { $array: 1, $items: { fileFormat: "string:4", bitRate: "number" } });
  assert.ok(!JSON.stringify(r).includes("MARK"));
});

test("schema size is bounded for huge answers", () => {
  const big = { list: Array.from({ length: 5000 }, (_, i) => ({ ["k" + (i % 300)]: { a: { b: { c: i } } } })) };
  assert.ok(JSON.stringify(shapeOf(big)).length < 60000);
});

// ---------- script text ----------

const text = fs.readFileSync(SCRIPT, "utf8");

test("script names no address other than the match pattern and the project address", () => {
  const allowed = ["https://channels.weixin.qq.com/web/pages/*", "https://github.com/usuyo-fetch/wechat-channels"];
  let rest = text;
  for (const a of allowed) rest = rest.split(a).join("");
  assert.ok(!/[a-z][a-z0-9+.-]*:\/\//i.test(rest), "no other scheme:// address");
  assert.ok(!/\b[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:com|cn|net|org|io|dev|app|invalid)\b/i.test(rest), "no other host name");
  assert.ok(!/@connect/.test(text), "no @connect");
});

test("script reads no cookie or storage and makes no cross-origin request API call", () => {
  for (const banned of ["document.cookie", "localStorage.", "sessionStorage.", "indexedDB", "GM_xmlhttpRequest", "GM.xmlHttpRequest", "sendBeacon"]) {
    assert.ok(!text.includes(banned), `script contains ${banned}`);
  }
});

test("script never clicks or scrolls the page", () => {
  for (const banned of [".click(", "scrollTo", "scrollBy", "scrollIntoView", "dispatchEvent"]) {
    assert.ok(!text.includes(banned), `script contains ${banned}`);
  }
});

test("match pattern is the only page the script runs on", () => {
  const matches = text.match(/^\/\/ @(match|include)\s+.*$/gm);
  assert.deepEqual(matches, ["// @match        https://channels.weixin.qq.com/web/pages/*"]);
  assert.ok(/^\/\/ @run-at\s+document-start$/m.test(text));
});
