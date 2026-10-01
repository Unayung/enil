// 登入：優先用 storage 裡的 refreshToken 換 token，換不到才掃 QR。
import { Client } from "@evex/linejs";
import { BaseClient } from "@evex/linejs/base";
import { FileStorage } from "@evex/linejs/storage";
import qrcode from "qrcode-terminal";
import { STORAGE_FILE, prepareStorage } from "./paths.js";
import nacl from "tweetnacl";
import crypto from "node:crypto";

prepareStorage();                   // 建目錄、把舊的 ./storage.json 搬過來
export const storage = new FileStorage(STORAGE_FILE);
const init = { device: "ANDROIDSECONDARY", storage } as const; // 次要裝置登入，手機維持主裝置

/**
 * QR 與 PIN 要交給呼叫端 —— daemon 沒有終端機可印，得把它們寫進 state 給 UI。
 * 不給就退回印在終端機（互動式使用）。
 */
export interface LoginHandlers {
  onQr?: (url: string) => void;
  onPin?: (pin: string) => void;
}

export interface LoginOptions {
  /**
   * 允許在快取憑證不可用時開 QR 流程。預設不允許 ——
   * QR 需要有人拿手機在旁邊，沒人時產生它只會留下一張沒人掃的憑證。
   */
  qrAllowed?: boolean;
}

/** 快取憑證不可用、且呼叫端沒允許 QR 時丟這個。 */
export class NeedsQrLogin extends Error {
  constructor() { super("需要 QR 登入（沒有可用的快取憑證）"); }
}

const terminalHandlers: Required<LoginHandlers> = {
  onQr(url) {
    console.log("\n用手機 LINE 掃下面的 QR（或直接開這個連結）：\n" + url + "\n");
    qrcode.generate(url, { small: true }); // 把登入 URL 畫成終端機 QR
  },
  onPin(pin) {
    console.log("\n>>> 到手機 LINE 輸入這組 PIN：  " + pin + "\n");
  },
};

// linejs 的 loginWithQR 從不看 storage，所以「快取 token」等於沒用。
// 這裡先自己拿 refreshToken 換 accessToken；換不到才退回掃 QR。
const AUTH_TOKEN = "authToken" as never; // linejs 的 KeyValues 沒這個 key，但 storage 就是個 JSON

