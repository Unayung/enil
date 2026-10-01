// 從自己的 LINE 訊息萃取說話風格，輸出 style.md。
// 跑：npx tsx style.ts [目標則數=500] [聊天室數=120]
//
// 往深挖而不是往廣挖：多數聊天室是通知群，自己一句話都沒說。
// 第一頁沒撈到自己的訊息就整個跳過，有的就用 before 一直往前翻。
//
// 產出含真實私人訊息，留在本機、已在 .gitignore。
import { connect } from "node:net";
import { writeFileSync } from "node:fs";
import { SOCK } from "./paths.js";

function ask(req: unknown): Promise<any> {
  return new Promise((resolve, reject) => {
    const sock = connect(SOCK);
    let buf = "";
    sock.on("connect", () => sock.write(JSON.stringify(req) + "\n"));
    sock.on("data", (chunk) => {
      buf += chunk;
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      sock.destroy();
      try { resolve(JSON.parse(buf.slice(0, nl))); } catch (e) { reject(e); }
    });
    sock.on("error", () => reject(new Error("daemon 沒在跑")));
  });
}

interface Sample { text: string; group: boolean; burst: boolean }

const target = Number(process.argv[2] ?? 500);
const chatCount = Number(process.argv[3] ?? 120);
const PAGE = 100;          // 每次翻頁抓幾則
const MAX_PAGES = 12;      // 單一聊天室最多翻幾頁

const state = (await ask({ cmd: "state" })).data;
const me: string = state.me?.mid ?? "";
if (!me) throw new Error("daemon 還沒登入");

const chats = (await ask({ cmd: "chats", limit: chatCount })).data as any[];
const samples: Sample[] = [];
let scanned = 0;
let touched = 0;

/** 收一頁的訊息，回傳這頁撈到幾則自己的，以及最舊那則的 id（給下一頁當錨點）。 */
function harvest(msgs: any[], group: boolean): { mine: number; oldest?: string } {
  let mine = 0;
  msgs.forEach((m, i) => {
    scanned++;
    if (m.from !== me || m.hasMedia || m.decryptFailed) return;
    const text = String(m.text ?? "").trim();
    if (!text || text.startsWith("http")) return;
    // 連發：上一則也是我、且相隔 90 秒內。這是很個人的習慣，值得單獨記。
    const prev = msgs[i - 1];
    const burst = Boolean(prev && prev.from === me && m.time - prev.time < 90_000);
    samples.push({ text, group, burst });
    mine++;
  });
  return { mine, oldest: msgs[0]?.id };
}

for (const c of chats) {
  if (samples.length >= target) break;
  const group = c.type !== "USER";
  const first = await ask({ cmd: "history", chat: c.mid, count: PAGE });
  if (!first.ok || !Array.isArray(first.data) || first.data.length === 0) continue;
  touched++;
  let { mine, oldest } = harvest(first.data, group);
  // 通知群這種地方自己一句話都沒有，別浪費來回往前翻。
  if (mine === 0) continue;

  for (let page = 1; page < MAX_PAGES && oldest && samples.length < target; page++) {
    const res = await ask({ cmd: "history", chat: c.mid, count: PAGE, before: oldest });
    if (!res.ok || !Array.isArray(res.data) || res.data.length === 0) break;
    // before 會把錨點那則一起回傳，去掉才不會重複計。
    const fresh = res.data.filter((m: any) => m.id !== oldest);
    if (fresh.length === 0) break;
    const got = harvest(fresh, group);
    oldest = got.oldest ?? oldest;
  }
  console.log(`  ${samples.length}/${target}　（掃過 ${scanned} 則、${touched} 個聊天室）`);
}

if (samples.length === 0) throw new Error("沒有取到任何自己的訊息");

