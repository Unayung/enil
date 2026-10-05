// 個人 LINE 帳號的程式化介面：純函式，不印任何東西。
// daemon 持有它，CLI 與 omarchy 外掛透過 daemon 的 socket 使用。
//
// ⚠️ 同一時間只能有一個 process 用這個模組。
// 登入會拿 refreshToken 換 token，而 LINE 的 refreshToken 是 rotate 的
// （JWT 標了 rot:ROTATE，回應帶回新的、舊的作廢）。兩個 process 同時啟動
// 會互相洗掉對方的憑證，最後退回掃 QR。要多個消費者就架一個 daemon 持有唯一 session。
import { createHash, createHmac, timingSafeEqual } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, extname, join } from "node:path";
import { keepAuthToken, login, type LoginHandlers, type LoginOptions } from "./login.js";
import { MEDIA_DIR } from "./paths.js";

export type ChatType = "USER" | "GROUP" | "ROOM" | "SQUARE";

export interface Chat {
  mid: string;
  type: ChatType;
  name: string;
  unread: number;
  lastTime: number;
  /** 大頭貼的本機路徑（一對一是對方的照片，群組是群組圖）。只有清單前面那些有 */
  avatar?: string;
}

export interface Msg {
  id: string;
  chat: string;
  from: string;
  fromName: string;
  text: string;
  time: number;
  contentType: string;
  /** 這則是 E2EE 且解密失敗（text 會是空字串） */
  decryptFailed: boolean;
  /** 有可下載的內容（圖片／影片／檔案） */
  hasMedia: boolean;
  fileName?: string;
  fileSize?: number;
  /** 影片／語音的長度（毫秒）。影片的 metadata 是 DURATION，語音以前叫 AUDLEN，兩個都看 */
  duration?: number;
  /** 圖片縮圖或影片預覽圖的本機路徑（history() 會順手抓好，抓不到就是 undefined） */
  mediaPath?: string;
  /** FLEX / RICH 之類版面訊息的純文字備援（LINE 自己附的 ALT_TEXT） */
  altText?: string;
  /** FLEX 版面裡的圖片網址（carousel 的每個 bubble 一張），公開 CDN，不需認證 */
  flexImages?: string[];
  /** 貼圖的參數（貼圖訊息才有）。送一張一樣的貼圖要這三個。 */
  sticker?: { id: string; pkg: string; ver: string };
  /** FLEX 版面裡的文字，一個元件一行。altText 常常只是「您有新訊息」，內容在這裡 */
  flexText?: string;
  /** FLEX 版面裡的連結（uri action） */
  flexLinks?: string[];
  /**
   * 貼圖、或 FLEX 第一張圖的本機路徑（history() 順手抓的，沒有副檔名）。
   * 不併進 mediaPath：那個欄位的意思是「這則有原檔可以 download」，貼圖與 FLEX 沒有。
   */
  thumbPath?: string;
  /** 發話者大頭貼的本機路徑（history() 順手抓的，沒有副檔名；對方沒設照片就沒有） */
  fromAvatar?: string;
  /** 自己發的訊息有幾個人讀了（history 才有；一對一時 1 就是「已讀」） */
  readCount?: number;
  /** 表情回應：誰、哪一種（NICE／LOVE／FUN／AMAZING／SAD／OMG） */
  reactions?: { from: string; type: string }[];
  /** 這則 @ 了誰（mid）；"all" 是 @所有人 */
  mentions?: string[];
  /** 聊天室名稱。只有 OpenChat 訊息會帶 —— square 的名字事後查不到 */
  chatName?: string;
}

const FLEX_IMAGE_MAX = 4;

/** 表情回應的種類。LINE 用編號，linejs 有時給編號有時給名稱，一律轉成名稱。 */
export const REACTIONS = ["ALL", "UNDO", "NICE", "LOVE", "FUN", "AMAZING", "SAD", "OMG"] as const;
export function reactionName(v: unknown): string {
  // 沒有 predefinedReactionType 的是 LINE 的自訂／付費回應，內容我們不認得。
  if (v === undefined || v === null || v === "") return "OTHER";
  return typeof v === "number" ? REACTIONS[v] ?? String(v) : String(v);
}

/** contentMetadata.MENTION（{"MENTIONEES":[{"S":"0","E":"5","M":"<mid>"}]}）裡 @ 了誰。 */
export function mentionsOf(json: string | undefined): string[] | undefined {
  if (!json) return undefined;
  try {
    const list = JSON.parse(json)?.MENTIONEES;
    if (!Array.isArray(list)) return undefined;
    const mids = list.map((x: any) => (x?.A ? "all" : String(x?.M ?? ""))).filter(Boolean);
    return mids.length ? mids : undefined;
  } catch {
    return undefined;
  }
}

/**
 * 送出時的 MENTION metadata。spans 的 start／end 是 JS 字串索引（UTF-16），linejs 解析時
 * 也是這樣算（substring）。
 */
export function mentionMeta(spans: { mid: string; start: number; end: number }[]): string {
  return JSON.stringify({ MENTIONEES: spans.map((x) => ({ S: String(x.start), E: String(x.end), M: x.mid })) });
}

/** 推送來的「不是新訊息」的事件。 */
export type ChatEvent =
  | { kind: "read"; chat: string; reader: string; messageId: string }
  | { kind: "chat-read"; chat: string }
  | { kind: "unsend"; chat: string; messageId: string }
  | { kind: "reaction"; chat: string; messageId: string; from: string; type: string }
  | { kind: "edit"; msg: Msg };

/** fetchMyEvents 一輪最多翻幾頁。保險絲：伺服器要是一直回 continuationToken 不能無限拉。 */
const SQUARE_MAX_PAGES = 20;
/** 記住多少個已交出去的 OpenChat 訊息 id，用來擋重複。 */
const SQUARE_SEEN_MAX = 5000;

const FLEX_LINES_MAX = 40;
const FLEX_LINKS_MAX = 8;

/**
 * 走訪 FLEX JSON 收集圖片網址、文字與連結。不做完整版面 —— 照出現順序攤平成
 * 一行一個元件，終端機與 agent 讀得懂就好。
 */
