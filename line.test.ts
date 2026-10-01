// 純函式的自我檢查：npx tsx line.test.ts
//
// 這裡守兩件事：FLEX 攤平出來的文字與連結，以及「對方給的檔名」不能變成路徑。

import assert from "node:assert";
import { createHash, createHmac } from "node:crypto";
import { chunkedMac, flexOf, safeExt } from "./line.js";

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

console.log("ok");
