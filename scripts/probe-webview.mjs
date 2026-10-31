// 通过 CDP 直接问运行中的 WebView 页面：桥是否就绪、真机数据是否读到。
// 用法：node scripts/probe-webview.mjs <port> <expr>
const port = process.argv[2] ?? "9333";
const expr =
  process.argv[3] ??
  "JSON.stringify(await window.__KALEID__.invoke('roots_list'))";

const res = await fetch(`http://127.0.0.1:${port}/json/list`);
const targets = await res.json();
const page = targets.find((t) => t.type === "page") ?? targets[0];
if (!page) {
  console.error("没有找到页面目标");
  process.exit(2);
}
console.error(`target: ${page.title} ${page.url}`);

const ws = new WebSocket(page.webSocketDebuggerUrl);
let id = 0;
const pending = new Map();

ws.addEventListener("message", (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
});

await new Promise((ok, bad) => {
  ws.addEventListener("open", ok);
  ws.addEventListener("error", bad);
});

function send(method, params) {
  const myId = ++id;
  return new Promise((resolve) => {
    pending.set(myId, resolve);
    ws.send(JSON.stringify({ id: myId, method, params }));
  });
}

const r = await send("Runtime.evaluate", {
  expression: `(async () => { ${expr} })()`,
  awaitPromise: true,
  returnByValue: true,
});

if (r.result?.exceptionDetails) {
  console.log(
    "EXCEPTION: " + JSON.stringify(r.result.exceptionDetails.exception?.description ?? r.result.exceptionDetails),
  );
  process.exit(3);
}
console.log(typeof r.result?.result?.value === "string" ? r.result.result.value : JSON.stringify(r.result?.result?.value));
ws.close();
