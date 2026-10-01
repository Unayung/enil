import { connect as netConnect } from "node:net";
// daemon 與 client 共用的路徑。
import { copyFileSync, chmodSync, existsSync, mkdirSync, renameSync, unlinkSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

/**
 * 這個帳號的狀態目錄。所有東西都掛在這底下，所以換掉它就等於換一個帳號 ——
 * socket、憑證、state、媒體、存檔一起搬，兩個帳號互相看不到對方。
 *
 * ENIL_STATE_DIR 直接指定完整路徑；沒給就沿用 XDG 慣例（多一層 enil/）。
 * 兩個都支援是因為 XDG_STATE_HOME 是「state 的根」，多帳號時會變成
 * ~/.local/state/enil-b/enil 這種多一層的路徑，能讀但不好看。
 */
const stateHome = process.env.XDG_STATE_HOME || join(homedir(), ".local", "state");
export const STATE_DIR = process.env.ENIL_STATE_DIR || join(stateHome, "enil");
export const STATE_FILE = join(STATE_DIR, "state.json");
export const SOCK = join(STATE_DIR, "sock");
export const MEDIA_DIR = join(STATE_DIR, "media");
export const STORAGE_FILE = join(STATE_DIR, "storage.json");
/** 收到的訊息一則一行，只增不刪 —— LINE 伺服器只留約兩週，要長期歷史只能自己存。 */
export const ARCHIVE_FILE = join(STATE_DIR, "archive.jsonl");
/** 群發紀錄，一次送出一行。同一個 --tag 重跑時靠它跳過已送出的對象。 */
export const SENT_FILE = join(STATE_DIR, "sent.jsonl");

/**
 * 建好 STATE_DIR，並把舊的 ./storage.json 搬進去。
 *
 * 憑證本來存在 CWD 的 `./storage.json`，而其他東西都在 STATE_DIR。差別會咬人：
 * 從別的目錄啟動 daemon 就找不到 refreshToken → 重新掃 QR → rotate 掉舊的那把，
 * 等於把原本那份憑證洗掉。這正是「同時只能有一個 process 持有 session」那條約束
 * 被 `cd` 一下就繞過的路。改放 STATE_DIR，路徑就跟從哪裡啟動無關了。
 *
 * 搬家只做一次：新檔已存在就不動舊的（只提醒一聲，那是殘留、可以刪）。
 */
export function prepareStorage(cwd = process.cwd()): void {
  mkdirSync(STATE_DIR, { recursive: true });
  const legacy = join(cwd, "storage.json");
  if (!existsSync(legacy)) return;
  if (existsSync(STORAGE_FILE)) {
    console.warn(`⚠️  ${legacy} 是舊位置的殘留（現在用 ${STORAGE_FILE}），可以刪掉。`);
    return;
  }
  try {
    renameSync(legacy, STORAGE_FILE);
  } catch {
    // 跨檔案系統 rename 會失敗（EXDEV）—— 先複製再刪，不要留下兩份憑證。
    copyFileSync(legacy, STORAGE_FILE);
    unlinkSync(legacy);
  }
  chmodSync(STORAGE_FILE, 0o600); // 憑證：只有自己讀得到
  console.log(`📦 憑證已搬家：${legacy} → ${STORAGE_FILE}`);
}

/**
 * daemon 是否已在跑。會自己登入的程式（例如 probe-e2ee）啟動前要問一下 ——
 * LINE 的 refresh token 是 rotate 的，第二個登入會把第一個的憑證洗掉。
 */
export function daemonAlive(): Promise<boolean> {
  return new Promise((resolve) => {
    const probe = netConnect(SOCK);
    probe.on("connect", () => { probe.destroy(); resolve(true); });
    probe.on("error", () => resolve(false));
  });
}