export function flexOf(json: string): { images: string[]; text: string; links: string[] } {
  const images: string[] = [];
  const lines: string[] = [];
  const links: string[] = [];
  const line = (s: unknown) => {
    const t = typeof s === "string" ? s.trim() : "";
    if (t && lines.length < FLEX_LINES_MAX) lines.push(t);
  };
  const walk = (n: any) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!n || typeof n !== "object") return;
    if (n.type === "image" && typeof n.url === "string" && images.length < FLEX_IMAGE_MAX) images.push(n.url);
    // 有 span 的 text 元件，字在 contents 裡，text 欄位會被忽略（LINE 的規格）。
    if (n.type === "text") {
      line(Array.isArray(n.contents) && n.contents.length
        ? n.contents.map((s: any) => s?.text ?? "").join("")
        : n.text);
    }
    if (n.type === "button") line(n.action?.label && `[${n.action.label}]`);
    if (n.type === "uri" && typeof n.uri === "string"
        && links.length < FLEX_LINKS_MAX && !links.includes(n.uri)) links.push(n.uri);
    for (const v of Object.values(n)) walk(v);
  };
  try {
    walk(JSON.parse(json));
  } catch { /* 壞掉的 JSON 就當沒有內容 */ }
  return { images, text: lines.join("\n"), links };
}

/** chats() 幫前幾個聊天室準備大頭貼。 */
const AVATAR_CHATS = 60;

function stickerUrl(id: string): string {
  return `https://stickershop.line-scdn.net/stickershop/v1/sticker/${id}/android/sticker.png`;
}

/** 訊息 metadata 裡的貼圖參數。三個都要是數字才算，缺一個就送不出一樣的貼圖。 */
export function stickerOf(meta: any): { id: string; pkg: string; ver: string } | undefined {
  const [id, pkg, ver] = [meta?.STKID, meta?.STKPKGID, meta?.STKVER].map((v) => String(v ?? ""));
  return [id, pkg, ver].every((v) => /^\d+$/.test(v)) ? { id, pkg, ver } : undefined;
}

/** 好友清單快取多久。 */
const CONTACTS_TTL_MS = 10 * 60_000;
/** getContacts 一次查幾個人。 */
const CONTACTS_BATCH = 100;

const MAC_CHUNK = 131072;

/** E2EE 影片的驗證碼：密文每 131072 bytes 一塊各算 SHA-256，串起來再算 HMAC-SHA256。 */
export function chunkedMac(enc: Buffer, macKey: Buffer): Buffer {
  const hmac = createHmac("sha256", macKey);
  for (let o = 0; o < enc.length; o += MAC_CHUNK) {
    hmac.update(createHash("sha256").update(enc.subarray(o, o + MAC_CHUNK)).digest());
  }
  return hmac.digest();
}

/**
 * linejs 的 decryptByKeyMaterial 只認「整份密文算一次 HMAC」，圖片與檔案是這樣簽的，
 * 影片不是（見 chunkedMac）—— 所以 E2EE 影片一律報 "HMAC verification failed"，
 * 其實金鑰與解密都是對的。2026-10-02 拿真實影片的密文離線比對出來的，3.4.2 仍未修。
 *
 * 原本的驗不過才用分塊那套再驗一次。兩套用的是同一把 macKey，沒有放寬驗證：
 * 兩個都不符就照原樣丟錯。
 */
function acceptChunkedMac(e2ee: any) {
  const orig = e2ee.decryptByKeyMaterial.bind(e2ee);
  e2ee.decryptByKeyMaterial = async (rawData: Buffer, keyMaterial: Buffer | string) => {
    try {
      return await orig(rawData, keyMaterial);
    } catch (err) {
      if (!String((err as Error)?.message).includes("HMAC verification failed")) throw err;
      const keys = await e2ee.deriveKeyMaterial(
        typeof keyMaterial === "string" ? Buffer.from(keyMaterial, "base64") : keyMaterial);
      const enc = rawData.subarray(0, -32);
      if (!timingSafeEqual(chunkedMac(enc, keys.macKey), rawData.subarray(-32))) throw err;
      return await e2ee.___decryptAESCTR(keys.encKey, keys.nonce, enc);
    }
  };
}

/** 副檔名只收英數。檔名是對方給的，原樣拼進路徑的話 "a./../../x" 會寫到 media 目錄外面。 */
export function safeExt(name: string | undefined): string | undefined {
  return name?.match(/\.([A-Za-z0-9]{1,10})$/)?.[1];
}

/** 公開圖片（貼圖、FLEX）單張上限。超過就不存，UI 退回文字。 */
const THUMB_MAX_BYTES = 5 * 1024 * 1024;

// 一次開啟最多預抓幾張縮圖。抓過會快取，所以這只影響第一次進入某聊天室。
const PREFETCH_IMAGES = 12;

const MEDIA_TYPES = new Set(["IMAGE", "VIDEO", "AUDIO", "FILE"]);
const EXT: Record<string, string> = { IMAGE: "jpg", VIDEO: "mp4", AUDIO: "m4a" };
const RAW_CACHE_MAX = 1000;

// obs 用 blob.type 反查副檔名，所以 mime 要給對。常見的列舉，其餘當一般檔案。
const MIME: Record<string, string> = {
  ".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png",
  ".gif": "image/gif", ".webp": "image/webp", ".bmp": "image/bmp",
  ".mp4": "video/mp4", ".mov": "video/quicktime", ".webm": "video/webm",
  ".mp3": "audio/mpeg", ".m4a": "audio/mp4", ".wav": "audio/x-wav",
  ".pdf": "application/pdf", ".txt": "text/plain", ".zip": "application/zip",
};

function objTypeOf(mime: string): "image" | "gif" | "video" | "audio" | "file" {
  if (mime === "image/gif") return "gif";
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "audio";
  return "file";
}

// 抓到 hasNext 為止。上限只是防呆 —— 聊天室是照 chat id 分頁不是照時間，
// 少抓一頁就可能漏掉最近有未讀的聊天室（實測：只抓 4 頁會少算 6 則未讀）。
const MAX_BOX_PAGES = 20;   // 每頁 50 個 → 上限 1000 個聊天室

export class Line {
  #client: any;
  #base: any;
  #names = new Map<string, string>();
  /** mid → 大頭貼在 CDN 上的路徑（picturePath）。跟名字同一趟 getContacts 拿到的。 */
  #pictures = new Map<string, string>();
  #anchors = new Map<string, any>();
  // 下載要用 raw 的 chunks / contentMetadata，轉成 Msg 之後就沒了，所以留一份。
  #raw = new Map<string, any>();
  me!: { mid: string; displayName: string };

