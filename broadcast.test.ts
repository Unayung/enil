// 名單解析的自我檢查：npx tsx broadcast.test.ts
//
// 只測 resolveTargets —— 送出那段要真的 socket 與真人收件者，不在這裡測。
// 這裡守的是「會把訊息送給錯的人」那類錯誤。

import assert from "node:assert";
import { resolveTargets, restrictionOf } from "./broadcast.js";
import type { Chat } from "./line.js";

const chat = (mid: string, name: string, type = "USER"): Chat =>
  ({ mid, type, name, unread: 0, lastTime: 0 } as Chat);

const A = "u" + "a".repeat(32);
const B = "u" + "b".repeat(32);
const G = "c" + "c".repeat(32);

const chats = [
  chat(A, "Alice"),
  chat(B, "Alice Wang"),
  chat(G, "週末爬山團", "GROUP"),
  chat("u" + "d".repeat(32), "Bob"),
];

const none = new Set<string>();

// mid 直接命中，名字從清單補回來
{
  const r = resolveTargets([A], chats, none);
  assert.deepStrictEqual(r.errors, []);
  assert.deepStrictEqual(r.targets, [{ mid: A, name: "Alice", type: "USER" }]);
}

// 認不得的 mid 要拒發，不能因為「看起來像 mid」就送出去
{
  const r = resolveTargets(["u" + "9".repeat(32)], chats, none);
  assert.strictEqual(r.targets.length, 0);
  assert.match(r.errors[0], /不在聊天室清單裡/);
}

// 名稱唯一命中
{
  const r = resolveTargets(["Bob"], chats, none);
  assert.deepStrictEqual(r.errors, []);
  assert.strictEqual(r.targets[0].name, "Bob");
}

// 名稱多重命中不猜 —— Alice 同時符合 Alice 與 Alice Wang
{
  const r = resolveTargets(["Alice"], chats, none);
  assert.strictEqual(r.targets.length, 0);
  assert.match(r.errors[0], /符合 2 個/);
}

// 名稱找不到
{
  const r = resolveTargets(["查無此人"], chats, none);
  assert.strictEqual(r.targets.length, 0);
  assert.match(r.errors[0], /找不到/);
}

// 註解與空行忽略；同一個對象在名單裡出現兩次只送一次
{
  const r = resolveTargets(["// 客戶名單", "", "Bob", "  Bob  "], chats, none);
  assert.deepStrictEqual(r.errors, []);
  assert.strictEqual(r.targets.length, 1);
}

// 名稱與 mid 指到同一個聊天室，也只算一次
{
  const r = resolveTargets(["Bob", "u" + "d".repeat(32)], chats, none);
  assert.strictEqual(r.targets.length, 1);
}

// 已經送過的進 skipped，不進 targets
{
  const r = resolveTargets(["Bob", "週末爬山團"], chats, new Set([G]));
  assert.deepStrictEqual(r.targets.map((t) => t.name), ["Bob"]);
  assert.deepStrictEqual(r.skipped.map((t) => t.name), ["週末爬山團"]);
}

// 一筆壞的就要讓整批拒發（呼叫端看 errors 非空就停）
{
  const r = resolveTargets(["Bob", "查無此人"], chats, none);
  assert.strictEqual(r.targets.length, 1);
  assert.strictEqual(r.errors.length, 1);
}

// 帳號被限制的碼要從 linejs 塞進 message 的那包 JSON 裡認出來
{
  const real =
    'Request internal failed, sendMessage(/S4) -> {"code":"ABUSE_BLOCK","reason":"","parameterMap":{}}';
  assert.strictEqual(restrictionOf(real), "ABUSE_BLOCK");
}

// 一般的失敗不能誤判成帳號被限制，否則每次網路抖動都在喊狼來了
{
  assert.strictEqual(restrictionOf("daemon 沒在跑"), undefined);
  assert.strictEqual(restrictionOf('{"code":"NOT_FRIEND"}'), undefined);
}

console.log("broadcast: 12 項檢查通過");
