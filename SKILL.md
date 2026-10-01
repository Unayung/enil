---
name: line
description: >
  用 CLI 讀寫使用者的個人 LINE 帳號：列聊天室、讀訊息（含 E2EE 解密、圖片、FLEX）、
  送文字與檔案、依名單群發、下載媒體、標已讀、列 OpenChat。當使用者提到「LINE」、
  「傳訊息給某某」、「看一下某某的對話」、「LINE 未讀」、「傳檔案到 LINE」、
  「群發」、「OpenChat」時使用。
  訊息會真的送到真人手上，送出前務必確認收件人；群發還會打擾一整份名單。
---

# LINE CLI

操作使用者**個人 LINE 帳號**的命令列介面。輸出一律是 JSON，適合接 `jq`。

專案在 `~/Projects/enil`。所有指令都要在該目錄下跑。

## 先決條件

session 由一個常駐 daemon 持有（systemd user service）。沒跑的話每個指令都會回
`{"ok":false,"error":"daemon 沒在跑"}`。

```bash
systemctl --user status enil     # 檢查
systemctl --user start enil      # 啟動（開機會自動跑）
```

`state.login.status` 是 `idle` 表示沒有可用憑證 —— QR 不會自動產生，要
`cli.ts login` 或請使用者按面板上的登入鈕（需要手機在旁邊）。

**不要對同一個帳號另外跑一份 daemon。** LINE 的 refresh token 是 rotate 的，兩個
process 拿同一個帳號登入會互相洗掉憑證，最後使用者得重新掃 QR。不同帳號各跑一份
是可以的，見下一節。

## 多帳號

一個 daemon 一個帳號。第二個帳號就是第二個 daemon，用 `ENIL_STATE_DIR` 隔開 ——
socket、憑證、狀態、存檔全部跟著搬，兩邊互不影響。

```bash
# 現在有哪些帳號在跑：一個 sock 就是一個 daemon
fd -t s '^sock$' ~/.local/state --max-depth 3

npx tsx cli.ts chats 5                                      # 預設帳號
ENIL_STATE_DIR=~/.local/state/enil-b npx tsx cli.ts chats 5 # 另一個帳號
```

`XDG_STATE_HOME` 也還能用（會變成 `<那個路徑>/enil`，多一層）。兩個都設時
`ENIL_STATE_DIR` 優先。**daemon 與 CLI 要帶同一個值**，不然 CLI 會連到別的帳號。

使用者沒指明就用預設帳號（不帶環境變數那個）。

**找不到人的時候先確認帳號對不對。** 拿 A 帳號去搜 B 帳號的聯絡人只會回空陣列、
不會報錯 —— 症狀是「查無此人」，實際是問錯帳號。

## 指令

```bash
cd ~/Projects/enil

npx tsx cli.ts chats [limit] [關鍵字]         # 聊天室清單，關鍵字比對名稱
npx tsx cli.ts read <chat> [count] [--read]   # 讀訊息，--read 順便標已讀
npx tsx cli.ts download <messageId> [--full]  # 下載媒體，回傳本機路徑
npx tsx cli.ts send <chat> <text...>          # 送文字
npx tsx cli.ts sendfile <chat> <path>         # 送檔案／圖片，支援 ~
npx tsx cli.ts broadcast <名單檔> <text...>   # 群發，見下節
npx tsx cli.ts squares                        # 加入的 OpenChat（不在 chats 裡）
npx tsx cli.ts sread <squareChatMid> [count]  # 讀某個 OpenChat 聊天室的訊息
npx tsx cli.ts login                          # 沒有快取憑證時觸發 QR 登入
npx tsx cli.ts state                          # daemon 快照
```

`<chat>` 是 mid，不是名字。先用 `chats` 查。

## 群發

```bash
npx tsx cli.ts broadcast <名單檔> <text...> [--tag 名稱] [--delay 秒] [--dry-run] [--yes]
```

名單一行一個對象，`u`/`c`/`r` 開頭 + 32 個 hex 當 mid，其餘當名稱做子字串比對，
`//` 開頭是註解。