  static async connect(handlers: LoginHandlers = {}, opts: LoginOptions = {}): Promise<Line> {
    const self = new Line();
    self.#client = await login(handlers, opts);
    await keepAuthToken(self.#client);
    self.#base = self.#client.base;
    acceptChunkedMac(self.#base.e2ee);
    const p = await self.#client.getMyProfile();
    self.me = { mid: p.mid, displayName: p.displayName };
    self.#names.set(p.mid, p.displayName);
    if (p.picturePath) self.#pictures.set(p.mid, p.picturePath);
    return self;
  }

  /** 聊天室清單，最近有動靜的在前。 */
  async chats(limit = 30): Promise<Chat[]> {
    const boxes: any[] = [];
    let cursor: string | undefined;
    let truncated = true;
    for (let page = 0; page < MAX_BOX_PAGES; page++) {
      // minChatId/maxChatId 給空字串會被當成真的範圍過濾、回 0 筆 —— 要整個不給。
      const r = await this.#base.talk.getMessageBoxes({
        messageBoxListRequest: {
          ...(cursor ? { minChatId: cursor } : {}),
          activeOnly: false,
          messageBoxCountLimit: 50,
          withUnreadCount: true,
          lastMessagesPerMessageBoxCount: 1,
        },
        syncReason: "INTERNAL",
      });
      const page_: any[] = r?.messageBoxes ?? [];
      if (!page_.length) { truncated = false; break; }
      boxes.push(...page_);
      if (!r.hasNext) { truncated = false; break; }
      cursor = page_[page_.length - 1].id;
    }
    // 靜默截斷會讓未讀數看起來就是「比手機少」，而且沒有任何線索。
    if (truncated) {
      console.warn(`⚠️  聊天室超過 ${MAX_BOX_PAGES * 50} 個，未讀統計可能不完整`);
    }

    // 判準只能用 lastDeliveredMessageId：lastMessages 是同步用的暫態欄位，
    // 伺服器只在「這次同步有新東西」時才帶，拿它當「聊天室是不是空的」會濾掉絕大多數。
    // 少數聊天室有錨點卻讀不到歷史（已刪光/隱藏），那由呼叫端顯示「沒有歷史訊息」。
    // 有未讀的一律保留，即使沒有錨點 —— 少算未讀比多一個讀不到歷史的聊天室糟得多。
    const live = boxes.filter((b) =>
      b.lastDeliveredMessageId?.messageId || Number(b.unreadCount ?? 0) > 0
    );
    for (const b of live) this.#anchors.set(b.id, b.lastDeliveredMessageId);

    // 伺服器照 chat id 排，不是照時間。
    live.sort((a, b) =>
      Number(b.lastDeliveredMessageId?.deliveredTime ?? 0) -
      Number(a.lastDeliveredMessageId?.deliveredTime ?? 0)
    );
    const shown = live.slice(0, limit);
    await this.#resolveNames(shown.map((b) => b.id), shown.map((b) => String(b.midType)));

    const out: Chat[] = shown.map((b) => ({
      mid: b.id,
      type: String(b.midType) as ChatType,
      name: this.#names.get(b.id) ?? b.id.slice(0, 10),
      unread: Number(b.unreadCount ?? 0),
      lastTime: Number(b.lastDeliveredMessageId?.deliveredTime ?? 0),
    }));
    // 大頭貼只給前面這些 —— 清單畫面用得到的就這麼多。搜尋會一次要幾百個聊天室，
    // 全抓等於對 CDN 連發幾百個請求。抓過的有快取，每分鐘的 resync 不會重抓。
    await Promise.all(out.slice(0, AVATAR_CHATS).map(async (c) => {
      c.avatar = await this.#avatarOf(c.mid);
    }));
    return out;
  }

