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
const { callerOf, plainAllowed, chunkedMac, rejectEmptyUpload, flexOf, safeExt, mentionsOf, mentionMeta, reactionName, stickerOf, packOf } = await import("./line.js");

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
  const { setVip, vipList } = await import("./vip.js");
  assert.deepEqual(vipList(), ["u1", "c2"]);
  assert.deepEqual(setVip("u3", true), ["u1", "c2", "u3"]);
  assert.deepEqual(setVip("u3", true), ["u1", "c2", "u3"]); // 設兩次不會重複
  assert.deepEqual(setVip("u1", false), ["c2", "u3"]);
  assert.equal(isVip("u1"), false);
  assert.throws(() => setVip("u9\nu1", true)); // 換行不能混進名單
  assert.deepEqual(vipList(), ["c2", "u3"]);
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

// 快取憑證換不到 token 的原因：被別台踢掉要叫人重新登入，網路問題不要。
{
  const { refreshFailureReason } = await import("./login.js");
  const kicked = refreshFailureReason(new Error('refresh -> {"accessTokenRefreshException":{"errorCode":"INVALID_REQUEST","reasonCode":2}}'));
  assert.ok(kicked.includes("別台機器") && kicked.includes("重新登入"));
  const net = refreshFailureReason(new Error("connect ETIMEDOUT 147.92.185.2:443"));
  assert.ok(net.includes("網路") && net.includes("不必重新登入") && net.includes("ETIMEDOUT"));
}

// 貼圖參數：三個都要是數字，缺一個或夾了別的東西都不算（不能拿去拼網址）。
assert.deepEqual(stickerOf({ STKID: "52002734", STKPKGID: "11537", STKVER: "1" }), { id: "52002734", pkg: "11537", ver: "1" });
assert.equal(stickerOf({ STKID: "52002734", STKPKGID: "11537" }), undefined);
assert.equal(stickerOf({ STKID: "1/../x", STKPKGID: "1", STKVER: "1" }), undefined);
assert.equal(stickerOf(undefined), undefined);

// 擁有的貼圖組：數字鍵的 thrift 結構 → 貼圖 id 清單（同一組連號）。
assert.deepEqual(packOf({ "1": "6427", "11": "Doraemon", "21": 2, "93": { "1": { "1": [{ "1": 11256040, "2": 3 }] } } }),
  { pkg: "6427", ver: "2", name: "Doraemon", stickers: ["11256040", "11256041", "11256042"] });
assert.equal(packOf({ "1": "6427", "21": 1, "93": { "1": { "1": [] } } }), undefined); // 沒有貼圖
assert.equal(packOf({ "1": "x/../", "21": 1, "93": { "1": { "1": [{ "1": 1, "2": 1 }] } } }), undefined); // id 不是數字
assert.equal(packOf({ "1": "1", "21": 1, "93": { "1": { "1": [{ "1": 1, "2": 99999 }] } } }), undefined); // 張數不合理

// 上傳：linejs 被拒時回空的 objId，要變成錯誤（不然一般上傳「成功」、E2EE 拿 OID: "" 送出）。
{
  const obs = { uploadObjectForService: async (o: any) => ({ objId: o.ok ? "abc" : "", headers: new Headers() }) };
  rejectEmptyUpload(obs);
  assert.equal((await obs.uploadObjectForService({ ok: true })).objId, "abc");
  await assert.rejects(obs.uploadObjectForService({ ok: false }), /objId/);
}

// refresh 失敗：伺服器拒絕的要人掃 QR；其他（網路）自動重試。
{
  const { refreshRejected } = await import("./login.js");
  assert.equal(refreshRejected(new Error('refresh -> {"errorCode":"INVALID_REQUEST","reasonCode":3}')), true);
  assert.equal(refreshRejected({ message: "NOT_AUTHORIZED_DEVICE" }), true);
  assert.equal(refreshRejected(new TypeError("fetch failed")), false);
  assert.equal(refreshRejected(new Error("getaddrinfo EAI_AGAIN legy.line-apps.com")), false);
}