**預設不送。** 沒有 `--yes` 只會印出計畫並回 exit 2 —— 那不是成功，是還沒送。
名單裡有任何一筆解析不出來（找不到、同名、認不得的 mid）就整批拒發回 exit 1，
不會「送出能送的那些」。

`--tag` 會把送出的對象記進 `sent.jsonl`，同一個 tag 重跑時跳過。**群發一律帶 tag**：
中途失敗重跑時，沒有 tag 就是讓同一個人再收一次。

`--delay` 預設 3 秒，是憑感覺的節奏不是已知安全線 —— LINE 沒有公開的速率上限。

失敗會停在第一筆，不會繼續跑完。stdout 的 JSON 帶 `restricted` 欄位時代表帳號被
LINE 限制了（`ABUSE_BLOCK` / `EXCEED_DAILY_QUOTA` / `FEATURE_RESTRICTED` …）——
**這種情況不要重跑**，重送只會加重限制，先請使用者用手機確認帳號狀態。

群發是打擾真人的操作，收件人可以按檢舉。名單與內容一定要使用者確認過再帶 `--yes`。

## OpenChat

`squares` 列出加入的 OpenChat。它們**不會出現在 `chats`** —— OpenChat 在協定上
是另一套 API（square），`chats` 只認一般聊天室。

讀 OpenChat 用 `sread`，不是 `read`（`read` 只認一般聊天室）。它拉的是那個聊天室
完整的事件流，所以**比 archive 完整** —— archive 只有 daemon 啟動之後的，`sread`
連啟動前的都拿得到：

```bash
npx tsx cli.ts sread m290e5a05910d59960aeb8c91cb1ad85e 20 \
  | jq -r '.[]|"\(.time/1000|strflocaltime("%m-%d %H:%M"))  \(.fromName)：\(.text)"'
```

`send` 仍不支援 OpenChat。daemon 每 10 秒拉一輪把新訊息存進 `archive.jsonl`。

**不要拿 `fetchMyEvents` 的通知當訊息來源。** 那是推播通知，5 秒內連發會合併成
最後一則 —— 實測「照片 → 想找這樣的店面 → 松山區」通知只給了「松山區」。enil
現在從各聊天室的 `fetchSquareChatEvents` 拉，一則不漏。

**mid 有三種前綴，混用就對不上帳：**

| 前綴 | 是什麼 | 在哪 |
|---|---|---|
| `s` | square，那個「社群」 | `squares` 回的 `mid` |
| `m` | squareChat，社群底下的聊天室 | archive 的 `chat` |
| `p` | squareMember，社群裡的成員 | archive 的 `from` |

所以**不能拿 `squares` 的 mid 去比對 archive 的 `chat`**，那是兩種東西。要篩某個
OpenChat 的訊息用 `chatName`（只有 OpenChat 訊息會帶這個欄位）：

```bash
jq -rc 'select(.chatName=="某某社群")|"\(.fromName)：\(.text)"' ~/.local/state/enil/archive.jsonl
```

**別人**發的訊息 `fromName` 是正常的顯示名稱；**使用者自己**發的會退成 mid 前綴
（`p` 開頭那串）。square 成員不在通訊錄裡，名字只能靠事件自帶的
`senderDisplayName`，而自己發的那則 LINE 不填。監測別人說了什麼不受影響。

## 輸出欄位

`chats` 每筆：`mid` / `name` / `type`（USER｜GROUP｜ROOM）/ `unread` / `lastTime` /
`lastText`（最後一則的預覽）/ `lastFrom`（誰說的）

有了 `lastText` 就不必為了看預覽對每個 mid 再跑一次 `read`：

```bash
# 掃一遍最近在講什麼
npx tsx cli.ts chats 200 | jq -r 'sort_by(-.lastTime)[0:10][]|"\(.name)：\(.lastText)"'
```

`read` 每筆（舊到新排序）：