  /**
   * 某聊天室的訊息，舊到新。
   * beforeId 給定時，取「那則之前」的 —— 往前翻頁用。
   */
  async history(chatMid: string, count = 20, beforeId?: string): Promise<Msg[]> {
    let anchor: any;
    if (beforeId) {
      const raw = this.#raw.get(beforeId);
      // 錨點要的是 {deliveredTime, messageId}，deliveredTime 只有 raw 有。
      if (raw) {
        anchor = {
          deliveredTime: Number(raw.deliveredTime ?? raw.createdTime ?? 0),
          messageId: BigInt(String(raw.id)),
        };
      }
    }
    if (!anchor) {
      if (!this.#anchors.has(chatMid)) await this.chats(Number.MAX_SAFE_INTEGER);
      anchor = this.#anchors.get(chatMid);
    }
    if (!anchor) return [];
    // 錨點必須是真的 lastDeliveredMessageId，隨便給 0 或 MAX_SAFE 都回 0 則。
    const raw: any[] = await this.#base.talk.getPreviousMessagesV2WithRequest({
      request: {
        messageBoxId: chatMid,
        endMessageId: anchor,
        messagesCount: count,
        withReadCount: true,
        receivedOnly: false,
        lastOpRevision: 0,
      },
      syncReason: "INTERNAL",
    });
    await this.#resolveNames(raw.map((m) => m.from), raw.map(() => "USER"));
    const out = await Promise.all(raw.map((m) => this.#toMsg(m)));

    // 圖片縮圖在這裡就抓好，直接把路徑放進 Msg。
    // 讓 UI 自己非同步要圖再對帳回來，是多一層沒必要的狀態機。
    // 貼圖與 FLEX 的第一張圖也一起抓，放在 thumbPath。
    const images = out
      .filter((m) => m.contentType === "IMAGE" || m.contentType === "VIDEO"
        || m.contentType === "STICKER" || m.flexImages?.length)
      .slice(0, PREFETCH_IMAGES);
    await Promise.all(images.map(async (m) => {
      try {
        // 影片拿到的是預覽圖，不是影片本身。
        if (m.contentType === "IMAGE" || m.contentType === "VIDEO") {
          m.mediaPath = await this.download(m.id, true);
        } else if (m.contentType === "STICKER") {
          const stk = String(this.#raw.get(m.id)?.contentMetadata?.STKID ?? "");
          if (/^\d+$/.test(stk)) {
            // 檔名用貼圖 id：同一張貼圖在很多訊息裡出現，抓一次就好（貼圖選單也用這份）。
            m.thumbPath = await this.#cachePublic(`sticker-${stk}.thumb`, stickerUrl(stk));
          }
        } else {
          m.thumbPath = await this.#cachePublic(`${m.id}.thumb`, m.flexImages![0]);
        }
      } catch { /* 抓不到就顯示成一般附件 */ }
    }));

    // 大頭貼：這一頁出現過的發話者各抓一次。
    const avatars = new Map<string, string | undefined>();
    await Promise.all([...new Set(out.map((m) => m.from))].map(async (mid) => {
      avatars.set(mid, await this.#avatarOf(mid));
    }));
    for (const m of out) m.fromAvatar = avatars.get(m.from);

    return out.reverse(); // 回傳是新到舊
  }

  /**
   * 標成已讀。lastMessageId 省略時用聊天室錨點（lastDeliveredMessageId）——
   * 有些聊天室讀不到歷史卻仍有未讀，不能讓「清未讀」依賴「拿得到訊息」。
   */
  async markRead(chatMid: string, lastMessageId?: string): Promise<void> {
    let id = lastMessageId;
    if (!id) {
      if (!this.#anchors.has(chatMid)) await this.chats(Number.MAX_SAFE_INTEGER);
      const anchor = this.#anchors.get(chatMid);
      if (!anchor?.messageId) return;
      id = String(anchor.messageId);
    }
    await this.#base.talk.sendChatChecked({
      seq: await this.#base.getReqseq("talk"),
      chatMid,
      lastMessageId: id,
      sessionId: 0,
    });
  }

  /** mentions 有給就帶 MENTION metadata（compact 訊息帶不了 metadata，要走一般的 sendMessage）。 */
  async send(chatMid: string, text: string, mentions?: { mid: string; start: number; end: number }[]): Promise<void> {
    if (!mentions?.length) {
      await this.#client.sendCompactMessage(chatMid, text);
      return;
    }
    const meta = { MENTION: mentionMeta(mentions) };
    try {
      await this.#base.talk.sendMessage({ to: chatMid, text, contentMetadata: meta });
    } catch {
      // E2EE 聊天室：明文送會被拒，改加密。metadata 留在外面（linejs 只加密 text）。
      await this.#base.talk.sendMessage({ to: chatMid, text, contentMetadata: meta, e2ee: true });
    }
  }

  /**
   * 送貼圖。Letter Sealing（E2EE）不加密貼圖，一般聊天室與 E2EE 聊天室都是明文送 ——
   * 貼圖訊息本來就沒有文字可以加密，內容就是這三個參數。
   */
  async sendSticker(chatMid: string, s: { id: string; pkg: string; ver: string }): Promise<void> {
    if (![s.id, s.pkg, s.ver].every((v) => /^\d+$/.test(v))) throw new Error("貼圖參數要是數字");
    await this.#base.talk.sendMessage({
      to: chatMid,
      contentType: "STICKER" as any,
      contentMetadata: { STKID: s.id, STKPKGID: s.pkg, STKVER: s.ver },
    });
  }

  /** 某張貼圖的圖檔（本機路徑），給貼圖選單畫縮圖。跟訊息裡的貼圖走同一個 CDN、同一份快取。 */
  async stickerImage(id: string): Promise<string> {
    if (!/^\d+$/.test(id)) throw new Error("貼圖 id 要是數字");
    return await this.#cachePublic(`sticker-${id}.thumb`, stickerUrl(id));
  }

  /** 收回自己的訊息。 */
  async unsend(messageId: string): Promise<void> {
    await this.#base.talk.unsendMessage({ seq: await this.#base.getReqseq("talk"), messageId });
  }

  /** 表情回應。type 是 REACTIONS 裡的名稱；UNDO 是收回自己的回應。 */
  async react(messageId: string, type: string): Promise<void> {
    if (!REACTIONS.includes(type as any) || type === "ALL") throw new Error(`不認得的表情：${type}`);
    await this.#base.talk.react({ id: BigInt(messageId), reaction: type as any });
  }

  #contacts?: { at: number; list: { mid: string; name: string }[] };

