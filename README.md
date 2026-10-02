# enil

用個人 LINE 帳號的程式化介面：CLI、終端機 TUI 與 omarchy bar 外掛，共用一個常駐 daemon。

不是 LINE Messaging API（那是 Bot 帳號）。這裡走的是
[`@evex/linejs`](https://jsr.io/@evex/linejs) 逆向出來的個人帳號協定，能讀自己的
歷史訊息、E2EE 對話、送檔案，以你自己的身分。

## 用之前要知道

- **這不是官方支援的用法。** 協定是逆向來的，LINE 隨時可以改掉它，也可以因此限制或
  停用你的帳號。會不會發生沒有人能保證，用的人自己承擔。
- **它以「次要裝置」登入**，手機仍是主裝置。LINE 對次要裝置有數量上限：同一個帳號
  在第二台機器登入 enil，第一台會被踢掉。會不會影響你原本在用的電腦版或平板版 LINE，
  沒有驗證過。
- **訊息與金鑰都存在本機、沒有加密。** daemon 收到的每則訊息明文寫進
  `archive.jsonl`，E2EE 私鑰在 `storage.json`，都只靠檔案權限（0600）保護。
  拿得到你家目錄的人就讀得到。
- **訊息會真的送到真人手上**，無法從程式端收回。`broadcast` 更是一次打擾一整份名單。

## 架構

```
daemon.ts          唯一持有 LINE session
  ├─ state.json    唯讀快照（登入狀態、聊天室、未讀）
  └─ unix socket   一行一個 JSON 請求／回應，另有新訊息推送
       ├── cli.ts    一次性指令，輸出 JSON
       ├── tui/      終端機 TUI（Rust，不碰 LINE 協定）
       └── 外掛      bar 上的未讀數，點一下開 TUI（獨立 repo，只讀 state.json）
```

為什麼一定要 daemon，而不是每個指令自己登入：

1. **refresh token 會 rotate**。兩個 process 同時登入會互相洗掉憑證，最後得重掃 QR。
   這條是**跨機器**的 —— 在另一台機器登入會把這台踢掉，所以同一個帳號同時只在
   一台機器上跑 enil。
2. **收訊息靠 `listen()` 常駐連線**，一次性指令拿不到「剛剛進來的訊息」。

協定知識全部收在 `line.ts`，其餘都是它的消費者。

## 需要什麼

| 東西 | 版本 | 用途 |
|---|---|---|
| Node.js | 要有內建 `fetch`（18 起）。實際只在 26 上跑過 | daemon 與 CLI |
| npm | 任何版本。12 以上靠 repo 裡的 `.npmrc` 才裝得起來，別刪它 | 安裝依賴 |
| Rust | 1.85 以上（edition 2024） | 只有 TUI 需要 |
| `qrencode` | — | 只有不在終端機前登入時需要（把 QR 寫成 PNG） |

daemon 與 CLI 在 Linux 與 macOS 都跑過。TUI 只在 Linux 上測過（macOS 理論上能編，
開附件會改用 `open`）。omarchy 外掛只有 omarchy 能用。

## 安裝

**enil 自己就是完整的**，不需要 omarchy、不需要任何圖形介面。

```bash
npm install
npm run daemon          # 第一次會在終端機直接畫出 QR，用手機 LINE 掃
```

掃完會顯示一組 PIN，在手機上輸入即可。之後啟動走快取 token，不用再掃。

開機自動啟動（Linux／systemd）。`enil.service` 裡的 `@REPO@` 是佔位符，
下面這行會換成當下的 repo 路徑 —— 所以 repo 放哪都行，在根目錄跑就對了：

```bash
mkdir -p ~/.config/systemd/user
sed "s|@REPO@|$PWD|" enil.service > ~/.config/systemd/user/enil.service
systemctl --user daemon-reload
systemctl --user enable --now enil
```

macOS 用 `launchd/` 裡的 plist，安裝指令寫在那個檔案開頭。

TUI 要另外編（見 [`tui/README.md`](tui/README.md)）：

```bash
cd tui && cargo build --release
ln -s "$PWD/target/release/enil-tui" ~/.local/bin/enil-tui
```

### 登入

**QR 只在有人要求時才產生。** daemon 啟動只會試快取憑證；沒有可用憑證就停在
`idle`，等你開 `enil-tui`（它會顯示登入畫面）或跑 `npm run line login`。

這是刻意的：QR 需要有人拿手機在旁邊，自動產生只會在無人時留下一張沒人掃的憑證，
並持續敲 LINE 的伺服器。

QR 的顯示方式依 daemon 怎麼啟動而定：

| 環境 | 行為 |
|---|---|
| 終端機（TTY） | 直接把 QR 畫在終端機上 |
| systemd／pipe（非 TTY） | 寫成 PNG，路徑印在 log 也放進 `state.json` |
| 非 TTY 且沒有 `qrencode` | 不產 PNG；log 與 `state.json` 只給登入網址與原因 |

所以 daemon 在背景跑時也能完成登入 —— `enil-tui` 的登入畫面就是把 `state.json` 裡
那張 PNG 畫出來，再顯示要在手機上輸入的 PIN。

## 三種介面

| 介面 | 用途 |
|---|---|
| `npm run line …` | 一次性指令，輸出 JSON。人在終端機用、agent 也用同一套 |
| [`tui/`](tui/) | 終端機 TUI（Rust + ratatui）：看訊息、回覆、開附件、搜尋、登入 |
| omarchy 外掛 | bar 上的未讀數，點一下開 TUI |

前兩個是 daemon 的消費者，走同一個 socket；外掛只讀 `state.json`。
**同時只能有一個 session 持有者** —— 會自己登入的東西（例如 `probe-e2ee.ts`）
在 daemon 執行時會拒絕啟動。

## 用法

```bash
npm run line chats 50 Alice            # 找聊天室，第二個參數篩名稱
npm run line read <mid> 20             # 讀訊息（E2EE 已解密、圖片縮圖已下載）
npm run line read <mid> 20 --read      # 順便標成已讀
npm run line download <msgId> --full   # 下載媒體原檔，回傳本機路徑
npm run line send <mid> "訊息"
npm run line sendfile <mid> ~/a.png
npm run line squares                   # 加入的 OpenChat（不在 chats 裡）
npm run line sread <squareChatMid> 20  # 讀某個 OpenChat
```

`read` 回傳的每則訊息除了文字，還有圖片與影片的縮圖路徑、FLEX 版面的文字與連結、
檔名、發話者大頭貼等欄位，完整清單在 [`SKILL.md`](SKILL.md)。`download` 需要那則
訊息在 daemon 快取裡，也就是要先 `read` 過該聊天室。

`SKILL.md` 同時是給 agent 用的說明（放到 `~/.claude/skills/line/` 即可）。

LINE 伺服器對次要裝置只保留約兩週的歷史。daemon 會把收到的每則訊息存進
`~/.local/state/enil/archive.jsonl`，那是唯一的長期紀錄，從開始跑的那天算起。

## VIP

在 TUI 裡對某個聊天室按 `Ctrl-P` 設成 VIP（名單存在 `~/.local/state/enil/vip`，一行一個
mid）。VIP 的新訊息一定要被看到，所以提醒是 daemon 發的，TUI 沒開也一樣：

- 有裝 omarchy 外掛：螢幕中央彈一張大卡片，蓋在所有視窗上，讀過那個聊天室才消失。
- 沒有：`notify-send -u critical`，不會自己消失的系統通知（需要 libnotify）。

正開著那個聊天室、而且 TUI 有焦點時不提醒。

## omarchy 外掛

在獨立的 repo：**[omarchy-line](https://github.com/Unayung/omarchy-line)**

```bash
omarchy plugin add https://github.com/Unayung/omarchy-line.git --enable
```

它只讀 `state.json`，本身不碰 LINE。點圖示會開 `~/.local/bin/enil-tui`。

## 對外契約

任何東西照這個契約來都能取代 enil，消費者不需要知道對面是誰。

**`~/.local/state/enil/state.json`** —— 原子寫入（先寫 `.tmp` 再 rename），
所以監看者不會讀到寫到一半的內容。

```jsonc
{
  "me": { "mid": "u…", "displayName": "…" },   // 未登入時為 null
  "updatedAt": 1787740000000,                   // 用來判斷 daemon 是否還活著
  "login": { "status": "starting|idle|qr|pin|ok|error", "qrPng": "…", "url": "…", "pin": "…" },
                                                // idle = 沒憑證，等人主動要求 QR
  "chats": [{ "mid": "…", "name": "…", "type": "USER|GROUP|ROOM",
              "unread": 0, "lastTime": 0, "lastText": "…", "lastFrom": "…",
              "avatar": "/…/media/avatar-…" }], // 最近 50 個；avatar 可能沒有
  "vip": [{ "id": "…", "chat": "…", "name": "…", "from": "…", "text": "…", "time": 0 }]
                                                // VIP 還沒被讀的訊息，聊天室讀過就移除
}
```

**`~/.local/state/enil/sock`** —— unix socket，一行一個 JSON 請求，回一行 JSON。
請求可帶 `id`，回應原樣奉還（同時發多筆才不會對錯）。`history` 的 `before` 給某則
訊息 id 就取「那則之前」的，用來往前翻頁 —— 注意錨點那則會被重複回傳，呼叫端要去重。

| cmd | 參數 | 回傳 |
|---|---|---|
| `state` | — | 上面那份快照 |
| `login` | — | 觸發 QR 登入（沒有快取憑證時） |
| `chats` | `limit` `query` | 聊天室陣列（含 `lastText`／`lastFrom` 預覽；前 60 個帶 `avatar`） |
| `history` | `chat` `count` `markRead` `before` | 訊息陣列（舊到新） |
| `send` | `chat` `text` | `{sent:true}` |
| `sendFile` | `chat` `path` | `{oType,e2ee}` |
| `download` | `messageId` `preview` | `{path}` |
| `squares` | — | 加入的 OpenChat |
| `squareHistory` | `chat` `count` | 某個 OpenChat 的訊息 |
| `subscribe` | — | 見下 |

`subscribe` 把那條連線變成推送用的：先回一行 `{ok:true}`，之後每收到一則新訊息就寫
一行 `{"event":"message","data":<訊息>}`，直到連線關閉。登入前就能訂。請求照舊另開
連線，不要跟訂閱共用。OpenChat 的訊息不會被推送。

## 繞過的 linejs 問題

目前用 `@evex/linejs` 3.3.3。有四個問題是在這裡繞過的（其中兩個上游已修，繞法留著
當保險）：`loginWithQR` 不看快取憑證、rotate 後的 refresh token 沒存、E2EE 金鑰挑錯
把、E2EE 影片的驗證碼算法不同導致一律下載失敗。細節與「為什麼長這樣」在
[`CLAUDE.md`](CLAUDE.md)，改 `login.ts`／`line.ts` 之前先讀。

## 注意

- 登入憑證在 `~/.local/state/enil/storage.json`（權限 0600），**不要提交也不要複製**
  到別台機器 —— 兩邊同時用會互相洗掉。
- 媒體快取在 `~/.local/state/enil/media/`，權限 0600。
- 測試要送訊息時，送到自己的聊天室（`chats` 裡 mid 等於 `state.me.mid` 那個），
  不要拿真人試。
