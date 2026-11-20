// 自我檢查：npx tsx apns.test.ts
// 守一件事：JWT 的簽名是 APNs 要的 r||s 格式，拿公鑰驗得過。真的打 APNs 要有金鑰與實機，這裡驗不到。
import assert from "node:assert";
import { generateKeyPairSync, verify } from "node:crypto";
import { activityPayload, apnsJwt } from "./apns.js";

const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
const key = privateKey.export({ type: "pkcs8", format: "pem" }) as string;
const jwt = apnsJwt({ keyId: "KEY1234567", teamId: "TEAM123456", bundleId: "x.y", key }, 1_700_000_000_000);
const [h, p, s] = jwt.split(".");
const json = (x: string) => JSON.parse(Buffer.from(x, "base64url").toString());

assert.deepStrictEqual(json(h), { alg: "ES256", kid: "KEY1234567" });
assert.deepStrictEqual(json(p), { iss: "TEAM123456", iat: 1_700_000_000 });
const sig = Buffer.from(s, "base64url");
assert.strictEqual(sig.length, 64, "ES256 的簽名是 32+32 bytes，DER 會比較長");
assert.ok(verify("sha256", Buffer.from(`${h}.${p}`), { key: publicKey, dsaEncoding: "ieee-p1363" }, sig));

// Live Activity：start 要帶型別名稱與 alert，end 要帶 dismissal-date 而且不出聲。
const card = { chat: "u1", name: "阿明", text: "在嗎", count: 2 };
const aps = (e: "start" | "update" | "end", alert: boolean) => (activityPayload(e, card, alert, 1_700_000_000_000) as any).aps;
assert.deepStrictEqual(aps("start", true), {
  timestamp: 1_700_000_000, event: "start", "content-state": card,
  "attributes-type": "VipAttributes", attributes: {}, "input-push-token": 1,
  alert: { title: "阿明", body: "在嗎", sound: "default" },
});
assert.deepStrictEqual(aps("update", false), { timestamp: 1_700_000_000, event: "update", "content-state": card });
assert.deepStrictEqual(aps("end", true), { timestamp: 1_700_000_000, event: "end", "content-state": card, "dismissal-date": 1_700_000_000 });
console.log("apns ok");