const lens = samples.map((s) => s.text.length).sort((a, b) => a - b);
const pct = (q: number) => lens[Math.min(lens.length - 1, Math.floor(lens.length * q))];
const share = (fn: (s: Sample) => boolean) =>
  Math.round(samples.filter(fn).length / samples.length * 100);

const stats = {
  total: samples.length,
  median: pct(0.5),
  p90: pct(0.9),
  max: lens[lens.length - 1],
  burst: share((s) => s.burst),
  english: share((s) => /[A-Za-z]{3,}/.test(s.text)),
  fullStop: share((s) => /[。．]/.test(s.text)),
  question: share((s) => /[？?]/.test(s.text)),
  emoji: share((s) => /\p{Extended_Pictographic}/u.test(s.text)),
  laugh: share((s) => /(哈{2,}|[Xx][Dd]|ㄏ{2,})/.test(s.text)),
  group: share((s) => s.group),
};

// 範例挑法：長度分桶各取幾則，一對一與群組分開 —— 語氣不一樣。
function pick(pool: Sample[], n: number): string[] {
  const buckets: Sample[][] = [[], [], []];
  for (const s of pool) {
    const i = s.text.length <= 6 ? 0 : s.text.length <= 20 ? 1 : 2;
    buckets[i].push(s);
  }
  const out: string[] = [];
  for (const b of buckets) {
    const step = Math.max(1, Math.floor(b.length / Math.ceil(n / 3)));
    for (let i = 0; i < b.length && out.length < n; i += step) out.push(b[i].text);
  }
  return out;
}

const oneToOne = pick(samples.filter((s) => !s.group), 15);
const inGroup = pick(samples.filter((s) => s.group), 15);

const md = `# 我的說話風格（自動萃取）

來自 ${touched} 個聊天室、掃過 ${scanned} 則訊息，取得 ${stats.total} 則自己發的文字訊息。
**這份檔案含真實私人訊息，不要提交或外流。**

## 量化特徵

| 指標 | 值 |
|---|---|
| 長度中位數 | ${stats.median} 字 |
| p90 長度 | ${stats.p90} 字 |
| 最長 | ${stats.max} 字 |
| 連發（90 秒內接續自己） | ${stats.burst}% |
| 含英文詞 | ${stats.english}% |
| 含句號 | ${stats.fullStop}% |
| 含問號 | ${stats.question}% |
| 含 emoji | ${stats.emoji}% |
| 含笑（哈哈／XD） | ${stats.laugh}% |
| 來自群組 | ${stats.group}% |

## 據此得到的規則

- **短**。中位數 ${stats.median} 字，超過 ${stats.p90} 字就該考慮拆開。
- **少用句號**（只有 ${stats.fullStop}%）。句子結束就結束，不補標點。
- ${stats.burst >= 25 ? `**拆成多則連發**（${stats.burst}% 是連發）。一個想法一則，不要塞成一段。` : "多半一則講完，不特別拆。"}
- ${stats.english >= 20 ? `**中文夾英文技術詞**（${stats.english}%）。repo、daemon、deploy 直接用英文，不翻譯。` : "少用英文詞。"}
- ${stats.emoji < 10 ? `**幾乎不用 emoji**（${stats.emoji}%）。` : `emoji 出現在 ${stats.emoji}% 的訊息。`}

## 驗證方式

產出的訊息拿同一套統計比對：長度中位數應接近 ${stats.median}、句號比例應低於 ${Math.max(5, stats.fullStop + 3)}%。

## 一對一的樣子

${oneToOne.map((t) => "- " + t.replace(/\n/g, " ⏎ ")).join("\n")}

## 群組裡的樣子

${inGroup.map((t) => "- " + t.replace(/\n/g, " ⏎ ")).join("\n")}
`;

writeFileSync("style.md", md);
console.log(`✅ style.md：${stats.total} 則語料`);
console.log(`   中位數 ${stats.median} 字、連發 ${stats.burst}%、英文 ${stats.english}%、句號 ${stats.fullStop}%`);
