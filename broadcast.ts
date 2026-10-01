// 群發：一份名單，一則訊息。
//
// 收件人是真人，所以預設是「不送」：名單裡有任何一筆解析不出來就整批拒發，
// 沒有 --yes 也只印計畫。寧可多跑一次，不要送錯人。
//
// 為什麼在 CLI 不在 daemon：群發只是「呼叫 N 次現有的 send」加上本地帳務，
// daemon 不需要知道有這回事。ask 由呼叫端傳進來，這個檔案就不必碰 socket。

import { appendFileSync, readFileSync } from "node:fs";
import type { Chat } from "./line.js";
import { SENT_FILE } from "./paths.js";

/** mid 長這樣：u/c/r 開頭 + 32 個 hex。比對得夠緊，顯示名稱不會意外撞上。 */
const MID_RE = /^[ucr][0-9a-f]{32}$/;

/** 名單解析要拿得到全部聊天室 —— 實測 757 個，取整數留餘裕。 */
const CHATS_LIMIT = 1000;

export interface Target { mid: string; name: string; type: string }

export interface Resolution {
  /** 這次要送的 */
  targets: Target[];
  /** 這個 tag 已經送過、要跳過的 */
  skipped: Target[];
  /** 有任何一筆就整批拒發 */
  errors: string[];
}

/**
 * 把名單的每一行對到聊天室。
 *
 * 一行一個對象：符合 MID_RE 的當 mid，其餘當名稱（不分大小寫的子字串比對）。
 * `//` 開頭與空行忽略。
 *
 * 同名不猜 —— 兩個都符合就是拒發，不是擲骰子。認不得的 mid 也拒發：
 * 送到一個沒對過帳的 mid，錯了就收不回來。
 */
export function resolveTargets(
  lines: string[],
  chats: Chat[],
  sent: Set<string>,
): Resolution {
  const byMid = new Map(chats.map((c) => [c.mid, c]));
  const targets: Target[] = [];
  const skipped: Target[] = [];
  const errors: string[] = [];
  const seen = new Set<string>();

  for (const raw of lines) {
    const line = raw.trim();
    if (!line || line.startsWith("//")) continue;

    let chat: Chat | undefined;
    if (MID_RE.test(line)) {
      chat = byMid.get(line);
      if (!chat) {
        errors.push(`${line}：不在聊天室清單裡`);
        continue;
      }
    } else {
      const q = line.toLowerCase();
      const hits = chats.filter((c) => String(c.name || "").toLowerCase().includes(q));
      if (hits.length === 0) {
        errors.push(`${line}：找不到這個聊天室`);
        continue;
      }
      if (hits.length > 1) {
        const names = hits.slice(0, 3).map((h) => h.name).join("、");
        errors.push(`${line}：符合 ${hits.length} 個（${names}…），請改寫成 mid`);
        continue;
      }
      chat = hits[0];
    }

    if (seen.has(chat.mid)) continue; // 名單裡重複只送一次
    seen.add(chat.mid);
    const t: Target = { mid: chat.mid, name: chat.name, type: chat.type };
    (sent.has(chat.mid) ? skipped : targets).push(t);
  }

  return { targets, skipped, errors };
}

/** 某個 tag 已經送出去的 mid。沒給 tag 就沒有續傳。 */
export function alreadySent(tag: string): Set<string> {
  if (!tag) return new Set();
  let text: string;
  try {
    text = readFileSync(SENT_FILE, "utf-8");
  } catch {
    return new Set(); // 還沒送過任何東西
  }
  const out = new Set<string>();
  for (const line of text.split("\n")) {
    if (!line.trim()) continue;
    try {
      const rec = JSON.parse(line);
      if (rec.tag === tag) out.add(rec.mid);
    } catch { /* 壞行跳過，不要因為一行壞掉就整批重送 */ }
  }
  return out;
}

/** 送出成功才記。記錄裡有名字，事後對帳不必再查 mid。 */
export function recordSent(tag: string, t: Target): void {
  if (!tag) return;
  const rec = { tag, mid: t.mid, name: t.name, time: Date.now() };
  appendFileSync(SENT_FILE, JSON.stringify(rec) + "\n", { mode: 0o600 });
}

/**
 * 帳號被 LINE 限制的錯誤碼。linejs 把 thrift 例外整包 JSON.stringify 進
 * Error.message（base/request/mod.ts:245），碼名會原樣出現在字串裡，daemon
 * 再把 message 透傳回來 —— 所以這裡比對字串就夠，不必動 daemon 或 line.ts。
 *
 * 碰到這些不是「這一筆失敗」，是「不要再送了」。
 */