// 來電：只有 1:1、不是我、一分鐘內的才響。
{
  const u = "u" + "a".repeat(32), me = "u" + "b".repeat(32), now = 1_800_000_000_000;
  const op = (o: object) => ({ type: "NOTIFIED_RECEIVED_CALL", param1: u, param3: '{"k":"CA"}', createdTime: String(now - 5000), ...o });
  assert.equal(callerOf(op({}), me, now), u);
  assert.equal(callerOf(op({ param3: '{"k":"CA","g":1}' }), me, now), undefined); // 群組通話
  assert.equal(callerOf(op({ param1: me }), me, now), undefined);                  // 我自己（另一台裝置）
  assert.equal(callerOf(op({ createdTime: String(now - 61_000) }), me, now), undefined); // 重開後補收到的舊 op
  assert.equal(callerOf(op({ param1: "c" + "a".repeat(32) }), me, now), undefined); // 不是個人
  assert.equal(callerOf(op({ type: "CANCEL_CALL" }), me, now), undefined);
  assert.equal(callerOf(op({ param3: "" }), me, now), u); // 沒細節：當成 1:1
}

// 送出路線：只有 LINE 明確說對方不收 E2EE 才送明文；其他錯誤不能變成「不加密送出」。
{
  const notSupport = (mid: string) => Object.assign(new Error(mid), { name: "Not support E2EE", type: "Not support E2EE" });
  assert.equal(plainAllowed(notSupport("u1"), "u1"), true);
  assert.equal(plainAllowed(notSupport("u9"), "c1"), false); // 群組協商時問到的是別人
  assert.equal(plainAllowed({ type: "RequestError", message: "x", data: { code: "E2EE_RETRY_PLAIN" } }, "c1"), true);
  assert.equal(plainAllowed(new TypeError("fetch failed"), "u1"), false);
  assert.equal(plainAllowed({ data: { code: "ABUSE_BLOCK" } }, "u1"), false);
  assert.equal(plainAllowed(undefined, "u1"), false);
}

// archive：伺服器只留兩週，更早的從自己存的補。舊到新、同一則只算一次、只看那個聊天室。
{
  const { olderFrom, timeIn } = await import("./archive.js");
  const m = (id: string, chat: string, time: number) => JSON.stringify({ id, chat, time, text: id });
  const lines = [m("1", "c1", 100), m("2", "c2", 150), m("3", "c1", 200), m("3", "c1", 200), "{壞掉", m("4", "c1", 300), m("5", "c1", 400), ""];
  const ids = (xs: { id: string }[]) => xs.map((x) => x.id).join(",");
  assert.equal(ids(olderFrom(lines, "c1", 300, 10, new Set())), "1,3,4");      // ≤ edge、去重、跳過別的聊天室與壞行
  assert.equal(ids(olderFrom(lines, "c1", 300, 2, new Set())), "3,4");         // 取最接近 edge 的那幾則
  assert.equal(ids(olderFrom(lines, "c1", 300, 10, new Set(["4"]))), "1,3");   // 伺服器已經給過的不重複
  assert.equal(olderFrom(lines, "c1", 300, 1, new Set())[0].archived, true);
  assert.equal(timeIn(lines, "4"), 300);
  assert.equal(timeIn(lines, "9"), undefined);
}

// 憑證檔：寫完是 0600、重開讀得回來；壞檔要丟錯，不能當成空的再蓋掉。
{
  const { SafeFileStorage } = await import("./storage.js");
  const { readFileSync, statSync, writeFileSync, chmodSync } = await import("node:fs");
  const f = join(mkdtempSync(join(tmpdir(), "enil-storage-")), "storage.json");
  await new SafeFileStorage(f).set("refreshToken", "r1");
  assert.equal(statSync(f).mode & 0o777, 0o600);
  assert.equal(await new SafeFileStorage(f).get("refreshToken"), "r1");
  chmodSync(f, 0o644); // 舊檔權限太寬：讀進來就收緊
  new SafeFileStorage(f);
  assert.equal(statSync(f).mode & 0o777, 0o600);
  writeFileSync(f, '{"refreshToken":"r');
  assert.throws(() => new SafeFileStorage(f));
  assert.equal(readFileSync(f, "utf8"), '{"refreshToken":"r'); // 殘骸留著給人看
}

console.log("ok");
