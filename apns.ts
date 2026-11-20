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
  /** VoIP 推播的 token，也是這支手機的識別。 */
  token: string;
  /** Live Activity 的「遠端啟動」token：app 沒開也能叫出一張卡片。 */
  start?: string;
  /** 目前那張 Live Activity 的 token，用來更新與結束。沒有卡片時是空的。 */
  activity?: string;
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

/** VIP 卡片上顯示的內容。欄位要跟 iOS 那邊的 VipAttributes.ContentState 一模一樣。 */
export interface VipCard { chat: string; name: string; text: string; count: number }

/**
 * Live Activity 推播的內容。start／update 帶 alert 才會亮螢幕、出聲、讓手錶震；
 * end 把 dismissal-date 設成現在，卡片馬上消失，不然系統會讓它在鎖定畫面多留幾個小時。
 */
export function activityPayload(event: "start" | "update" | "end", card: VipCard, alert: boolean, now = Date.now()): object {
  const ts = Math.floor(now / 1000);
  return {
    aps: {
      timestamp: ts,
      event,
      "content-state": card,
      // input-push-token：要系統替這張新卡片發一個 token 給 app，之後才更新得到、收得掉。
      ...(event === "start" ? { "attributes-type": "VipAttributes", attributes: {}, "input-push-token": 1 } : {}),
      ...(event === "end" ? { "dismissal-date": ts } : {}),
      ...(alert && event !== "end" ? { alert: { title: card.name, body: card.text, sound: "default" } } : {}),
    },
  };
}

export const voipPush = (cfg: ApnsConfig, device: PushDevice, payload: object) =>
  apnsPush(cfg, device.sandbox, device.token, "voip", payload);

// ponytail: 每次推播開一條新連線。VIP 訊息一天沒幾則；量大了再改成長連線。
export function apnsPush(
  cfg: ApnsConfig, sandbox: boolean, token: string, type: "voip" | "liveactivity", payload: object,
): Promise<{ status: number; body: string }> {
  if (!cached || cached.keyId !== cfg.keyId || Date.now() - cached.at > JWT_TTL_MS) {
    cached = { jwt: apnsJwt(cfg), at: Date.now(), keyId: cfg.keyId };
  }
  const jwt = cached.jwt;
  return new Promise((resolve, reject) => {
    const client = connect(`https://api.${sandbox ? "sandbox." : ""}push.apple.com`);
    client.on("error", reject);
    const req = client.request({
      ":method": "POST",
      ":path": `/3/device/${token}`,
      authorization: `bearer ${jwt}`,
      "apns-topic": `${cfg.bundleId}.${type === "voip" ? "voip" : "push-type.liveactivity"}`,
      "apns-push-type": type,
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
