// VIP 聊天室：他們的訊息一定要被看到，不管 TUI 有沒有開、有沒有焦點。
//
// 名單是 TUI（Ctrl-P）寫的，一行一個 mid。提醒由 daemon 發 —— 它是唯一一直在跑的。
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { STATE_DIR } from "./paths.js";

export const VIP_FILE = join(STATE_DIR, "vip");

/** 每次都重讀：檔案很小，而且名單一改就要生效，不值得為它做快取失效。 */
export function isVip(chat: string): boolean {
  try {
    return readFileSync(VIP_FILE, "utf8").split("\n").some((l) => l.trim() === chat);
  } catch {
    return false; // 沒有檔案 = 沒有 VIP
  }
}

/**
 * notify-send 的參數。critical 的通知不會自己消失，要人按掉；在 omarchy 上還會繞過
 * 勿擾模式 —— 但那條規則認的是 app 名稱 "notify-send"，所以**不要加 -a**。
 *
 * 內文會被通知系統當成標記語言解析，而它是別人寫的：控制字元拿掉，< > & 跳脫。
 */
export function vipAlertArgs(chatName: string, fromName: string, preview: string): string[] {
  const clean = (s: string) => s.replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, "");
  const title = fromName && fromName !== chatName ? `★ ${chatName}｜${fromName}` : `★ ${chatName}`;
  const body = clean(preview).slice(0, 200)
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  // "--" 之後的都是位置參數：標題是別人取的名字，不能讓它被當成選項。
  return ["-u", "critical", "--", clean(title), body];
}