const RESTRICTED = [
  "ABUSE_BLOCK",
  "EXCEED_DAILY_QUOTA",
  "FEATURE_RESTRICTED",
  "SECURITY_CENTER_BLOCKED",
  "TOO_MANY_REQUESTS",
];

/** 錯誤訊息裡有沒有「帳號被限制」的碼。寧可誤報 —— 漏報會讓人重跑進更深的限制。 */
export function restrictionOf(error: string): string | undefined {
  return RESTRICTED.find((code) => error.includes(code));
}

export interface BroadcastOpts {
  listFile: string;
  text: string;
  tag: string;
  delayMs: number;
  dryRun: boolean;
  yes: boolean;
}

type Ask = (req: unknown) => Promise<any>;

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/**
 * 跑一次群發。進度走 stderr，最後的結算走 stdout（跟其他指令一樣是 JSON）。
 * 回傳值就是 exit code。
 */
export async function runBroadcast(ask: Ask, o: BroadcastOpts): Promise<number> {
  const lines = readFileSync(o.listFile, "utf-8").split("\n");

  const st = await ask({ cmd: "state" });
  // 多帳號之後「送錯帳號」是真的會發生的事，所以先報是誰要送。
  const who = st?.data?.me?.displayName ?? "(未登入)";

  const res = await ask({ cmd: "chats", limit: CHATS_LIMIT });
  if (!res.ok) {
    console.error(`讀不到聊天室清單：${res.error}`);
    return 1;
  }
  const chats: Chat[] = res.data;
  const { targets, skipped, errors } = resolveTargets(lines, chats, alreadySent(o.tag));

  const people = targets.filter((t) => t.type === "USER").length;
  console.error(`帳號：${who}`);
  console.error(
    `名單：${targets.length} 個要送（${people} 人 / ${targets.length - people} 群）` +
      (skipped.length ? `，${skipped.length} 個在 tag「${o.tag}」送過了會跳過` : ""),
  );
  for (const t of targets) console.error(`  → ${t.name}`);

  if (errors.length) {
    console.error(`\n有 ${errors.length} 筆解析不出來，整批不送：`);
    for (const e of errors) console.error(`  ✗ ${e}`);
    return 1;
  }
  if (!targets.length) {
    console.error("\n沒有要送的對象。");
    console.log(JSON.stringify({ sent: 0, skipped: skipped.length }, null, 2));
    return 0;
  }
  if (o.dryRun) {
    console.error("\n--dry-run，沒有送出。");
    console.log(JSON.stringify({ sent: 0, wouldSend: targets.length }, null, 2));
    return 0;
  }
  if (!o.yes) {
    // exit 0 會讓呼叫端以為送出去了。沒送就是沒送，要看得出來。
    console.error("\n沒有 --yes，沒有送出。確認上面的名單無誤再加 --yes。");
    return 2;
  }

  let sent = 0;
  for (const [i, t] of targets.entries()) {
    const r = await ask({ cmd: "send", chat: t.mid, text: o.text });
    if (!r.ok) {
      // 一筆失敗就停。憑證掛掉的話，繼續跑只是對著死掉的 session 連打。
      const restricted = restrictionOf(String(r.error ?? ""));
      console.error(`✗ ${t.name}：${r.error}`);
      if (restricted) {
        // 這類錯誤重跑會加重限制，所以連「用 tag 續傳」都不要建議。
        console.error(`\n⚠️  帳號被 LINE 限制了（${restricted}），已送出 ${sent} 個。`);
        console.error("不要重跑 —— 這不是暫時性的錯誤。");
        console.error("先用手機 LINE 確認帳號還正常，再決定要不要繼續。");
      } else {
        console.error(
          `中止，已送出 ${sent} 個。` +
            (o.tag
              ? `用同一個 --tag ${o.tag} 重跑會從中斷的地方接續。`
              : `沒有 --tag，重跑會整批重送 —— 下次記得帶。`),
        );
      }
      console.log(JSON.stringify({ sent, aborted: true, at: t.name, restricted }, null, 2));
      return 1;
    }
    recordSent(o.tag, t);
    sent++;
    console.error(`✓ ${t.name}`);
    if (i < targets.length - 1) await sleep(o.delayMs);
  }

  console.log(JSON.stringify({ sent, skipped: skipped.length }, null, 2));
  return 0;
}
