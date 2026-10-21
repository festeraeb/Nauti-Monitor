// Headless render check via CDP. Node 22 has a built-in WebSocket, so no deps.
// Uses real time (not --virtual-time-budget, which starves interval loops).
const URL_PAGE = process.argv[2] || "http://127.0.0.1:9122/";
const WAIT_MS = Number(process.argv[3] || 6000);

const log = (...a) => console.error("[rc]", ...a);

const list = await (await fetch("http://127.0.0.1:9222/json/list")).json();
const target = list.find((t) => t.type === "page");
if (!target) {
  console.error("no page target");
  process.exit(1);
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
let id = 0;
const pending = new Map();
const logs = [];

const send = (method, params = {}, timeoutMs = 15000) =>
  new Promise((res, rej) => {
    const msgId = ++id;
    const timer = setTimeout(() => {
      pending.delete(msgId);
      rej(new Error(`timeout on ${method}`));
    }, timeoutMs);
    pending.set(msgId, (v) => {
      clearTimeout(timer);
      res(v);
    });
    ws.send(JSON.stringify({ id: msgId, method, params }));
  });

await new Promise((r) => (ws.onopen = r));
log("ws open");

ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m.result ?? m.error ?? {});
    pending.delete(m.id);
  } else if (m.method === "Runtime.consoleAPICalled") {
    logs.push(
      `[${m.params.type}] ` +
        m.params.args.map((a) => a.value ?? a.description ?? "").join(" "),
    );
  } else if (m.method === "Runtime.exceptionThrown") {
    logs.push(
      "[exception] " +
        (m.params.exceptionDetails.exception?.description ||
          m.params.exceptionDetails.text),
    );
  }
};

await send("Runtime.enable");
log("runtime enabled");
await send("Page.enable");
log("page enabled");
// The WASM bundle must be re-fetched, not served from the previous deploy.
await send("Network.enable").catch(() => {});
await send("Network.setCacheDisabled", { cacheDisabled: true }).catch(() => {});

await send("Page.navigate", { url: URL_PAGE }).catch((e) => log("nav:", e.message));
log("navigated");

// Real wall-clock wait so the WASM can fetch telemetry and paint.
await new Promise((r) => setTimeout(r, WAIT_MS));

const probe = `JSON.stringify({
  app: !!document.querySelector('.app'),
  theme: document.querySelector('.app')?.getAttribute('data-theme') || null,
  gauges: document.querySelectorAll('.gauge').length,
  cards: document.querySelectorAll('.card').length,
  gpuRows: document.querySelectorAll('.gcard').length,
  needles: document.querySelectorAll('.g-needle').length,
  readouts: [...document.querySelectorAll('.g-read')].map(e => e.textContent),
  nodeNames: [...document.querySelectorAll('.c-id h2')].map(e => e.textContent),
  bootHidden: document.getElementById('boot')?.classList.contains('gone') ?? null,
  title: document.querySelector('.hdr h1')?.textContent || null,
  bodyLen: document.body.innerHTML.length
})`;

const { result } = await send("Runtime.evaluate", {
  expression: probe,
  returnByValue: true,
});

console.log("RENDER:", result?.value ?? "(no value)");
console.log("CONSOLE:", logs.length ? logs.join("\n  ") : "(none)");
ws.close();
process.exit(0);

