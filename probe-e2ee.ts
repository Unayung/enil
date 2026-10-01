// 離線把 failed-e2ee.jsonl 裡的信封逐一試解。
// 只變「信封怎麼拆」的參數，密碼學本身直接用 linejs 的 V1/V2 函式。
// 跑：npx tsx probe-e2ee.ts
import { BaseClient } from "@evex/linejs/base";
import { FileStorage } from "@evex/linejs/storage";
import { keepAuthToken, login } from "./login.js";
import { existsSync, readFileSync } from "node:fs";
import { STORAGE_FILE, daemonAlive } from "./paths.js";

const storage = new FileStorage(STORAGE_FILE);
const base = new BaseClient({ device: "ANDROIDSECONDARY", storage });
const e2ee = base.e2ee as any;

const raw = JSON.parse(readFileSync(STORAGE_FILE, "utf-8")) as Record<string, string>;
const selfKeys = Object.entries(raw)
  .filter(([k]) => k.startsWith("e2eeKeys:"))
  .map(([k, v]) => ({ name: k, ...JSON.parse(v) as { keyId: string; privKey: string; pubKey: string } }));
const pubKeys = Object.entries(raw)
  .filter(([k]) => k.startsWith("e2eePublicKeys:"))
  .map(([k, v]) => ({ keyId: k.slice("e2eePublicKeys:".length), pub: v }));

// linejs 對字串 chunk 一律用 utf-8 解；base64 是另一個可能，兩個都試。
const encodings = ["as-is", "utf-8", "base64"] as const;
const toChunks = (chunks: any[], enc: typeof encodings[number]) =>
  chunks.map((c) =>
    c.enc === "buf"
      ? Buffer.from(c.v, "base64")
      : enc === "base64"
      ? Buffer.from(c.v, "base64")
      : Buffer.from(c.v, "utf-8")
  );

if (!existsSync("./failed-e2ee.jsonl")) {
  console.log("還沒有 failed-e2ee.jsonl —— 先跑 npm start 收到幾則解不開的訊息再回來。");
  process.exit(0);
}
const offlineFailed: any[] = [];
const byteInt = (c: any) => Buffer.from(c.v, c.enc === "buf" ? "base64" : "utf-8").reduce((a, x) => a * 256 + x, 0);
const lines = readFileSync("./failed-e2ee.jsonl", "utf-8").trim().split("\n").filter(Boolean);
console.log(`${lines.length} 個失敗信封，自鑰 ${selfKeys.length} 把，對方公鑰 ${pubKeys.length} 把\n`);

for (const [i, line] of lines.entries()) {
  const env = JSON.parse(line);
  console.log(`── #${i} from=${env.from} toType=${env.toType} ct=${env.contentType} meta.e2eeVersion=${env.contentMetadata?.e2eeVersion ?? "(unset)"}`);
  console.log(`   原始錯誤: ${env.error}`);
  let hit = false;
  for (const enc of encodings) {
    const chunks = toChunks(env.chunks, enc);
    for (const sk of selfKeys) {
      for (const pk of pubKeys) {
        const privK = Buffer.from(sk.privKey, "base64");
        const pubK = Buffer.from(pk.pub, "base64");
        for (const ver of [1, 2]) {
          try {
            const out = ver === 1
              ? e2ee.decryptE2EEMessageV1(chunks, privK, pubK)
              : e2ee.decryptE2EEMessageV2(env.to, env.from, chunks, privK, pubK, ver, env.contentType ?? 0);
            console.log(`   ✅ 解開了 enc=${enc} self=${sk.name} peerKeyId=${pk.keyId} v${ver}`);
            console.log(`      → ${JSON.stringify(out).slice(0, 160)}`);
            hit = true;
          } catch { /* 這組不對，換下一組 */ }
        }
      }
    }
    if (hit) break;
  }
  if (!hit) {
    console.log("   ❌ 所有組合都失敗 —— 不是信封參數的問題");
    offlineFailed.push(env);
  }
  console.log();
}

if (!offlineFailed.length) process.exit(0);

// 離線全敗 → 鑰匙本身有問題。問 LINE 現在登記的公鑰是什麼，跟本機快取比。
console.log("=== 線上比對公鑰 ===");
// 同時只能有一個 session 持有者。daemon 在跑就不要再登入一次。
if (await daemonAlive()) {
  console.error("daemon 正在跑，診斷工具 不能同時登入（會互相洗掉憑證）。\n先 systemctl --user stop enil，或改用 cli.ts。");
  process.exit(1);
}

const client = await login();
await keepAuthToken(client);
const myMid = (await client.getMyProfile()).mid;

const ask = async (mid: string) => {
  const r = await client.base.talk.negotiateE2EEPublicKey({ mid });
  return { keyId: String(r.publicKey.keyId), key: Buffer.from(r.publicKey.keyData).toString("base64"), specVersion: r.specVersion };
};

// 先驗自己：LINE 登記的公鑰若跟本機私鑰配不起來，那對方加密給的就是另一把鑰匙。
const mine = await ask(myMid);
const localSelf = selfKeys[0];
console.log(`我方 LINE 登記: keyId=${mine.keyId} spec=${mine.specVersion}`);
console.log(`我方本機自鑰:   keyId=${localSelf.keyId} pub=${localSelf.pubKey}`);
console.log(mine.keyId === String(localSelf.keyId) && mine.key === localSelf.pubKey
  ? "→ ✅ 一致，私鑰是對的\n"
  : `→ ❌ 不一致！LINE 上的 pub=${mine.key}\n   本機這把私鑰解不開任何發給你的新訊息。\n`);

for (const env of offlineFailed) {
  const isSelf = env.from === myMid;
  const peer = isSelf ? env.to : env.from;
  const wantKeyId = String(isSelf ? byteInt(env.chunks[4]) : byteInt(env.chunks[3]));
  const now = await ask(peer);
  const cached = raw[`e2eePublicKeys:${wantKeyId}`];
  console.log(`對方 ${peer}: 訊息用 keyId=${wantKeyId}，LINE 現在登記 keyId=${now.keyId}`);
  console.log(`   本機快取 ${wantKeyId} = ${cached ?? "(沒有)"}`);
  console.log(`   LINE 目前 ${now.keyId} = ${now.key}`);
  if (now.keyId === wantKeyId && cached && cached !== now.key) {
    console.log("   ⚠️  快取的公鑰跟 LINE 上的不一樣 —— 快取毒掉了");
  }
  // 用剛抓到的公鑰再試一次
  for (const sk of selfKeys) {
    for (const enc of encodings) {
      for (const ver of [1, 2]) {
        try {
          const out = ver === 1
            ? e2ee.decryptE2EEMessageV1(toChunks(env.chunks, enc), Buffer.from(sk.privKey, "base64"), Buffer.from(now.key, "base64"))
            : e2ee.decryptE2EEMessageV2(env.to, env.from, toChunks(env.chunks, enc), Buffer.from(sk.privKey, "base64"), Buffer.from(now.key, "base64"), ver, env.contentType ?? 0);
          console.log(`   ✅ 用新公鑰解開了 (enc=${enc} v${ver}): ${JSON.stringify(out).slice(0, 120)}`);
        } catch { /* 換下一組 */ }
      }
    }
  }
  console.log();
}
process.exit(0);
