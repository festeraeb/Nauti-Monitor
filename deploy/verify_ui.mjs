// End-to-end UI verification over CDP: renders the app, checks the DOM really
// has fleet content (not just a boot shell), flips to the Concept theme, and
// confirms the readouts actually move between polls (live telemetry).
// Node 22 has a built-in WebSocket, so this needs no dependencies.
const URL_PAGE = process.argv[2] || "http://127.0.0.1:9122/";
const WAIT_MS = Number(process.argv[3] || 8000);
const CDP = process.argv[4] || "http://127.0.0.1:9222";

const log = (...a) => console.error("[v]", ...a);
const fail = [];
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? "  -> " + detail : ""}`);
  if (!ok) fail.push(name);
};

const list = await (await fetch(`${CDP}/json/list`)).json();
const target = list.find((t) => t.type === "page");
if (!target) {
  console.error("no page target");
  process.exit(1);
}
const ws = new WebSocket(target.webSocketDebuggerUrl);
let id = 0;
const pending = new Map();
const logs = [];

const send = (method, params = {}, timeoutMs = 20000) =>
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
await send("Page.enable");
// The WASM bundle must be re-fetched, not served from the previous deploy.
await send("Network.enable").catch(() => {});
await send("Network.setCacheDisabled", { cacheDisabled: true }).catch(() => {});

const evalJs = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, returnByValue: true });
  return r?.result?.value;
};

const PROBE = `JSON.stringify((() => {
  const q = (s) => [...document.querySelectorAll(s)];
  const app = document.querySelector('.app');
  return {
    app: !!app,
    theme: app?.getAttribute('data-theme') || null,
    fresh: app?.getAttribute('data-fresh') || null,
    // Buick (analog) tree
    gauges: q('.gauge').length,
    needles: q('.g-needle').length,
    cards: q('.card').length,
    gpuRows: q('.gcard').length,
    readouts: q('.g-read').map(e => e.textContent),
    subs: q('.g-sub').map(e => e.textContent),
    nodes: q('.c-id h2').map(e => e.textContent),
    needleDegs: q('.g-needle').map(e => e.getAttribute('transform')),
    // Concept (KITT) tree -- deliberately a DIFFERENT component tree, not a
    // recolour of the one above, so it has its own selectors.
    segd: q('.segd').length,
    seg: q('.seg').length,
    segOn: q('.seg.on').length,
    segRed: q('.seg.red').length,
    kitt: q('.kitt').length,
    regs: q('.reg').length,
    rail: q('.cp-rail').length,
    alerts: q('.cp-alerts').length,
    segNum: q('.segd-num').map(e => e.textContent),
    bootHidden: !!document.getElementById('boot')?.classList.contains('gone'),
    // Broad live fingerprint: every number in whichever body is mounted.
    // (Was '.grid', which no longer exists -- that made this always empty and
    // the liveness check always fail.)
    liveSig: q('.bk-grid, .cp-body').map(e => e.textContent).join('|'),
    tsec: q('.hdr-tsec').map(e => e.textContent).join('|'),
    scrollable: document.documentElement.scrollHeight > window.innerHeight + 2,
    clipped: document.documentElement.scrollHeight > window.innerHeight + 2 &&
             getComputedStyle(document.body).overflowY === 'hidden'
  };
})())`;

await send("Page.navigate", { url: URL_PAGE }).catch((e) => log("nav:", e.message));
log("navigated");
await sleep(WAIT_MS);

const shot = async (path) => {
  const r = await send("Page.captureScreenshot", { format: "png" });
  if (r?.data) {
    const { writeFileSync } = await import("node:fs");
    writeFileSync(path, Buffer.from(r.data, "base64"));
    log("shot ->", path);
  }
};

const a = JSON.parse((await evalJs(PROBE)) || "{}");
console.log("STATE A:", JSON.stringify(a, null, 1));

check("app mounted (.app)", a.app);
check("4 gauges", a.gauges === 4, `got ${a.gauges}`);
check("4 needles", a.needles === 4, `got ${a.needles}`);
check("node cards present", a.cards >= 4, `got ${a.cards}`);
check("GPU rows present", a.gpuRows >= 8, `got ${a.gpuRows}`);
check("boot shell hidden", a.bootHidden);
check("readouts populated", a.readouts?.every((r) => r && r.trim().length), a.readouts?.join(" | "));
// Regression guard: Gauge renders `display` followed by `unit`, so a unit
// symbol baked into BOTH renders as "53%%" / "53CC".
check("no doubled unit in readouts", !(a.readouts || []).some((r) => /%%|CC/.test(r)), a.readouts?.join(" | "));
check("nodes listed", a.nodes?.length >= 4, a.nodes?.join(", "));
await shot("/tmp/ui-buick.png");

// Needles must be spread across the dial, not all pinned at one angle.
const degs = (a.needleDegs || []).map((t) => parseFloat(String(t).replace(/[^-\d.]/g, "")));
const spread = degs.length ? Math.max(...degs) - Math.min(...degs) : 0;
check("needles vary", spread > 5, `deg spread ${spread.toFixed(1)} (${degs.join(", ")})`);

// Flip to Concept via the real button, not by poking state.
const flipped = await evalJs(`(() => {
  const b = [...document.querySelectorAll('.tg')].find(e => /CONCEPT/i.test(e.textContent));
  if (!b) return 'no-button';
  b.click();
  return 'clicked';
})()`);
log("theme click:", flipped);
await sleep(2500);
const b2 = JSON.parse((await evalJs(PROBE)) || "{}");
console.log("STATE B:", JSON.stringify(b2, null, 1));
check("switched to concept", b2.theme === "concept", `theme=${b2.theme}`);
// Concept is a separate component tree, so it must be populated with ITS OWN
// parts -- and the analog parts must be gone, otherwise the two "themes" are
// just one tree with different colours.
check("concept populated (segmented arcs + register + scanner)",
  b2.segd === 4 && b2.regs >= 4 && b2.kitt === 1 && b2.seg > 0,
  `segd=${b2.segd} seg=${b2.seg} (on=${b2.segOn} red=${b2.segRed}) regs=${b2.regs} kitt=${b2.kitt} rail=${b2.rail} alerts=${b2.alerts}`);
check("concept drops the analog tree",
  b2.gauges === 0 && b2.needles === 0 && b2.cards === 0 && b2.gpuRows === 0,
  `gauges=${b2.gauges} needles=${b2.needles} cards=${b2.cards} gpuRows=${b2.gpuRows}`);
check("concept readouts populated",
  (b2.segNum || []).length === 4 && b2.segNum.every((s) => s && s.trim().length),
  (b2.segNum || []).join(" | "));
await shot("/tmp/ui-concept.png");

// Live telemetry: the per-node/per-GPU values must move between two polls.
await sleep(6000);
const c = JSON.parse((await evalJs(PROBE)) || "{}");
const changed = c.liveSig !== b2.liveSig;
// Buick's grid shows per-node RX/TX, which move on every poll. Concept's
// register rail deliberately shows only slowly-changing aggregates (node name
// + peak temp) because full detail lives in the overlay -- so on an idle fleet
// its visible numbers can legitimately be identical for 6s. Liveness there is
// instead proven by the freshness state staying LIVE with a small T-SEC, which
// can only happen if a NEW frame was decoded during the wait.
const tsecNum = (s) => { const m = /([\d.]+)/.exec(String(s || "")); return m ? parseFloat(m[1]) : -1; };
const freshProof = c.fresh === "LIVE" && tsecNum(c.tsec) >= 0 && tsecNum(c.tsec) < 2.5;
check("telemetry is live (new frames arriving)", changed || freshProof,
  changed ? `values differ between polls (tsec ${b2.tsec} -> ${c.tsec})`
          : `displayed aggregates static over 6s but frames still landing: fresh=${c.fresh} tsec=${c.tsec}`);

// T-SEC is driven by an independent 4 Hz wall clock, so it must advance even
// between polls. A counter that only moves when data arrives freezes on a dead
// backend and still looks healthy -- the exact failure this redesign targets.
check("T-SEC advances between polls", c.tsec !== b2.tsec, `${b2.tsec} -> ${c.tsec}`);
check("freshness state exposed", ["BOOT", "LIVE", "STALE", "LOST"].includes(c.fresh), `fresh=${c.fresh}`);


// Node detail overlay: clicking a node opens it, CLOSE dismisses it. Works in
// either theme (.card in Buick, .reg in Concept) -- we are in Concept here.
const opened = await evalJs(`(() => {
  const el = document.querySelector('.card') || document.querySelector('.reg');
  if (!el) return 'no-node-el';
  el.click(); return 'clicked:' + (el.getAttribute('class') || '');
})()`);
log("overlay open:", opened);
await sleep(1200);
const o = JSON.parse((await evalJs(`JSON.stringify({
  ovl: document.querySelectorAll('.ovl').length,
  sheet: document.querySelectorAll('.sheet').length,
  txt: (document.querySelector('.sheet')?.textContent || '').replace(/\\s+/g, ' ')
})`)) || "{}");
check("node click opens detail overlay", o.ovl === 1 && o.sheet === 1,
  `ovl=${o.ovl} sheet=${o.sheet} "${(o.txt || "").slice(0, 90)}"`);
check("overlay shows accelerators + health flags",
  /ACCELERATOR/i.test(o.txt || "") && /HEALTH FLAGS/i.test(o.txt || ""),
  `accel=${/ACCELERATOR/i.test(o.txt || "")} flags=${/HEALTH FLAGS/i.test(o.txt || "")}`);
await shot("/tmp/ui-overlay.png");

const closed = await evalJs(`(() => {
  const b = [...document.querySelectorAll('.sheet button, .sheet-x')].find(e => /CLOSE/i.test(e.textContent));
  if (b) { b.click(); return 'close-button'; }
  const bk = document.querySelector('.ovl');
  if (bk) { bk.click(); return 'backdrop'; }
  return 'nothing-to-close';
})()`);
await sleep(900);
const gone = await evalJs(`document.querySelectorAll('.ovl').length + '/' + document.querySelectorAll('.sheet').length`);
check("overlay dismisses", gone === "0/0", `${closed} -> ovl/sheet=${gone}`);

check("no unreachable content (scrollable, not clipped)", !c.clipped,
  `scrollable=${c.scrollable} clipped=${c.clipped}`);

const errs = logs.filter((l) => l.startsWith("[error]") || l.startsWith("[exception]"));
console.log("CONSOLE:", logs.length ? "\n  " + logs.join("\n  ") : "(none)");
check("no console errors/panics", errs.length === 0, errs.join(" | "));

ws.close();
console.log(fail.length ? `\nFAILURES (${fail.length}): ${fail.join(", ")}` : "\nALL CHECKS PASSED");
process.exit(fail.length ? 1 : 0);
