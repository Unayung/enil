// 打 APNs 的 VoIP 推播：VIP 訊息靠它在手機上跳出全螢幕的來電畫面（iOS app 那邊用 CallKit 接）。
// 只用內建的 http2 與 crypto。
import { sign } from "node:crypto";
import { connect } from "node:http2";

export interface ApnsConfig {
  /** 開發者後台那把 .p8 金鑰的 Key ID */
  keyId: string;
  teamId: string;
  /** iOS app 的 bundle id；VoIP 推播的 topic 是它加 `.voip` */
  bundleId: string;
  /** .p8 的內容（PEM） */
  key: string;
}

export interface PushDevice {
  token: string;
  /** Xcode 直接裝的 build 拿到的是 sandbox 的 token，TestFlight 是正式的。由 app 自己回報。 */
  sandbox: boolean;
}

/** APNs 的 provider token：ES256 簽的 JWT。簽名要 r||s 原始格式，不是 DER。 */
export function apnsJwt(cfg: ApnsConfig, now = Date.now()): string {
  const b64 = (o: object) => Buffer.from(JSON.stringify(o)).toString("base64url");
  const head = `${b64({ alg: "ES256", kid: cfg.keyId })}.${b64({ iss: cfg.teamId, iat: Math.floor(now / 1000) })}`;
  const sig = sign("sha256", Buffer.from(head), { key: cfg.key, dsaEncoding: "ieee-p1363" });
  return `${head}.${sig.toString("base64url")}`;
}

// Apple 要求同一把 token 重複用（太常換會回 TooManyProviderTokenUpdates），最長一小時。
const JWT_TTL_MS = 30 * 60_000;
let cached: { jwt: string; at: number; keyId: string } | undefined;

// ponytail: 每次推播開一條新連線。VIP 訊息一天沒幾則；量大了再改成長連線。
export function voipPush(cfg: ApnsConfig, device: PushDevice, payload: object): Promise<{ status: number; body: string }> {
  if (!cached || cached.keyId !== cfg.keyId || Date.now() - cached.at > JWT_TTL_MS) {
    cached = { jwt: apnsJwt(cfg), at: Date.now(), keyId: cfg.keyId };
  }
  const jwt = cached.jwt;
  return new Promise((resolve, reject) => {
    const client = connect(`https://api.${device.sandbox ? "sandbox." : ""}push.apple.com`);
    client.on("error", reject);
    const req = client.request({
      ":method": "POST",
      ":path": `/3/device/${device.token}`,
      authorization: `bearer ${jwt}`,
      "apns-topic": `${cfg.bundleId}.voip`,
      "apns-push-type": "voip",
      "apns-priority": "10",
      "apns-expiration": "0", // 送不到就算了：過期的來電沒有意義
    });
    let status = 0;
    let body = "";
    req.on("response", (h) => { status = Number(h[":status"]); });
    req.on("data", (c) => { body += c; });
    req.on("end", () => { client.close(); resolve({ status, body }); });
    req.on("error", (e) => { client.close(); reject(e); });
    req.end(JSON.stringify(payload));
  });
}
