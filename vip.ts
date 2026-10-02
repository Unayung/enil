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
 * Linux：notify-send 的參數。critical 的通知不會自己消失，要人按掉；在 omarchy 上還會繞過
 * 勿擾模式 —— 但那條規則認的是 app 名稱 "notify-send"，所以**不要加 -a**。
 *
 * 內文會被通知系統當成標記語言解析，而它是別人寫的：控制字元拿掉，< > & 跳脫。
 */
export function vipAlertArgs(chatName: string, fromName: string, preview: string): string[] {
  const { title, body } = alertText(chatName, fromName, preview);
  const markup = body.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  // "--" 之後的都是位置參數：標題是別人取的名字，不能讓它被當成選項。
  return ["-u", "critical", "--", title, markup];
}

export const MAC_REPLY_BUTTON = "前往回覆";

/**
 * macOS 的對應物：osascript 的 display dialog，按下按鈕才會消失，跟 critical 的 notify-send 一樣；
 * 也會浮在其他視窗上面（實測：切到別的 app 仍蓋在上面）。回傳按下的按鈕名稱（stdout）。
 *
 * 用 display dialog 而不是 display alert：只有它能換圖示。icons 是圖檔路徑，依序試，第一個
 * 讀得了的就用；全都不行才用 caution。不用 note：osascript 沒有自己的 app 圖示，
 * note 在 macOS 上畫出來是一個資料夾（實測）。圖示壞掉不能讓整個提醒不見。
 *
 * 標題、內文、圖示路徑都走 argv，不拼進 AppleScript 字串 —— 別人寫的引號拼進去就是對你的電腦下指令。
 */
export function macAlertArgs(chatName: string, fromName: string, preview: string, icons: string[]): string[] {
  const { title, body } = alertText(chatName, fromName, preview);
  const dialog = (withIcon: string) =>
    `return button returned of (display dialog (item 2 of argv) with title (item 1 of argv) ` +
    `with icon ${withIcon} buttons {"關閉", "${MAC_REPLY_BUTTON}"} default button "${MAC_REPLY_BUTTON}")`;
  const script = [
    "on run argv",
    "  repeat with i from 3 to count of argv",
    "    try",
    `      ${dialog("(POSIX file (item i of argv))")}`,
    "    end try",
    "  end repeat",
    `  ${dialog("caution")}`,
    "end run",
  ].join("\n");
  // 空字串會被 POSIX file 當成根目錄，畫出資料夾而不是報錯，所以先拿掉。
  return ["-e", script, title, body, ...icons.filter(Boolean)];
}

function alertText(chatName: string, fromName: string, preview: string) {
  const clean = (s: string) => s.replace(/[\u0000-\u0008\u000b-\u001f\u007f]/g, "");
  const title = fromName && fromName !== chatName ? `★ ${chatName}｜${fromName}` : `★ ${chatName}`;
  return { title: clean(title), body: clean(preview).slice(0, 200) };
}
