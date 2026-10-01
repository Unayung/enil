// LINE CLI：跟 daemon 講話，一次一個指令，吐 JSON 到 stdout。
// 沒有登入成本 —— session 在 daemon 手上，這裡只是連 unix socket。
//
//   npx tsx cli.ts chats [limit] [關鍵字]        關鍵字篩選聊天室名稱
//   npx tsx cli.ts read <chat> [count] [--read]  --read 順便標成已讀
//   npx tsx cli.ts download <messageId> [--full] 下載媒體，回傳本機路徑
//   npx tsx cli.ts send <chat> <text...>
//   npx tsx cli.ts sendfile <chat> <path>
//   npx tsx cli.ts broadcast <名單檔> <text...> [--tag N] [--delay 秒] [--dry-run] [--yes]
//   npx tsx cli.ts login                          沒有快取憑證時觸發 QR 登入
//   npx tsx cli.ts state
//
// 成功 exit 0 並印出 JSON；失敗 exit 1 並印出 {"ok":false,"error":...}
import { connect } from "node:net";
import { SOCK } from "./paths.js";
import { runBroadcast } from "./broadcast.js";

function ask(req: unknown): Promise<any> {
  return new Promise((resolve, reject) => {
    const sock = connect(SOCK);
    let buf = "";
    sock.on("connect", () => sock.write(JSON.stringify(req) + "\n"));
    sock.on("data", (chunk) => {
      buf += chunk;
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      sock.destroy();
      try { resolve(JSON.parse(buf.slice(0, nl))); } catch (e) { reject(e); }
    });
    sock.on("error", (e: NodeJS.ErrnoException) => {
      reject(e.code === "ENOENT" || e.code === "ECONNREFUSED"
        ? new Error("daemon 沒在跑（npm run daemon）")
        : e);
    });
  });
}

const argv = process.argv.slice(2);
const [cmd, ...rest] = argv;
const flags = new Set(rest.filter((a) => a.startsWith("--")));
const args = rest.filter((a) => !a.startsWith("--"));

const USAGE = "用法：\n" +
  "  chats [limit] [關鍵字]\n" +
  "  read <chat> [count] [--read]\n" +
  "  download <messageId> [--full]\n" +
  "  send <chat> <text...>\n" +
  "  sendfile <chat> <path>\n" +
  "  broadcast <名單檔> <text...> [--tag 名稱] [--delay 秒] [--dry-run] [--yes]\n" +
  "  squares              （加入的 OpenChat，不在 chats 裡）\n" +
  "  login                （沒有快取憑證時，觸發 QR 登入）\n" +
  "  state";

// broadcast 不是「一個請求一個回應」，所以走自己的路，也自己解析帶值的旗標
// （--tag X / --delay X）—— 下面那組 args/flags 只認得沒有值的 --x。
if (cmd === "broadcast") {
  const valued = new Set(["--tag", "--delay"]);
  const optOf = (name: string) => {
    const i = rest.indexOf(`--${name}`);
    return i >= 0 ? rest[i + 1] : undefined;
  };
  const pos: string[] = [];
  for (let i = 0; i < rest.length; i++) {
    if (valued.has(rest[i])) { i++; continue; }   // 跳過旗標與它的值
    if (rest[i].startsWith("--")) continue;
    pos.push(rest[i]);
  }
  const [listFile, ...textParts] = pos;
  const text = textParts.join(" ");
  if (!listFile || !text) {
    console.error("用法：broadcast <名單檔> <text...> [--tag 名稱] [--delay 秒] [--dry-run] [--yes]");
    process.exitCode = 2;
  } else {
    try {
      process.exitCode = await runBroadcast(ask, {
        listFile,
        text,
        tag: optOf("tag") ?? "",
        delayMs: Number(optOf("delay") ?? 3) * 1000,
        dryRun: flags.has("--dry-run"),
        yes: flags.has("--yes"),
      });
    } catch (err) {
      console.error((err as Error).message);
      process.exitCode = 1;
    }
  }
}

const req = (cmd === "broadcast" ? undefined : (() => {
  switch (cmd) {
    case "chats":
      return { cmd: "chats", limit: args[0] ? Number(args[0]) : undefined, query: args[1] };
    case "read":
      return {
        cmd: "history",
        chat: args[0],
        count: args[1] ? Number(args[1]) : undefined,
        markRead: flags.has("--read"),
      };
    case "download":
      return { cmd: "download", messageId: args[0], preview: !flags.has("--full") };
    case "send":
      return { cmd: "send", chat: args[0], text: args.slice(1).join(" ") };
    case "sendfile":
      return { cmd: "sendFile", chat: args[0], path: args[1] };
    case "login":
      return { cmd: "login" };
    case "squares":
      return { cmd: "squares" };
    case "sread":
      return { cmd: "squareHistory", chat: args[0], count: args[1] ? Number(args[1]) : undefined };
    case "state":
      return { cmd: "state" };
    default:
      return undefined;
  }
})());

if (!req && cmd !== "broadcast") {
  console.error(USAGE);
  process.exitCode = 2;
}

// 用 exitCode 而不是 process.exit()：stdout 導到管線時是非同步寫入，
// 立刻 exit 會把還沒排出去的內容截掉 —— 大量輸出時會吐出「看起來合法但少一截」的 JSON。
if (req) {
  try {
    const res = await ask(req);
    console.log(JSON.stringify(res.ok ? res.data : res, null, 2));
    process.exitCode = res.ok ? 0 : 1;
  } catch (err) {
    console.log(JSON.stringify({ ok: false, error: (err as Error).message }));
    process.exitCode = 1;
  }
}