// QR 登入：linejs 會從 response[10] 取 E2EE 金鑰信封並解出 keychain，但
// decryptKeyChain（e2ee/mod.ts:376）硬取清單第 0 筆。那份清單是歷史金鑰列表，
// 第 0 筆是最舊的，於是 storage 存到一把有效但早已不是現行的鑰匙 ——
// 私鑰配不上 LINE 登記的公鑰，所有 E2EE 訊息都解不開。這裡自己挑對的那把。
//
// 試過但無效（別再走一次）：把身分字串偽裝成官方 Chrome extension
// （CHROMEOS\t3.7.2\tChrome_OS\t1）會讓 createQrCodeForSecure 完全不回應，連 QR 都產不出來。
async function qrLogin(handlers: LoginHandlers): Promise<Client> {
  const base = new BaseClient(init);
  base.on("qrcall", handlers.onQr ?? terminalHandlers.onQr);
  base.on("pincall", handlers.onPin ?? terminalHandlers.onPin);

  // QR 網址裡的 ?secret= 是這裡產生的臨時金鑰對，keychain 就是加密給它的。
  // linejs 沒對外暴露私鑰，攔下來。
  let sqrSecret: Buffer | undefined;
  const e2eeMod = base.e2ee as any;
  const _createSqrSecret = e2eeMod.createSqrSecret.bind(e2eeMod);
  e2eeMod.createSqrSecret = (...a: any[]) => {
    const r = _createSqrSecret(...a);
    sqrSecret = Buffer.from(r[0]);
    return r;
  };

  const lp = base.loginProcess as any;
  const _qrCodeLogin = lp.qrCodeLoginV2ForSecure.bind(lp);
  let envelope: any;
  lp.qrCodeLoginV2ForSecure = async (...a: any[]) => {
    const res = await _qrCodeLogin(...a);
    envelope = res?.[10];
    return res;
  };

  await lp.withQrCode({});
  await lp.ready();
  const client = new Client(base);

  if (!envelope || !sqrSecret) {
    console.warn("⚠️  登入回應沒有 E2EE 金鑰信封，E2EE 訊息會解不開。");
    return client;
  }

  try {
    const shared = e2eeMod.generateSharedSecret(sqrSecret, Buffer.from(envelope.publicKey, "base64"));
    const aesKey = e2eeMod.getSHA256Sum(Buffer.from(shared), "Key");
    const aesIv = e2eeMod.xor(e2eeMod.getSHA256Sum(Buffer.from(shared), "IV"));
    const d = crypto.createDecipheriv("aes-256-cbc", aesKey, aesIv);
    d.setAutoPadding(false);
    const keychain = Buffer.concat([
      d.update(Buffer.from(envelope.encryptedKeyChain, "base64")),
      d.final(),
    ]);
    const list = (base.thrift as any).readThriftStruct(keychain)[1];

    // LINE 現在登記的公鑰就是配對目標：私鑰推導出的公鑰要跟它一樣。
    const registered = Buffer.from(
      (await base.talk.negotiateE2EEPublicKey({ mid: base.profile!.mid })).publicKey.keyData,
    ).toString("base64");
    console.log(`keychain 有 ${list.length} 把金鑰，LINE 登記的是 ${registered}`);

    let picked: { keyId: string; priv: Buffer; pub: string } | undefined;
    for (const [i, entry] of (list as any[]).entries()) {
      const priv = Buffer.from(entry[5]);
      const pub = Buffer.from(nacl.scalarMult.base(priv)).toString("base64");
      const keyId = String(entry[2] ?? envelope.keyId);
      console.log(`  [${i}] keyId=${keyId} pub=${pub}${pub === registered ? "  ← 就是這把" : ""}`);
      if (pub === registered) picked = { keyId, priv, pub };
    }

    if (!picked) {
      console.warn("⚠️  keychain 裡沒有一把配得上 LINE 登記的公鑰。");
    } else {
      const value = JSON.stringify({
        keyId: picked.keyId,
        privKey: picked.priv.toString("base64"),
        pubKey: picked.pub,
        e2eeVersion: envelope.e2eeVersion ?? "1",
      });
      // 收訊息查 e2eeKeys:<keyId>，送訊息查 e2eeKeys:<mid>，兩邊都要寫。
      await storage.set(`e2eeKeys:${picked.keyId}` as never, value as never);
      await storage.set(`e2eeKeys:${base.profile!.mid}` as never, value as never);
      console.log(`🔐 E2EE 金鑰已更新：keyId=${picked.keyId}`);
    }
  } catch (err) {
    console.warn("⚠️  keychain 解析失敗：", (err as Error)?.message ?? err);
  }
  return client;
}

export async function login(
  handlers: LoginHandlers = {},
  opts: LoginOptions = {},
): Promise<Client> {
  const qrAllowed = opts.qrAllowed ?? false;
  const needQr = () => {
    if (!qrAllowed) throw new NeedsQrLogin();
    return qrLogin(handlers);
  };
  // E2EE 金鑰只在 QR 登入時更新，所以要強迫走那條路時用：LINE_FORCE_QR=1
  if (process.env.LINE_FORCE_QR) return needQr();
  const refreshToken = await storage.get("refreshToken");
  if (typeof refreshToken !== "string") return needQr();

  const base = new BaseClient(init);
  // legy 對沒帶 x-line-access 的請求會回 403（getHeader() 只在有 authToken 時才加這個 header）。
  base.authToken = await storage.get(AUTH_TOKEN) as string | undefined;
  try {
    const res = await base.auth.refresh({ request: { refreshToken } });
    base.authToken = res.accessToken;
    // linejs 的 tryRefreshToken() 不存 rotate 後的新 refreshToken（JWT 是 rot:ROTATE），
    // 舊的用過即廢，這行漏掉下次就得重掃 QR。
    await storage.set("refreshToken", res.refreshToken);
    await storage.set(AUTH_TOKEN, res.accessToken as never);
    await base.loginProcess.ready();
    console.log("🔑 用快取 token 登入（免掃 QR）");
    return new Client(base);
  } catch (err) {
    console.warn("⚠️  refresh 失敗：", (err as Error)?.message ?? err);
    return needQr();
  }
}


// 掃 QR 那條路的 accessToken 也要存，否則下次 refresh 又會少 x-line-access header。
// linejs 換發 token 時會 emit 這個事件（含 response 的 x-line-next-access）。
export function keepAuthToken(client: Client) {
  client.base.on("update:authtoken", (token: string) => {
    storage.set(AUTH_TOKEN, token as never).catch(() => {});
  });
  return storage.set(AUTH_TOKEN, client.base.authToken as never);
}