  /**
   * 好友清單：mid 與顯示名稱。給「跟還沒聊過的好友開對話」用 —— chats() 只列有訊息的聊天室。
   * 好友不常變，快取 10 分鐘；每次打開搜尋都拉幾百人沒有必要。
   */
  async contacts(): Promise<{ mid: string; name: string }[]> {
    if (this.#contacts && Date.now() - this.#contacts.at < CONTACTS_TTL_MS) return this.#contacts.list;
    const ids: unknown = await this.#base.talk.getAllContactIds({ syncReason: "INTERNAL" } as any);
    const mids = (Array.isArray(ids) ? ids : []).map(String).filter((m) => m.startsWith("u") && m !== this.me.mid);
    // getContacts 一次要太多會被拒；分批。
    for (let i = 0; i < mids.length; i += CONTACTS_BATCH) {
      const batch = mids.slice(i, i + CONTACTS_BATCH);
      await this.#resolveNames(batch, batch.map(() => "USER"));
    }
    const list = mids.map((mid) => ({ mid, name: this.#names.get(mid) ?? mid.slice(0, 8) }));
    this.#contacts = { at: Date.now(), list };
    return list;
  }

  /** 群組成員（打 @ 時挑人用）：mid 與顯示名稱，不含自己。 */
  async members(chatMid: string): Promise<{ mid: string; name: string }[]> {
    const { chats } = await this.#base.talk.getChats({ chatMids: [chatMid], withMembers: true, withInvitees: false });
    const mids = Object.keys(chats?.[0]?.extra?.groupExtra?.memberMids ?? {}).filter((m) => m !== this.me.mid);
    await this.#resolveNames(mids, mids.map(() => "USER"));
    return mids.map((mid) => ({ mid, name: this.#names.get(mid) ?? mid.slice(0, 8) }));
  }

  /** 某人的大頭貼（本機路徑）。推送來的訊息不帶 fromAvatar，要的人自己問（VIP 對話框）。 */
  avatar(mid: string): Promise<string | undefined> {
    return this.#avatarOf(mid);
  }

  /** 每一個推送來的 Operation 的類型（字串；linejs 不認得的是數字）。觀察用。 */
  onAnyOp(cb: (type: string, op: any) => void): void {
    this.#client.on("event", (op: any) => {
      try {
        cb(String(op?.type ?? "?"), op);
      } catch { /* 觀察用的，不能影響其他處理 */ }
    });
  }

  /**
   * 「不是新訊息」的推送：已讀、收回、表情、編輯。linejs 只把新訊息與編輯包成事件，
   * 其餘要從原始 Operation 自己認（欄位意思照 linejs 的 example/_event/talk-class.ts）。
   */
  onEvents(cb: (ev: ChatEvent) => void): void {
    this.#client.on("event", (op: any) => {
      try {
        const [p1, p2, p3] = [String(op.param1 ?? ""), String(op.param2 ?? ""), String(op.param3 ?? "")];
        switch (op.type) {
          case "NOTIFIED_READ_MESSAGE": // 別人讀了：聊天室、誰、讀到哪則
            return cb({ kind: "read", chat: p1, reader: p2, messageId: p3 });
          case "SEND_CHAT_CHECKED": // 我在別的裝置（手機）讀了
            return cb({ kind: "chat-read", chat: p1 });
          case "DESTROY_MESSAGE": // 我在手機上收回
          case "NOTIFIED_DESTROY_MESSAGE": // 別人收回
            return cb({ kind: "unsend", chat: p1, messageId: p2 });
          case "SEND_REACTION": // 我在別的裝置回應
          case "NOTIFIED_SEND_REACTION": { // 別人回應：訊息、{chatMid, curr}、誰
            const info = JSON.parse(p2 || "{}");
            const from = op.type === "SEND_REACTION" ? this.me.mid : p3;
            const type = reactionName(info?.curr?.predefinedReactionType ?? "UNDO");
            return cb({ kind: "reaction", chat: String(info?.chatMid ?? ""), messageId: p1, from, type });
          }
        }
      } catch (err) {
        console.warn("處理事件失敗：", op?.type, (err as Error)?.message ?? err);
      }
    });
    this.#client.on("message:edit", async (m: any) => {
      try {
        cb({ kind: "edit", msg: await this.#toMsg(m.raw ?? m) });
      } catch (err) {
        console.warn("處理編輯失敗：", (err as Error)?.message ?? err);
      }
    });
  }

  /**
   * 送出本機檔案。上傳即送出 —— 兩條路徑都會自己建訊息。
   * E2EE 聊天室要走另一條，但沒有可靠的事前判斷，所以照 linejs 送文字的作法：
   * 先試一般的，失敗才改 E2EE 重來。
   */
  async sendMedia(chatMid: string, filePath: string): Promise<{ oType: string; e2ee: boolean }> {
    if (!existsSync(filePath)) throw new Error(`檔案不存在：${filePath}`);
    const name = basename(filePath);
    const mime = MIME[extname(filePath).toLowerCase()] ?? "application/octet-stream";
    const oType = objTypeOf(mime);
    const blob = new Blob([readFileSync(filePath)], { type: mime });
    try {
      await this.#base.obs.uploadObjTalk(chatMid, oType, blob, undefined, name);
      return { oType, e2ee: false };
    } catch (err) {
      await this.#base.obs.uploadMediaByE2EE({ data: blob, oType, to: chatMid, filename: name });
      return { oType, e2ee: true };
    }
  }

  /**
   * 錨點往前推。getPreviousMessages 取的是「錨點之前」，而錨點原本只在
   * chats() 時更新（啟動 + 每 60 秒 resync）。剛送出或剛收到的訊息會落在
   * 窗外，症狀就是「我剛剛傳的那則不見了」。
   */
  #noteAnchor(raw: any) {
    const chat = chatOf(raw, this.me.mid);
    const id = String(raw?.id ?? "");
    if (!chat || !id) return;
    const time = Number(raw.deliveredTime ?? raw.createdTime ?? 0);
    const current = this.#anchors.get(chat);
    if (current && Number(current.deliveredTime ?? 0) > time) return; // 舊訊息不要倒退
    this.#anchors.set(chat, { deliveredTime: time, messageId: BigInt(id) });
  }

  /**
   * 開始收訊息。listen() 內部是 LEGY push 的長連線，呼叫端不要擋住 event loop。
   *
   * talk 與 square（OpenChat）兩條都收，交出去的都是同一個 Msg —— daemon 不必
   * 分辨來源，存檔與預覽走同一條路。
   */
  onMessage(cb: (m: Msg) => void): void {
    // linejs 3.4.2 起不替非同步的處理函式接例外 —— 一則訊息出錯不能讓之後的都收不到。
    this.#client.on("message", async (m: any) => {
      try {
        const raw = m.raw ?? m;
        this.#noteAnchor(raw);
        await this.#resolveNames([raw.from], ["USER"]);
        cb(await this.#toMsg(raw));
      } catch (err) {
        console.warn("處理收到的訊息失敗：", (err as Error)?.message ?? err);
      }
    });
    // OpenChat 不走這裡 —— 見 squareMessages()。
    this.#client.listen({ talk: true });
  }

  /**
   * 連線出問題時通知呼叫端。兩種情況 linejs 自己救不回來：
   *   - kicked：伺服器說這台裝置不再被授權（在別台登入、在手機上登出）。linejs 會刪掉
   *     authToken、停掉推送並發出 "end"。
   *   - push-dead：推送初始化失敗，迴圈結束（3.4.2 記成 LegyPusherError_cannot_init）。
   *     一般的連線中斷 linejs 會等 4 秒自己重連，記成 LegyPusherError，那種不用管。
   * 「連線還在但不再送東西」的卡死 linejs 看不出來，那個靠 daemon 對帳（見 daemon.ts catchUp）。
   */
  onTrouble(cb: (kind: "kicked" | "push-dead", detail: string) => void): void {
    this.#base.on("end", () => cb("kicked", "NOT_AUTHORIZED_DEVICE"));
    this.#base.on("log", ({ type, data }: { type: string; data: any }) => {
      if (type === "LegyPusherError_cannot_init") cb("push-dead", String(data?.error?.message ?? data?.error ?? ""));
      else if (type.endsWith("sync error:")) console.warn("推送同步出錯（之後可能不再收到訊息）：", data);
    });
  }

  /** fetchMyEvents 的增量游標。空字串代表「從頭」，第一次拉會帶回積壓的事件。 */
  #squareSync = "";

  /**
   * 拉一輪 OpenChat 的新訊息。呼叫端自己定時跑。
   *
   * 為什麼是拉不是推：square 的推送只送一個 subscriptionId 當「你有新事件」的
   * 鈴聲，客戶端收到才去 fetchMyEvents 取內容 —— 而那個訂閱會過期。linejs 的
   * _OnPingCallback 算得出哪些訂閱過期了，卻只寫進 log，從來沒呼叫
   * square.refreshSubscriptions()（connManager.ts:469，log 開頭那個 `?` 大概是
   * 作者自己也知道沒寫完）。實測 daemon 跑過一小時後 OpenChat 訊息就再也不進來。
   *
   * 與其去補別人的訂閱維護，不如自己拉：fetchMyEvents 帶 syncToken 是增量的，
   * 一次呼叫就好。代價是延遲變成輪詢間隔，對「偵測有人在 OpenChat 說了什麼」
   * 這個用途夠了。
   */
  async squareMessages(): Promise<Msg[]> {
    // OpenChat 的訊息**不能**從 fetchMyEvents 拿。它回的是 NOTIFICATION_MESSAGE ——
    // 推播通知，快速連發會合併成最後一則。實測「照片 → 想找這樣的店面 → 松山區」
    // 三則在 5 秒內發出，通知只給了「松山區」，照片與中間那句都沒有；拿同一個聊天室
    // 的 fetchSquareChatEvents 一看，三則都在。
    //
    // 所以通知流只做兩件事：撿聊天室名字、撿發話者名字（聊天室事件流裡沒有
    // senderDisplayName，只有通知才有）。訊息本身一律從各聊天室的事件流拉。
    let sync = this.#squareSync;
    let cont: string | undefined;
    for (let page = 0; page < SQUARE_MAX_PAGES; page++) {
      const res: any = await this.#base.square.fetchMyEvents({
        syncToken: sync, continuationToken: cont, limit: 100,
      });
      for (const ev of res?.events ?? []) {
        this.#harvestSquareChat(ev);
        const note = ev?.payload?.notificationMessage;
        const m = note?.squareMessage?.message;
        if (note?.squareChatMid && !this.#squareChats.has(String(note.squareChatMid))) {
          // 新加入的聊天室：先記下來讓下面去拉，名字等結構事件補
          this.#squareChats.set(String(note.squareChatMid), "");
        }
        if (m?.from && note?.senderDisplayName) {
          this.#squareMemberNames.set(String(m.from), String(note.senderDisplayName));
        }
      }
      sync = res?.syncToken ?? sync;
      cont = res?.continuationToken || undefined;
      if (!cont) break;
    }
    this.#squareSync = sync;

    const out: Msg[] = [];
    for (const chatMid of this.#squareChats.keys()) {
      try {
        out.push(...await this.#squareChatNew(chatMid));
      } catch (err) {
        console.warn(`OpenChat 聊天室拉取失敗（${chatMid.slice(0, 10)}）：`,
          (err as Error)?.message ?? err);
      }
    }
    return out.sort((a, b) => a.time - b.time);
  }

  /** 各聊天室自己的事件流游標。 */
  #chatSync = new Map<string, string>();
  /** 只收 daemon 開始追之後的訊息，見 #squareChatNew()。 */
  #squareSince = Date.now();
  /** 已經交出去的訊息 id。上限見 SQUARE_SEEN_MAX。 */
  #squareSeen = new Set<string>();
  /** squareMember mid → 顯示名稱，從通知的 senderDisplayName 撿來。 */
  #squareMemberNames = new Map<string, string>();

  /**
   * 一個聊天室自上次以來的新訊息。
   *
   * 第一次拉某個聊天室時沒有游標，伺服器會從歷史開頭給 —— 那些不能交出去，否則
   * 會被重新寫進 archive、watcher 當成新訊息重複通報（實際發生過，一次洗了 11 則）。
   * 所以只收 #squareSince 之後的；另外用 #squareSeen 再擋一次，重複通報的代價
   * 太高，多一道保險值得。
   */
  async #squareChatNew(chatMid: string): Promise<Msg[]> {
    let sync = this.#chatSync.get(chatMid);
    const events: any[] = [];
    for (let page = 0; page < SQUARE_MAX_PAGES; page++) {
      const res: any = await this.#base.square.fetchSquareChatEvents({
        squareChatMid: chatMid, syncToken: sync, limit: 100,
      });
      const got: any[] = res?.events ?? [];
      events.push(...got);
      sync = res?.syncToken ?? sync;
      if (!got.length) break;
    }
    if (sync) this.#chatSync.set(chatMid, sync);

    const out: Msg[] = [];
    for (const ev of events) {
      try {
        // 自己發的是 SEND_MESSAGE、別人發的是 RECEIVE_MESSAGE
        const raw = (ev?.payload?.sendMessage ?? ev?.payload?.receiveMessage)?.squareMessage?.message;
        if (!raw) continue;
        const id = String(raw.id ?? "");
        const time = Number(raw.createdTime ?? 0);
        if (!id || time < this.#squareSince || this.#squareSeen.has(id)) continue;

        const msg = await this.#toMsg(raw);
        msg.chat = chatMid;
        msg.chatName = this.#squareChats.get(chatMid) || undefined;
        const name = this.#squareMemberNames.get(String(raw.from ?? ""));
        if (name) msg.fromName = name;
        // 圖片當下就抓：daemon 一重啟 raw 快取就空了，那則的媒體就再也下載不到。
        // 原圖不是縮圖 —— 要從招牌、門牌、路名看出物件位置，縮圖的解析度不夠。
        if (msg.contentType === "IMAGE") {
          try {
            msg.mediaPath = await this.download(msg.id, false);
          } catch (err) {
            console.warn("OpenChat 圖片下載失敗：", (err as Error)?.message ?? err);
          }
        }
        this.#squareSeen.add(id);
        out.push(msg);
      } catch (err) {
        console.warn(`OpenChat 事件處理失敗，跳過這則（type=${ev?.type}）：`,
          (err as Error)?.message ?? err);
      }
    }
    if (this.#squareSeen.size > SQUARE_SEEN_MAX) {
      // Set 依插入序，砍最舊的一半就好
      const drop = [...this.#squareSeen].slice(0, this.#squareSeen.size / 2);
      for (const d of drop) this.#squareSeen.delete(d);
    }
    return out;
  }

  /**
   * 加入的 OpenChat。square 是另一套 API —— 這些聊天室不會出現在 chats() 裡，
   * 因為 getMessageBoxes 只認 talk。回的是 square（s 開頭）不是 squareChat（m 開頭）。
   */
  async squares(): Promise<Chat[]> {
    const res: any = await this.#base.square.getJoinedSquares({ request: { limit: 100 } });
    return (res?.squares ?? []).map((sq: any) => ({
      mid: String(sq.mid),
      type: "SQUARE" as ChatType,
      name: sq.name || String(sq.mid).slice(0, 10),
      unread: 0,
      lastTime: 0,
    }));
  }

  /**
   * 一個 OpenChat 聊天室的最近訊息，舊到新。talk 的 history() 對 square 無效，
   * 這是 square 的對應物。
   *
   * 這才是 OpenChat 的完整訊息紀錄。fetchMyEvents 回的是 NOTIFICATION_MESSAGE ——
   * 推播通知，快速連發時會合併成一則，所以只靠它會漏訊息。
   */
  async squareHistory(squareChatMid: string, count = 20): Promise<Msg[]> {
    // 照 linejs 自己的 ClientSquareChat.listen()：不帶 syncToken、不帶 direction，
    // 從歷史開頭給，一路翻到 events 為空（= 追到現在）。帶 direction BACKWARD
    // 反而一則都不回。
    const events: any[] = [];
    let sync: string | undefined;
    for (let page = 0; page < SQUARE_MAX_PAGES; page++) {
      const res: any = await this.#base.square.fetchSquareChatEvents({
        squareChatMid, syncToken: sync, limit: 100,
      });
      const got: any[] = res?.events ?? [];
      events.push(...got);
      sync = res?.syncToken ?? sync;
      if (!got.length) break;
    }
    const out: Msg[] = [];
    for (const ev of events) {
      // 自己發的是 SEND_MESSAGE、別人發的是 RECEIVE_MESSAGE
      const sm = ev?.payload?.sendMessage?.squareMessage
        ?? ev?.payload?.receiveMessage?.squareMessage;
      const raw = sm?.message;
      if (!raw) continue;
      try {
        const msg = await this.#toMsg(raw);
        msg.chat = squareChatMid;
        msg.chatName = this.#squareChats.get(squareChatMid);
        out.push(msg);
      } catch { /* 單則失敗不影響其他 */ }
    }
    out.sort((a, b) => a.time - b.time);
    return out.slice(-count);
  }

  /** squareChatMid → 聊天室名稱。從事件流裡撿的，見 #harvestSquareChat()。 */
  #squareChats = new Map<string, string>();

  /**
   * 從事件裡撿 squareChat 的名字。
   *
   * 名字沒有 API 可查：getJoinedSquareChats 伺服器回 NOT_IMPLEMENTED，
   * getSquareChat 回 ILLEGAL_ARGUMENT。但成員／聊天室相關的事件本身就帶完整的
   * chat 物件，而 syncToken 為空時 fetchMyEvents 會把這些事件重播一次 ——
   * 所以 daemon 每次啟動的第一輪就會把對照表補起來。
   */
  #harvestSquareChat(ev: any): void {
    for (const payload of Object.values(ev?.payload ?? {})) {
      const chat = (payload as any)?.chat;
      if (chat?.squareChatMid && chat?.name) {
        this.#squareChats.set(String(chat.squareChatMid), String(chat.name));
      }
    }
  }

  // listen() 內部會解密，但歷史訊息是生的 —— 兩條路必須走同一個解密函式，
  // 否則症狀會是「即時訊息正常、往上捲全是亂碼」。
  #cacheRaw(raw: any) {
    const id = String(raw?.id ?? "");
    if (!id) return;
    this.#raw.set(id, raw);
    if (this.#raw.size > RAW_CACHE_MAX) {
      // Map 依插入序，砍最舊的那批就好。
      const drop = this.#raw.size - RAW_CACHE_MAX;
      let i = 0;
      for (const k of this.#raw.keys()) {
        if (i++ >= drop) break;
        this.#raw.delete(k);
      }
    }
  }

  /**
   * 把媒體下載到本機快取，回傳檔案路徑。preview 取縮圖（圖片牆用小的就夠）。
   * 三種來源照 linejs 自己的順序試（client/features/message/talk.ts:365）。
   */
  async download(messageId: string, preview = false): Promise<string> {
    const raw = this.#raw.get(messageId);
    if (!raw) throw new Error("訊息不在快取裡（先讀一次那個聊天室的歷史）");

    const contentType = String(raw.contentType ?? "");
    const ext = preview && contentType === "VIDEO"
      ? "jpg" // 影片的預覽是一張圖
      : (contentType === "FILE" ? safeExt(await this.#fileNameOf(raw)) : undefined)
        ?? EXT[contentType] ?? "bin";
    mkdirSync(MEDIA_DIR, { recursive: true });
    const path = join(MEDIA_DIR, `${messageId}${preview ? ".preview" : ""}.${ext}`);
    if (existsSync(path)) return path; // 同一則不重抓

    // OpenChat（square）的圖必須走 OBS，不能走下面的 E2EE 分支：square 訊息也有
    // chunks，但 downloadMediaByE2EE 在 base/obs/mod.ts:441 明確擋掉非 u/c 開頭
    // 的 mid，連試都不試就丟 "Invalid mid"。分支順序讓它永遠先撞到那道牆。
    let blob: Blob;
    if (String(raw.to ?? "").startsWith("m")) {
      blob = await this.#base.obs.downloadMessageData({
        messageId, isPreview: preview, isSquare: true,
      });
    } else if (raw.contentMetadata?.DOWNLOAD_URL) {
      const url = preview
        ? raw.contentMetadata.PREVIEW_URL ?? raw.contentMetadata.DOWNLOAD_URL
        : raw.contentMetadata.DOWNLOAD_URL;
      blob = await (await this.#base.fetch(url)).blob();
    } else if (raw.chunks) {
      // E2EE 影片的預覽圖跟原檔放在同一個 OID 底下、加 __ud-preview，用同一把
      // keyMaterial 加密（見 linejs uploadMediaByE2EE）。downloadMediaByE2EE 不認得
      // preview，所以換掉 OID 再交給它 —— 解金鑰、帶標頭、解密都照原本的路走。
      // 只對影片這樣做：沒有這段的話「影片縮圖」會把整部影片抓下來。
      const src = preview && contentType === "VIDEO"
        ? { ...raw, contentMetadata: { ...raw.contentMetadata, OID: `${raw.contentMetadata?.OID}__ud-preview` } }
        : raw;
      const file = await this.#base.obs.downloadMediaByE2EE(src);
      if (!file) throw new Error("E2EE 媒體下載失敗");
      blob = file;
    } else {
      blob = await this.#base.obs.downloadMessageData({
        messageId, isPreview: preview, isSquare: false,
      });
    }
    writeFileSync(path, Buffer.from(await blob.arrayBuffer()), { mode: 0o600 });
    return path;
  }

  /** 檔名。E2EE 檔案的檔名在加密內容裡，metadata 沒有 —— 不解就只能顯示成無名檔、存成 .bin。 */
  async #fileNameOf(raw: any): Promise<string | undefined> {
    const plain = raw.contentMetadata?.FILE_NAME as string | undefined;
    if (plain || String(raw.contentType) !== "FILE" || !raw.chunks?.length) return plain;
    try {
      return (await this.#base.e2ee.decryptE2EEDataMessage(raw)).fileName;
    } catch {
      return undefined; // 沒檔名照樣能下載
    }
  }

  /**
   * 把公開 CDN 上的圖（貼圖、FLEX 的圖）存進快取。刻意用全域 fetch 而不是 #base.fetch：
   * FLEX 的網址是發訊息的人給的，不能把 LINE 的憑證標頭帶過去。
   * ponytail: 只擋非 https 與過大的回應，沒擋內網位址（面板本來就直接載這些網址）。
   * 要擋的話在這裡解析 host 再比對私有網段。
   */
  async #cachePublic(fileName: string, url: string): Promise<string> {
    if (!url.startsWith("https://")) throw new Error("只抓 https");
    mkdirSync(MEDIA_DIR, { recursive: true });
    const path = join(MEDIA_DIR, fileName);
    if (existsSync(path)) return path;
    const res = await fetch(url, { signal: AbortSignal.timeout(8000) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const buf = Buffer.from(await res.arrayBuffer());
    if (buf.length > THUMB_MAX_BYTES) throw new Error("圖太大");
    writeFileSync(path, buf, { mode: 0o600 });
    return path;
  }

  /**
   * 發話者的大頭貼（小圖）。檔名用 picturePath 的雜湊：同一張圖不重抓，
   * 對方換了照片 picturePath 會變，自然就是新檔。
   */
  async #avatarOf(mid: string): Promise<string | undefined> {
    const pic = this.#pictures.get(mid);
    if (!pic?.startsWith("/")) return undefined;
    const tag = createHash("sha1").update(pic).digest("hex").slice(0, 16);
    try {
      return await this.#cachePublic(`avatar-${tag}`, `https://profile.line-scdn.net${pic}/preview`);
    } catch {
      return undefined; // 沒圖就讓 UI 畫名字的第一個字
    }
  }

  async #toMsg(raw: any): Promise<Msg> {
    let text: string = raw.text ?? "";
    let decryptFailed = false;
    if (raw.chunks?.length) {
      try {
        text = (await this.#base.e2ee.decryptE2EEMessage(raw)).text ?? "";
      } catch {
        text = "";
        decryptFailed = true;
      }
    }
    this.#cacheRaw(raw);
    const contentType = String(raw.contentType ?? "NONE");
    const size = Number(raw.contentMetadata?.FILE_SIZE ?? 0);
    const altText = raw.contentMetadata?.ALT_TEXT as string | undefined;
    const flexJson = raw.contentMetadata?.FLEX_JSON as string | undefined;
    const flex = flexJson ? flexOf(flexJson) : undefined;
    return {
      id: String(raw.id ?? ""),
      chat: chatOf(raw, this.me.mid),
      from: raw.from,
      fromName: this.#names.get(raw.from) ?? raw.from?.slice(0, 8) ?? "?",
      text,
      time: Number(raw.createdTime ?? 0),
      contentType,
      decryptFailed,
      hasMedia: MEDIA_TYPES.has(contentType),
      fileName: await this.#fileNameOf(raw),
      fileSize: size > 0 ? size : undefined,
      duration: Number(raw.contentMetadata?.DURATION ?? raw.contentMetadata?.AUDLEN ?? 0) || undefined,
      altText,
      flexImages: flex?.images,
      flexText: flex?.text || undefined,
      flexLinks: flex?.links.length ? flex.links : undefined,
      // 已讀數只對自己發的有意義；伺服器對別人的訊息也可能填，不要。
      readCount: raw.from === this.me.mid && Number(raw.readCount) > 0 ? Number(raw.readCount) : undefined,
      reactions: Array.isArray(raw.reactions) && raw.reactions.length
        ? raw.reactions.map((r: any) => ({
          from: String(r?.fromUserMid ?? ""),
          type: reactionName(r?.reactionType?.predefinedReactionType),
        }))
        : undefined,
      // 解密之後才讀：E2EE 訊息的 MENTION 可能在加密的內容裡，decrypt 會把它併回 contentMetadata。
      mentions: mentionsOf(raw.contentMetadata?.MENTION),
      sticker: stickerOf(raw.contentMetadata),
    };
  }

  // 群組/聊天室的名稱在 getChats，一對一的在 getContacts。
  async #resolveNames(mids: string[], types: string[]): Promise<void> {
    const chatMids: string[] = [];
    const userMids: string[] = [];
    mids.forEach((mid, i) => {
      if (!mid || this.#names.has(mid)) return;
      (types[i] === "USER" || mid.startsWith("u") ? userMids : chatMids).push(mid);
    });
    if (chatMids.length) {
      try {
        const { chats } = await this.#base.talk.getChats({ chatMids: [...new Set(chatMids)] });
        for (const c of chats ?? []) {
          this.#names.set(c.chatMid, c.chatName || c.chatMid.slice(0, 10));
          if (c.picturePath) this.#pictures.set(c.chatMid, c.picturePath);
        }
      } catch { /* 沒名字不影響收發 */ }
    }
    if (userMids.length) {
      try {
        for (const c of await this.#base.talk.getContacts({ mids: [...new Set(userMids)] })) {
          this.#names.set(c.mid, c.displayName || c.mid.slice(0, 10));
          if (c.picturePath) this.#pictures.set(c.mid, c.picturePath);
        }
      } catch { /* 同上 */ }
    }
  }
}

/** 訊息屬於哪個聊天室：一對一時對方是 from（別人發的）或 to（自己發的）。 */
export function chatOf(raw: any, myMid: string): string {
  return raw.toType === "USER" || raw.toType === 0
    ? (raw.from === myMid ? raw.to : raw.from)
    : raw.to;
}
