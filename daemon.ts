// LINE daemon：唯一持有 LINE session 的 process。
// 跑：npx tsx daemon.ts   （或 systemd user unit，見 enil.service）
//
// 為什麼一定要 daemon（不是每個指令自己登入）：
//   1. refreshToken 是 rotate 的，兩個 process 同時登入會互相洗掉憑證，最後退回掃 QR。
//   2. 收訊息靠 listen() 常駐輪詢，一次性指令拿不到「剛剛進來的訊息」。
//
// 對外兩個介面：
//   - state.json  唯讀快照，給 omarchy bar widget 輪詢（含登入狀態與 QR 路徑）
//   - unix socket 一行一個 JSON 請求，回一行 JSON，給 CLI / MCP / agent
//
// socket 與 state 在「登入之前」就先建立：headless 跑的時候，QR 沒有終端機可印，
// 必須透過 state 交給 UI 顯示，否則第一次登入根本做不到。
import { createServer, connect as netConnect, type Socket } from "node:net";
import qrcode from "qrcode-terminal";
import { appendFileSync, chmodSync, closeSync, existsSync, mkdirSync, openSync, readdirSync, renameSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { execFile } from "node:child_process";
import { homedir } from "node:os";
import { join } from "node:path";
import { type Chat, Line, type Msg } from "./line.js";
import { ARCHIVE_FILE, SOCK, STATE_DIR, STATE_FILE } from "./paths.js";
import { isVip, vipAlertArgs } from "./vip.js";

const RESYNC_MS = 60_000;      // 定期跟伺服器對齊未讀數
const SQUARE_MS = 10_000;      // OpenChat 只能自己拉，理由見 line.ts squareMessages()

// 每產生一次 QR 就換一個檔名。路徑不變的話 UI 端得自己想辦法讓圖片重載，
// 那正是先前 QR 顯示不出來的原因；換檔名讓「路徑變了」自然觸發重載。
let qrSeq = 0;
function nextQrPath() { return join(STATE_DIR, `qr-${++qrSeq}.png`); }

// idle = 沒有可用的快取憑證，等人主動要求 QR 登入。
type LoginStatus = "starting" | "idle" | "qr" | "pin" | "ok" | "error";

interface State {
  me: { mid: string; displayName: string } | null;
  updatedAt: number;
  login: { status: LoginStatus; qrPng?: string; url?: string; pin?: string; error?: string };
  chats: (Chat & { lastText: string; lastFrom: string })[];
  /** VIP 還沒被讀的訊息，舊到新。畫面上要一直看得到，直到那個聊天室被讀了為止。 */
  vip: VipAlert[];
}

interface VipAlert {
  id: string;
  chat: string;
  /** 聊天室名稱 */
  name: string;
  /** 發話者（一對一時跟 name 一樣） */
  from: string;
  text: string;
  time: number;
}

let state: State = {
  me: null,
  updatedAt: 0,
  login: { status: "starting" },
  chats: [],
  vip: [],
};
let line: Line | undefined;

function writeState() {
  state.updatedAt = Date.now();
  // VIP 提醒留到那個聊天室被讀了為止 —— 不管是在這台、在手機上讀的（resync 會把 unread
  // 對齊成 0），都算。每次寫 state 都順手清，不必在每個會動 unread 的地方各寫一次。
  state.vip = state.vip.filter((a) => (state.chats.find((c) => c.mid === a.chat)?.unread ?? 0) > 0);
  // 先寫暫存再 rename：讀的人（FileView）不會看到寫到一半的檔案。
  const tmp = `${STATE_FILE}.tmp`;
  writeFileSync(tmp, JSON.stringify(state, null, 2));
  renameSync(tmp, STATE_FILE);
}

const QRENCODE_HINT = "沒裝 qrencode（Arch: pacman -S qrencode、Debian/Ubuntu: apt install qrencode、macOS: brew install qrencode）";

// QR 網址是活的憑證 —— 掃到的人就能登入這個帳號。先用 0600 建檔再讓 qrencode
// 覆寫（-o 會 truncate、保留權限），避免「先產生後改權限」中間那段可讀的空窗。
//
// qrencode 是外部指令、不是 npm 依賴，沒裝就是產不出 PNG。以前失敗照樣回傳路徑，
// state.json 於是指向一個 0 byte 的檔案 —— headless 第一次登入看起來「有 QR」卻掃不動，
// 而且完全不知道為什麼。現在失敗就把檔案刪掉、回 undefined，讓上層講清楚。
function writeQrPng(url: string): Promise<string | undefined> {
  const path = nextQrPath();
  return new Promise((resolve) => {
    try {
      closeSync(openSync(path, "w", 0o600));
      chmodSync(path, 0o600);
    } catch { /* 建不出來就讓 qrencode 自己建 */ }
    execFile("qrencode", ["-o", path, "-s", "6", "-m", "2", url], (err) => {
      if (!err) return resolve(path);
      rmSync(path, { force: true }); // 別留下 0 byte 的假 QR
      console.error(
        (err as NodeJS.ErrnoException).code === "ENOENT"
          ? `❌ ${QRENCODE_HINT}，QR PNG 產不出來`
          : `❌ qrencode 失敗：${err.message}`,
      );
      resolve(undefined);
    });
  });
}

function clearQr() {
  try {
    for (const f of readdirSync(STATE_DIR)) {
      if (f.startsWith("qr-") && f.endsWith(".png")) rmSync(join(STATE_DIR, f), { force: true });
    }
  } catch { /* 本來就沒有 */ }
}

// ------------------------------------------------------------------ 指令

const handlers: Record<string, (line: Line, req: any) => Promise<unknown>> = {
  async state(_line, req) {
    // 互動介面順便回報「使用者現在正盯著哪個聊天室」（null = 沒有）。只有 VIP 提醒用得到：
    // 正在看的那個不必再彈。帶期限是因為介面可能沒道別就消失（當掉、終端機被關）。
    if ("viewing" in req) {
      viewing = req.viewing ? { chat: String(req.viewing), until: Date.now() + VIEWING_TTL_MS } : undefined;
    }
    return state;
  },
  async login() {
    if (line) return { status: "ok" };
    void startLogin(true);
    return { status: "starting" };
  },
  async chats(line, req) {
    // 只回覆，不寫 state —— 呼叫端給的 limit 是它自己的需求，
    // 拿截斷後的清單覆蓋共用狀態會讓 state.json 憑空少掉聊天室。
    const all = await line.chats(req.limit ?? 30);
    // lastText / lastFrom 只存在於 daemon 的 state（listen() 收到訊息時填的），
    // line.chats() 不會有。帶上去，呼叫端才不用為了看預覽對每個 mid 再讀一次。
    // state 只保留最近 50 個，之外的就沒有預覽。
    const preview = new Map(state.chats.map((c) => [c.mid, c]));
    const enriched = all.map((c) => {
      const p = preview.get(c.mid);
      return { ...c, lastText: p?.lastText ?? "", lastFrom: p?.lastFrom ?? "" };
    });
    const q = String(req.query ?? "").trim().toLowerCase();
    if (!q) return enriched;
    // 跟面板搜尋同一套規則：比對名稱、不分大小寫。
    return enriched.filter((c) => String(c.name || c.mid).toLowerCase().includes(q));
  },
  async squareHistory(line, req) {
    if (!req.chat) throw new Error("缺 chat（squareChatMid，m 開頭）");
    return await line.squareHistory(String(req.chat), req.count ?? 20);
  },
  async squares() {
    if (!line) throw new Error("還沒登入");
    return await line.squares();
  },
  async history(line, req) {
    if (!req.chat) throw new Error("缺 chat");
    const msgs = await line.history(req.chat, req.count ?? 20, req.before);
    // 往前翻頁不該動已讀狀態。
    if (req.markRead && !req.before) {
      // 打開某聊天室就等於看過了。讀不到歷史的聊天室也要能清未讀，
      // 所以 lastMessageId 交給 line.markRead 用錨點決定。
      await line.markRead(req.chat, msgs.length ? msgs[msgs.length - 1].id : undefined);
      const chat = state.chats.find((c) => c.mid === req.chat);
      if (chat) { chat.unread = 0; writeState(); }
    }
    return msgs;
  },
  async download(line, req) {
    // 用 messageId 而不是 id：請求信封本身有一個對帳用的 id，
    // 兩個都叫 id 會互相覆蓋，回應就對不回原本的請求。
    const mid = req.messageId;
    if (!mid) throw new Error("缺 messageId");
    return { path: await line.download(String(mid), Boolean(req.preview)) };
  },
  async send(line, req) {
    if (!req.chat || !req.text) throw new Error("缺 chat 或 text");
    await line.send(req.chat, req.text);
    return { sent: true };
  },
  async sendFile(line, req) {
    if (!req.chat || !req.path) throw new Error("缺 chat 或 path");
    // shell 不會幫我們展開 ~，使用者卻一定會這樣打。
    const path = String(req.path).replace(/^~(?=\/|$)/, homedir());
    return await line.sendMedia(req.chat, path);
  },
};

function mergeChats(chats: Chat[]) {
  const old = new Map(state.chats.map((c) => [c.mid, c]));
  state.chats = chats.map((c) => ({
    ...c,
    lastText: old.get(c.mid)?.lastText ?? "",
    lastFrom: old.get(c.mid)?.lastFrom ?? "",
  }));
}

const PREVIEW_FILL = 15;   // 啟動時補幾個聊天室的預覽

/**
 * lastText / lastFrom 只在收到訊息時才填，所以 daemon 重啟後全是空的 ——
 * 直到有人說話為止。啟動後補抓最近幾個聊天室的最後一則。
 * 不 await：daemon 先開始服務，預覽幾秒後自己補上。
 */
async function fillPreviews() {
  const targets = state.chats.slice(0, PREVIEW_FILL).filter((c) => !c.lastText);
  await Promise.all(targets.map(async (c) => {
    try {
      const msgs = await line!.history(c.mid, 1);
      const m = msgs[msgs.length - 1];
      if (!m) return;
      c.lastText = previewOf(m);
      c.lastFrom = m.fromName;
    } catch { /* 讀不到歷史的聊天室就留白 */ }
  }));
  writeState();
}

/** 聊天室清單要顯示的一行預覽。跟面板的 bodyText 同一套規則。 */
function previewOf(m: Msg): string {
  if (m.decryptFailed) return "[E2EE 解密失敗]";
  if (m.text) return m.text;
  if (m.altText) return m.altText;   // FLEX / RICH 的純文字備援
  return `[${m.contentType}]`;
}

/**
 * 存檔。LINE 對次要裝置只保留約兩週的歷史，超過就 getPreviousMessages 回 0 則 ——
 * 不是錯誤，是伺服器真的沒有了。daemon 是所有新訊息的必經之路，在這裡落地，
 * 之後就只增不減。追不回過去，只能從開始存的那天算起。
 *
 * ponytail: 單一檔案、不輪替、不去重。重連時同一則可能寫兩次，讀的人用 id 去重；
 * 檔案大到不好處理再按月切。訊息內文是隱私，建檔就 0600。
 */
function archive(m: Msg) {
  try {
    appendFileSync(ARCHIVE_FILE, JSON.stringify(m) + "\n", { mode: 0o600 });
  } catch (err) {
    console.warn("存檔失敗：", (err as Error)?.message ?? err);
  }
}

function onIncoming(m: Msg) {
  archive(m);
  const chat = state.chats.find((c) => c.mid === m.chat);
  const text = previewOf(m);
  if (chat) {
    chat.lastText = text;
    chat.lastFrom = m.fromName;
    chat.lastTime = m.time;
    // ponytail: 未讀在本機累加，每 RESYNC_MS 跟伺服器對齊一次。
    if (m.from !== state.me?.mid) chat.unread += 1;
  } else {
    // 沒在清單裡的聊天室（例如剛被加進新群）—— 下次 resync 會補上完整資訊。
    state.chats.unshift({
      mid: m.chat, type: "GROUP", name: m.chat.slice(0, 10),
      unread: m.from === state.me?.mid ? 0 : 1, lastTime: m.time,
      lastText: text, lastFrom: m.fromName,
    });
  }
  state.chats.sort((a, b) => b.lastTime - a.lastTime);

  // VIP：除非使用者正盯著那個聊天室，否則留一筆提醒。要在 writeState 之前，才會跟著寫出去。
  if (m.from !== state.me?.mid && isVip(m.chat)) {
    const watching = viewing && viewing.chat === m.chat && Date.now() < viewing.until;
    if (!watching) {
      const name = chat?.name ?? m.fromName;
      state.vip = [...state.vip, { id: m.id, chat: m.chat, name, from: m.fromName, text, time: m.time }]
        .slice(-VIP_ALERTS_MAX);
      if (!hasOverlay()) alertVip(name, m.fromName, text);
    }
  }

  writeState();
  // 推給訂閱者。OpenChat 不走 onIncoming，所以不會被推送（見 startLogin）。
  const event = JSON.stringify({ event: "message", data: m }) + "\n";
  for (const sock of subscribers) sock.write(event);
}

const VIP_ALERTS_MAX = 20;

/**
 * 有沒有東西會把 state.vip 畫成大彈窗。目前唯一會畫的是 omarchy 外掛（omarchy-line），
 * 所以看它裝了沒。有的話就不另外發 notify-send，不然同一則訊息會跳兩個東西。
 * ponytail: 直接認外掛的安裝路徑。出現第二種會畫彈窗的消費者時，改成讓它們自己登記。
 */
function hasOverlay(): boolean {
  return existsSync(join(homedir(), ".config/omarchy/plugins/io.github.unayung.line/Widget.qml"));
}

// 使用者正盯著的聊天室。介面每 10 秒回報一次，所以期限抓寬一點。
const VIEWING_TTL_MS = 25_000;
let viewing: { chat: string; until: number } | undefined;

/**
 * 沒有大彈窗可用時的退路：一個不會自己消失的系統通知（大小由桌面決定，我們管不了）。
 * 由 daemon 發而不是介面發 —— 介面沒開、縮小、沒焦點的時候才是最需要它的時候。
 * ponytail: 只有 notify-send（Linux）。macOS 沒有對應的「不會消失」通知，要用到再想。
 */
let warnedNoNotify = false;
function alertVip(chatName: string, fromName: string, preview: string) {
  execFile("notify-send", vipAlertArgs(chatName, fromName, preview), (err) => {
    if (!err || warnedNoNotify) return;
    warnedNoNotify = true; // 沒裝的話每則都會失敗，講一次就好
    console.warn("VIP 提醒發不出去：", (err as NodeJS.ErrnoException).code === "ENOENT"
      ? "沒有 notify-send（libnotify）" : err.message);
  });
}

// 送過 subscribe 的連線。之後每則新訊息都會收到一行 { event: "message", data: Msg }。
const subscribers = new Set<Socket>();

// 只有一個 daemon 能跑。先試著連現有的 socket：連得上代表已經有人在跑。
async function claimSocket(): Promise<void> {
  await new Promise<void>((resolve, reject) => {
    const probe = netConnect(SOCK);
    probe.on("connect", () => {
      probe.destroy();
      reject(new Error("已經有一個 daemon 在跑了（socket 有回應）。"));
    });
    probe.on("error", () => {
      try { unlinkSync(SOCK); } catch { /* 本來就沒有 */ }
      resolve();
    });
  });
}

// ------------------------------------------------------------------ 啟動

mkdirSync(STATE_DIR, { recursive: true });
try {
  await claimSocket();
} catch (err) {
  console.error((err as Error).message);
  process.exit(1);
}
clearQr();
writeState();

// 心跳。state.json 的 updatedAt 是消費者判斷「daemon 還活著」的唯一依據
// （omarchy 面板超過三分鐘就顯示 DAEMON OFFLINE）。原本只有登入成功後的 resync
// 會定期寫，於是卡在 idle 等人登入時反而看起來像死了 —— 面板因此把登入鈕藏起來，
// 就再也登不進去。心跳要跟登入狀態無關。
setInterval(writeState, RESYNC_MS);

const server = createServer((sock: Socket) => {
  let buf = "";
  sock.on("data", async (chunk) => {
    buf += chunk;
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const raw = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (!raw.trim()) continue;
      let reply: string;
      let reqId: unknown;
      try {
        const req = JSON.parse(raw);
        reqId = req.id;
        // subscribe 要的是這條連線本身，handlers 拿不到，所以在這層處理。
        // 登入前就能訂：訂閱者不必等 daemon 登入完再重連。
        if (req.cmd === "subscribe") {
          subscribers.add(sock);
          sock.write(JSON.stringify({ ok: true, id: reqId, data: { subscribed: true } }) + "\n");
          continue;
        }
        const handler = handlers[req.cmd];
        if (!handler) throw new Error(`不認得的指令：${req.cmd}`);
        // state 在登入前就能查（UI 靠它顯示 QR）；其餘指令要有 session。
        if (req.cmd !== "state" && req.cmd !== "login" && !line) throw new Error("還沒登入");
        // id 原樣回傳：呼叫端同時發多筆時才不會把回應對錯請求。
        reply = JSON.stringify({ ok: true, id: reqId, data: await handler(line!, req) });
      } catch (err) {
        reply = JSON.stringify({ ok: false, id: reqId, error: (err as Error)?.message ?? String(err) });
      }
      sock.write(reply + "\n");
    }
  });
  sock.on("error", () => { /* client 斷線不是 daemon 的問題 */ });
  sock.on("close", () => subscribers.delete(sock));
});
server.listen(SOCK, () => console.log(`👂 ${SOCK}`));

