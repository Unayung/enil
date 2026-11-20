// 讀 archive.jsonl：LINE 對次要裝置只留約兩週，更早的只剩我們自己存的這份（見 CLAUDE.md）。
// 寫的那邊在 daemon.ts 的 archive()。
import { readFileSync } from "node:fs";
import { ARCHIVE_FILE } from "./paths.js";
import type { Msg } from "./line.js";

/**
 * 某聊天室時間 ≤ edge 的最新 count 則，舊到新，不含 skip 裡的 id。補上 archived: true ——
 * 那是收到當下的快照：沒有後來的已讀數、表情、收回，也不能 download（daemon 沒有它的原始資料）。
 * 重連可能把同一則寫兩次，用 id 去重。
 */
export function olderFrom(lines: string[], chat: string, edge: number, count: number, skip: Set<string>): (Msg & { archived: true })[] {
  const byId = new Map<string, Msg>();
  for (const l of lines) {
    if (!l.includes(chat)) continue; // 先粗篩，省掉大部分 JSON.parse
    try {
      const m = JSON.parse(l) as Msg;
      if (m.chat === chat && m.time <= edge && !skip.has(m.id) && !byId.has(m.id)) byId.set(m.id, m);
    } catch { /* 壞掉的一行 */ }
  }
  return [...byId.values()].sort((a, b) => a.time - b.time).slice(-count).map((m) => ({ ...m, archived: true }));
}

/** 存檔裡某則訊息的時間。往前翻頁的錨點是 archive 來的訊息時，daemon 只能從這裡知道它多舊。 */
export function timeIn(lines: string[], id: string): number | undefined {
  for (const l of lines) {
    if (!l.includes(id)) continue;
    try {
      const m = JSON.parse(l) as Msg;
      if (m.id === id) return m.time;
    } catch { /* 壞掉的一行 */ }
  }
}

// ponytail: 每次整份讀進來掃。現在 5k 則、2 MB，幾毫秒；大到有感再按聊天室建索引。
export function archiveLines(): string[] {
  try {
    return readFileSync(ARCHIVE_FILE, "utf8").split("\n");
  } catch {
    return []; // 還沒有存檔
  }
}
