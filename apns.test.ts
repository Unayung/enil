// 自我檢查：npx tsx apns.test.ts
// 守一件事：JWT 的簽名是 APNs 要的 r||s 格式，拿公鑰驗得過。真的打 APNs 要有金鑰與實機，這裡驗不到。
import assert from "node:assert";
import { generateKeyPairSync, verify } from "node:crypto";
import { apnsJwt } from "./apns.js";

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
console.log("apns ok");
