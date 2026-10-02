// 純函式的自我檢查：npx tsx line.test.ts
//
// 這裡守兩件事：FLEX 攤平出來的文字與連結，以及「對方給的檔名」不能變成路徑。

import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
// 這份測試會寫 VIP 名單。狀態目錄要在其他 import 之前指到暫存目錄，才不會碰到真的。
process.env.ENIL_STATE_DIR = mkdtempSync(join(tmpdir(), "enil-test-"));

import assert from "node:assert";
import { createHash, createHmac } from "node:crypto";
// 動態 import：靜態 import 會被提到最前面，比上面那行設環境變數還早執行，
// 那樣 paths.js 算出來的就是真的狀態目錄。
const { chunkedMac, flexOf, safeExt, mentionsOf, mentionMeta, reactionName } = await import("./line.js");

const flex = flexOf(JSON.stringify({
  type: "bubble",
  hero: { type: "image", url: "https://cdn.example/a.jpg", action: { type: "uri", uri: "https://example.com/item" } },
  body: {
    type: "box",
    contents: [
      { type: "text", text: " 商品名稱 " },
      { type: "text", text: "忽略我", contents: [{ type: "span", text: "NT$" }, { type: "span", text: "990" }] },
      { type: "text", text: "   " },
    ],
  },
  footer: {
    type: "box",
    contents: [{ type: "button", action: { type: "uri", label: "查看", uri: "https://example.com/item" } }],
  },
}));
assert.deepEqual(flex.images, ["https://cdn.example/a.jpg"]);
assert.equal(flex.text, "商品名稱\nNT$990\n[查看]");   // span 蓋過 text、空白行不留
assert.deepEqual(flex.links, ["https://example.com/item"]); // 同一個連結只留一次

assert.deepEqual(flexOf("不是 JSON"), { images: [], text: "", links: [] });

assert.equal(safeExt("報價單.pdf"), "pdf");
assert.equal(safeExt("archive.tar.gz"), "gz");
assert.equal(safeExt("沒有副檔名"), undefined);
assert.equal(safeExt(undefined), undefined);
// 路徑穿越：取出來的東西不能含分隔符，否則 join() 之後會落在 media 目錄外面。
// 舊寫法（split(".").pop()）對第一個會給 "/../../.bashrc"。
assert.equal(safeExt("x./../../.bashrc"), "bashrc");
assert.equal(safeExt("a.b/c"), undefined);
assert.equal(safeExt("a.sh\n"), undefined);

// 影片的驗證碼：守的是分塊邊界。剛好一塊、多一個 byte、空的，都要跟手算的一致。
{
  const key = Buffer.alloc(32, 7);
  const sha = (b: Buffer) => createHash("sha256").update(b).digest();
  const mac = (...parts: Buffer[]) => createHmac("sha256", key).update(Buffer.concat(parts)).digest();
  const data = Buffer.alloc(131072 + 1, 0xab);
  const [a, b] = [data.subarray(0, 131072), data.subarray(131072)];
  assert.deepEqual(chunkedMac(a, key), mac(sha(a)));
  assert.deepEqual(chunkedMac(data, key), mac(sha(a), sha(b)));
  assert.deepEqual(chunkedMac(Buffer.alloc(0), key), mac());
  // 跟「整份算一次」不是同一個值 —— 不然 linejs 原本就驗得過了。
  assert.notDeepEqual(chunkedMac(a, key), createHmac("sha256", key).update(a).digest());
}

// VIP 提醒的參數：標題與內文都是別人寫的，不能變成選項、標記或控制序列。
{
  const { isVip, vipAlertArgs, VIP_FILE } = await import("./vip.js");
  assert.deepEqual(vipAlertArgs("Alice", "Alice", "在嗎"), ["-u", "critical", "--", "★ Alice", "在嗎"]);
  // 群組：標題帶上是誰說的。
  assert.equal(vipAlertArgs("家人", "媽", "吃飯")[3], "★ 家人｜媽");
  // 沒有 -a：omarchy 繞過勿擾的規則認的是預設的 app 名稱。
  assert.ok(!vipAlertArgs("a", "a", "b").includes("-a"));
  // 內文的標記要跳脫、控制字元拿掉、換行保留、長度有上限。
  const body = vipAlertArgs("a", "a", "<b>1 & 2</b>\u001b[31m\n下一行" + "x".repeat(500))[4];
  assert.ok(body.startsWith("&lt;b&gt;1 &amp; 2&lt;/b&gt;[31m\n下一行"));
  assert.ok(body.length <= 200 + 30); // 200 個字，跳脫後會長一點
  // 名字以 - 開頭也只會是位置參數（在 -- 後面）。
  const dash = vipAlertArgs("--help", "--help", "x");
  assert.ok(dash.indexOf("--") < dash.indexOf("★ --help"));

  // macOS：標題與內文走 argv，不進 AppleScript 原始碼；沒有標記語言，所以不跳脫。
  const { macAlertArgs } = await import("./vip.js");
  const evil = 'x" & (do shell script "id") & "';
  const icon = `/tmp/${evil}.jpg`;
  const mac = macAlertArgs("家人", "媽", `<b>${evil}</b>\u001b\n下一行`, [icon, "", "/b.icns"]);
  assert.equal(mac[0], "-e");
  assert.ok(!mac[1].includes(evil) && !mac[1].includes("家人"));
  assert.equal(mac[2], "★ 家人｜媽");
  assert.equal(mac[3], `<b>${evil}</b>\n下一行`);
  // 圖示依序排在 argv 後面；空字串拿掉（POSIX file "" 是根目錄，會畫成資料夾）。
  assert.deepEqual(mac.slice(4), [icon, "/b.icns"]);
  assert.equal(macAlertArgs("a", "a", "x".repeat(500), [])[3].length, 200);

  // 名單：一行一個 mid，前後空白不算，沒有檔案就是沒有 VIP。
  const { writeFileSync, rmSync } = await import("node:fs");
  assert.ok(VIP_FILE.startsWith(process.env.ENIL_STATE_DIR!));
  assert.equal(isVip("u1"), false);
  writeFileSync(VIP_FILE, " u1 \nc2\n");
  assert.equal(isVip("u1"), true);
  assert.equal(isVip("c2"), true);
  assert.equal(isVip("u"), false); // 整行比對，不是前綴
  rmSync(process.env.ENIL_STATE_DIR!, { recursive: true });
}

// @提及：收到時解析、送出時組回去，兩邊要對得上。
{
  const meta = mentionMeta([{ mid: "u1", start: 0, end: 6 }, { mid: "u2", start: 7, end: 10 }]);
  assert.equal(meta, '{"MENTIONEES":[{"S":"0","E":"6","M":"u1"},{"S":"7","E":"10","M":"u2"}]}');
  assert.deepEqual(mentionsOf(meta), ["u1", "u2"]);
  assert.deepEqual(mentionsOf('{"MENTIONEES":[{"S":"0","E":"4","A":"1"}]}'), ["all"]); // @所有人
  assert.equal(mentionsOf(undefined), undefined);
  assert.equal(mentionsOf("壞掉"), undefined);
  assert.equal(mentionsOf('{"MENTIONEES":[]}'), undefined);
}

// 表情：linejs 有時給編號、有時給名稱。
assert.equal(reactionName(2), "NICE");
assert.equal(reactionName("LOVE"), "LOVE");
assert.equal(reactionName(99), "99");
assert.equal(reactionName(undefined), "OTHER"); // 自訂／付費回應

console.log("ok");