/**
 * 登入分兩種，只有一種可以自動做：
 *   - 快取 token 續期：不需要人，靜默完成 → 啟動時自動試
 *   - QR 登入：需要有人拿手機在旁邊 → 只在收到 login 指令時才做
 *
 * 混在一起的後果實測過：憑證失效後 daemon 整夜每五分鐘產一張沒人掃的 QR。
 */
let loggingIn = false;

async function startLogin(force: boolean): Promise<void> {
  if (loggingIn || line) return;
  loggingIn = true;
  state.login = { status: "starting" };
  writeState();
  try {
    const client = await Line.connect({
      onQr: async (url) => {
        clearQr();                       // 舊的 QR 已失效，別留著
        const path = await writeQrPng(url);
        // 產不出 PNG 也還是能登入 —— url 本身就是 QR 的內容，UI 可以自己畫。
        // 把原因一起寫進 state，面板才不用猜「為什麼沒有圖」。
        state.login = path
          ? { status: "qr", qrPng: path, url }
          : { status: "qr", url, error: QRENCODE_HINT };
        writeState();
        // 有終端機就也印出來。headless 時只有 state.json 那條路，
        // 但從終端機起 daemon 的人不該還要自己去開 PNG。
        if (process.stdout.isTTY) {
          console.log("\n用手機 LINE 掃下面的 QR：\n" + url + "\n");
          qrcode.generate(url, { small: true });
        } else if (path) {
          console.log(`QR 已寫入 ${path}（enil-tui 的登入畫面會顯示它）`);
        } else {
          console.log(`沒有 PNG，用這個網址登入：\n${url}`);
        }
      },
      onPin: (pin) => {
        state.login = { status: "pin", pin };
        writeState();
        console.log(`\n>>> 到手機 LINE 輸入這組 PIN：  ${pin}\n`);
      },
    }, { qrAllowed: force });
    line = client;
    clearQr();
    state.me = line.me;
    state.login = { status: "ok" };
    mergeChats(await line.chats(50));
    writeState();
    console.log(`✅ ${line.me.displayName}｜${state.chats.length} 個聊天室`);
    void fillPreviews();
    line.onMessage(onIncoming);
    // OpenChat 只存檔，不進 state.chats —— chats() 看不到 square，下一次 resync
    // 就會把它們洗掉，結果是聊天室清單每分鐘閃一次。要顯示得先讓 chats 認識
    // square，那是另一件事。
    setInterval(async () => {
      try {
        for (const m of await line!.squareMessages()) archive(m);
      } catch (err) {
        console.warn("OpenChat 拉取失敗：", (err as Error)?.message ?? err);
      }
    }, SQUARE_MS);
    setInterval(async () => {
      try {
        mergeChats(await line!.chats(50));
        writeState();
      } catch (err) {
        console.warn("resync 失敗：", (err as Error)?.message ?? err);
      }
    }, RESYNC_MS);
  } catch (err) {
    clearQr();
    const message = (err as Error)?.message ?? String(err);
    if (!force) {
      // 沒有可用的快取憑證。不自作主張開 QR —— 等人按登入。
      state.login = { status: "idle" };
      console.log("沒有可用的憑證。開 enil-tui 登入，或跑 cli.ts login。");
    } else {
      console.warn("登入失敗：", message);
      state.login = { status: "error", error: message + "（再按一次登入可重試）" };
    }
    writeState();
  } finally {
    loggingIn = false;
  }
}

// 啟動只試快取憑證；沒有就停在 idle 等人按登入。
void startLogin(false);

for (const sig of ["SIGINT", "SIGTERM"] as const) {
  process.on(sig, () => {
    server.close();
    clearQr();
    try { unlinkSync(SOCK); } catch { /* 已經沒了 */ }
    process.exit(0);
  });
}