| 欄位 | 說明 |
|---|---|
| `id` | 訊息 id，`download` 要用 |
| `fromName` | 發話者顯示名稱 |
| `text` | 內文，E2EE 已解密 |
| `time` | epoch **毫秒** |
| `contentType` | NONE（純文字）／IMAGE／FLEX／RICH／STICKER／FILE… |
| `decryptFailed` | true 表示 E2EE 解不開，`text` 會是空的 |
| `mediaPath` | 圖片縮圖、或影片預覽圖（一張 jpg）的本機路徑，已自動下載 |
| `altText` | FLEX／RICH 的純文字備援 |
| `flexImages` | FLEX 版面裡的圖片網址 |
| `flexText` | FLEX 版面裡的文字，一個元件一行。`altText` 常常只是「您有新訊息」，內容在這裡 |
| `flexLinks` | FLEX 版面裡的連結 |
| `fileName` | 檔案名稱（E2EE 的檔案也解得出來） |
| `thumbPath` | 貼圖、或 FLEX 第一張圖的本機路徑（沒有副檔名），已自動下載 |
| `fromAvatar` | 發話者大頭貼的本機路徑（沒有副檔名），對方沒設照片就沒有 |

## 長期歷史

LINE 對次要裝置只保留約兩週，`read` 對更久以前的聊天室會回**空陣列**（不是錯誤）。
daemon 收到的每則訊息都會落地成 `~/.local/state/enil/archive.jsonl`，一則一行，
欄位跟 `read` 回傳的一樣：

```bash
jq -r 'select(.chat=="<mid>")|"\(.fromName)：\(.text)"' ~/.local/state/enil/archive.jsonl
```

只從開始存的那天算起，補不回過去。重連可能重複寫同一則，需要時用 `id` 去重。

## 常用組合

```bash
# 查某人的 mid
npx tsx cli.ts chats 200 Alice | jq -r '.[].mid'

# 讀最後 5 則，轉成可讀格式。圖片／FLEX 的 text 是空的，一定要有 fallback，
# 否則那幾則會印成空白行。
npx tsx cli.ts read <mid> 5 | jq -r '.[]|"\(.time|./1000+28800|strftime("%H:%M"))  \(.fromName)：\(
  if .decryptFailed then "[解密失敗]"
  elif .text != "" then .text
  else (.altText // "[\(.contentType)]") end)"'

# 只看某人發的
npx tsx cli.ts read <mid> 30 | jq -r '.[]|select(.fromName=="Alice")|.text'

# 有未讀的聊天室
npx tsx cli.ts chats 50 | jq -r '.[]|select(.unread>0)|"\(.unread)  \(.name)"'
```

## 送訊息前必做

**訊息會真的送到真人手上，而且收不回來（只能請使用者自己在 LINE 裡收回）。**

1. 用 `chats` 查收件人，**確認只有一個符合**。名字重複很常見 —— 實際遇過兩個同名
   聯絡人，兩邊都讀不到歷史訊息，只能靠最後活動時間區分。
2. 有多筆符合就**停下來問使用者**，不要猜。送錯人無法挽回。
3. 送出後用 `read <mid> 2` 確認送達，並把實際送出的內容原文回報給使用者。

## 陷阱

- **`time` 是 epoch 毫秒，`jq` 的 `strftime` 預設 UTC**。台灣時間要 `./1000+28800`。
- **`download` 需要該訊息在 daemon 快取裡**，也就是要先 `read` 過那個聊天室。
  daemon 重啟後快取清空。
- **少數聊天室讀不到歷史**（回傳空陣列），但未讀數仍然正確、也仍可送訊息。
- **`read` 不會標已讀**，除非加 `--read`。
- 圖片縮圖與原圖存在 `~/.local/state/enil/media/`，權限 0600。

## 相關

`~/.local/state/enil/state.json` 是 daemon 的唯讀快照（登入狀態、未讀數），
omarchy 的 bar 外掛 `io.github.unayung.line` 就是讀它。要輪詢未讀數用這個檔比
反覆呼叫 CLI 便宜。
