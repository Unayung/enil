# enil — 給後續 session 的筆記

個人 LINE 帳號的 daemon 與 CLI。README 講「怎麼用」，這裡講**為什麼長這樣**，
以及踩過哪些坑 —— 這些從程式碼看不出來。

## 唯一的架構約束

**同一時間只能有一個 process 持有 LINE session。**

LINE 的 refresh token 是 rotate 的（JWT 標了 `rot:ROTATE`）：每次登入會換一把新的、
舊的立刻作廢。兩個 process 同時登入 → 互相洗掉憑證 → 使用者得重掃 QR。

daemon 因此存在，其餘全部是它的消費者（走 unix socket）。加新功能時的預設答案是
**在 daemon 加一個 cmd**，不是再寫一個會自己登入的程式。

`probe-e2ee.ts` 是唯一會自己登入的，所以它啟動前用 `daemonAlive()` 擋。

### 這條約束是跨機器的

範圍是**每個 LINE 帳號一個持有者**，不是每台機器一個。LINE 對次要裝置有數量上限，
在第二台機器掃 QR 登入會把第一台**踢掉** —— 之後第一台的 refresh 一直拿到
`INVALID_REQUEST reasonCode 3`，那不是憑證過期，是裝置被除名。

`daemonAlive()` 只查本機 socket，看不到別台。實際踩過：兩台筆電各自跑 enil，
其中一台整夜停在失效狀態。**同一個帳號同時只在一台機器上跑 enil。**

換機器就是重新掃 QR，這是協定給的，沒有繞路。好消息是被踢掉的那台現在會停在
`idle` 等人處理，而不是每五分鐘產一張沒人掃的 QR。

## linejs 的四個 bug（不要「修回去」）

用 `@evex/linejs` 3.4.2（2026-10-02 從 3.3.3 升上來，enil 用到的接口沒有改簽名）。以下是刻意繞過的，不是我們寫錯 —— 但 3.3.3 已經修掉其中兩個
（2026-09-09 對著 node_modules 查證），我們的繞法留著只是多一層保險，不是還在救火：

| 位置 | 問題 | 我們的作法 |
|---|---|---|
| `client/login.ts:68` | `loginWithQR` 從不看 storage，「快取 token」等於沒用 | `login.ts` 自己拿 refreshToken 換，換不到才掃 QR |
| `base/service/auth/mod.ts:21` | `tryRefreshToken()` 不存 rotate 後的新 refreshToken | 自己呼叫 `auth.refresh()` 並存回新的。**3.3.3 已修**（`mod.ts:31` 會 `storage.set`） |
| `base/e2ee/mod.ts:376` | `decryptKeyChain` 硬取金鑰清單 `[0]`，那是最舊的一把 | 挑「私鑰推導出的公鑰 == LINE 登記的公鑰」那把。**3.3.3 已修**（`mod.ts:479` 改成用 keyId 找） |
| `base/e2ee/mod.ts:1491` | `decryptByKeyMaterial` 只認「整份密文算一次 HMAC」，E2EE **影片**一律報 `HMAC verification failed` | `line.ts` 的 `acceptChunkedMac()`：驗不過再用分塊那套驗。3.4.2 仍未修 |

第四個的細節（2026-10-02 拿真實影片的密文離線比對出來的）：影片的驗證碼是
「密文每 131072 bytes 一塊各算 SHA-256，串起來再算 HMAC-SHA256」，金鑰推導與 AES-CTR
跟圖片完全一樣。所以症狀是「預覽圖解得開、原檔解不開」，看起來像金鑰錯，其實不是 ——
跳過驗證直接解就是完整的 mp4。只確認過影片；圖片、檔案、預覽圖都是整份算一次。
`sendFile` 送影片走的還是 linejs 的整份算法，官方 client 收不收沒驗過。

另外 linejs 沒有「下載 E2EE 預覽圖」的方法。預覽放在原檔 OID 後面加 `__ud-preview`、
同一把 keyMaterial 加密（從 `uploadMediaByE2EE` 看出來的），`download()` 把 OID 換掉
再交給 `downloadMediaByE2EE`。

第三個最致命：`[0]` 的公鑰跟 LINE 登記的對不上，**所有 E2EE 訊息都解不開**，而且
storage 裡的 keyId 標籤是對的、內容是錯的，光看檔案完全看不出來。

## 會安靜給錯答案的欄位

這幾個都不會崩，只會少或錯：

- **`lastMessages` 是暫態的**。`getMessageBoxes` 只在「這次同步有新東西」時才帶，
  拿它判斷「聊天室是不是空的」會濾掉 97%。用 `lastDeliveredMessageId`。
- **錨點會過期**。`getPreviousMessages` 取「錨點之前」，錨點只在 `chats()` 更新。
  剛送出的訊息會落在窗外 → 「我剛剛傳的不見了」。所以 `onMessage` 收到訊息就
  `#noteAnchor()` 往前推。
- **分頁的 `before` 包含錨點那則**，前置時要去重。
- **聊天室分頁不能寫死頁數**。實測 757 個聊天室，寫死 4 頁會少算未讀且完全無聲。
  現在抓到 `hasNext` 為 false，超過上限會 `console.warn`。
- **歷史只留約兩週**。實測 2026-09-03：最後活動在 08-27 的群組讀得到，08-17 的
  `getPreviousMessages` 回**空陣列**，`count` 開到 30 也一樣 0 則。不是錯誤，是
  LINE 對次要裝置的保留期，錨點還在但伺服器已經沒有那些訊息。輪詢不會延長它 ——
  要長期歷史只能自己存，見下面的 `archive.jsonl`。
- **`minChatId`/`maxChatId` 給空字串**會被當成真的範圍過濾、回 0 筆。要整個不給。
- **OpenChat 的訊息不能從 `fetchMyEvents` 拿**。它回的是 `NOTIFICATION_MESSAGE`，
  推播通知 —— 5 秒內連發會合併成最後一則，前面的就沒了，而且不報錯。實測三次，
  每次都漏中間那句；同一個聊天室的 `fetchSquareChatEvents` 裡三則都在。型別裡的
  `requiredToFetchChatEvents` 欄位就是在說這件事，我看過卻沒在意。現在通知流只拿來
  撿聊天室名字與發話者名字（聊天室事件流裡沒有 `senderDisplayName`）。
  第一次拉某個聊天室時沒有游標，伺服器會從歷史開頭給 —— 那些不能交出去，否則會
  重灌 archive、讓 watcher 重複通報。用 `#squareSince` 與已交出 id 的集合擋兩道。
- **OpenChat 的圖不能走 E2EE 分支**。square 訊息也有 `chunks`，所以
  `download()` 原本的分支順序會先進 `downloadMediaByE2EE`，而它在
  `base/obs/mod.ts:441` 明確擋掉非 `u`/`c` 開頭的 mid，直接丟 `Invalid mid`。
  square 要走 `obs.downloadMessageData({ isSquare: true })`，那條是通的。
  兩條的錯誤訊息都是 `Invalid mid`，只包其中一條加診斷會誤判成整條路不通 ——
  我就這樣繞了一輪，結論還下錯（以為要整個逆向 square 的 E2EE 媒體路徑）。

## 推送不可靠，所以要對帳（2026-10-02）

linejs 3.4.2 會自己重連一般的斷線（記成 `LegyPusherError`，4 秒後重試），但有兩種它看不出來：

- **卡死**：`connManager.ts` 的 sync 回錯誤後，後面的 TypeError 被 `catch (_e){return}` 吞掉，
  連線靠 ping 維持著，卻再也不送東西。只會留一筆 `[LEGY/PUSH] sync error:` log。
- **revision 只在記憶體**：daemon 重開後從 0 開始，停機期間的訊息不會補推。

所以 daemon 每次 resync（每分鐘）拿 `chats()` 的 `lastDeliveredMessageId.deliveredTime` 跟
`known`（我們收過的最新時間）比，伺服器比較新、而且超過 20 秒，就用 `history` 補抓。`known`
與 `seen` 啟動時從 `archive.jsonl` 讀，所以停機期間的也補得回來。補抓的走 `onIncoming(m, true)`
—— 不加未讀（resync 已經跟伺服器對齊過），但照樣存檔、推送、VIP 提醒。

判定推送死了要兩個條件同時成立：連續兩輪都要補，**而且**三分鐘沒收到任何推送。只看「有補」
會誤判：有些訊息可能本來就不走推送（系統訊息之類），那種每次都會補一則。重啟是 `process.exit(1)`
讓 systemd 拉起來 —— 同一個 process 裡再 `listen()` 不可靠：推送迴圈還活著時 `islisten` 會擋住，
卻又會換掉 stream，舊的 `for await` 就餓死了。

## 不是新訊息的推送

linejs 只把新訊息（`message`）和編輯（`message:edit`）包成事件，已讀、收回、表情要自己從
`client.on("event", op)` 認 `op.type`（字串）。param 的意思 linejs 核心沒有解釋，照的是它的
`example/_event/talk-class.ts`：`NOTIFIED_READ_MESSAGE`（聊天室、讀者、讀到哪則）、
`SEND_CHAT_CHECKED`（我在手機上讀了）、`DESTROY_MESSAGE`／`NOTIFIED_DESTROY_MESSAGE`
（聊天室、訊息）、`SEND_REACTION`／`NOTIFIED_SEND_REACTION`（訊息、`{chatMid, curr}` JSON、誰）。
自我聊天室裡實測過收回與表情；已讀要別人讀才看得到，沒實測過推送那條，history 的
`readCount` 有。

`history` 要帶 `withReadCount: true` 才有已讀數。表情在 `raw.reactions`，沒有
`predefinedReactionType` 的是自訂／付費回應，記成 `OTHER`。

## 自動化的邊界

登入有兩種，只有一種可以自動做：

- **快取 token 續期**：不需要人 → 啟動時自動試
- **QR 登入**：需要有人拿手機在旁邊 → 只在收到 `login` 指令時才做

曾經把兩者當成同一件事的兩條分支（refresh 失敗就 fallback 到 QR），結果憑證失效後
daemon 整夜每五分鐘產一張沒人掃的 QR，還一直敲 LINE 伺服器。**判準不是實作路徑，
是需不需要人在場。**

## 已經試過、無效的路

- 把 `x-line-application` 偽裝成官方 Chrome extension（`CHROMEOS\t3.7.2\t…`）
  → `createQrCodeForSecure` **完全不回應**，連 QR 都產不出來。別再試。

## 驗證方式

CLI 全部可以直接驗，不要用猜的：

```bash
npx tsx cli.ts chats 5 | jq -r '.[].name'
npx tsx cli.ts read <mid> 5 | jq -r '.[]|"\(.fromName)：\(.text)"'
```

**送訊息會真的送到真人手上**。要測送出請用使用者的自我聊天室
（`chats` 裡 mid == `state.me.mid` 那個），不要拿真人測試。

`process.exit()` 會截斷還沒排出去的 stdout —— CLI 用 `process.exitCode`，
大量輸出才不會吐出「看起來合法但少一截」的 JSON。

## 相關位置

- 一個 daemon 一個帳號，靠 `ENIL_STATE_DIR` 隔開（沒設就是 `$XDG_STATE_HOME/enil`）。
  `paths.ts` 只有這一個根，所有路徑都掛在它底下 —— 換掉它就等於換一個帳號。
  rotate 那條約束是**每帳號**的，所以兩個不同帳號同機各跑一個不衝突。
- 狀態、憑證與媒體快取：`~/.local/state/enil/`
  （`state.json` / `sock` / `media/` / `storage.json` / `archive.jsonl`）。
  `archive.jsonl` 是 `onIncoming` 落地的訊息存檔，一則一行、只增不刪、0600 ——
  伺服器只留兩週，這是唯一的長期歷史。重連可能重複寫同一則，讀的時候用 `id` 去重。
  它從加上這功能的那天開始累積，補不回過去。憑證本來在 CWD 的
  `./storage.json`，換個目錄啟動就會重掃 QR 並 rotate 掉舊憑證，所以搬進來了；
  `paths.ts` 的 `prepareStorage()` 負責搬家。
- 建立在 enil 之上的應用（監看聊天室、自動通報之類）放在別的 repo，不在這裡。
  enil 是逆向的 LINE 客戶端，介面只有兩個：讀 `archive.jsonl`、要送訊息時呼叫
  `cli.ts send`。
- systemd unit：`enil.service`，裡面的 `@REPO@` 是佔位符（安裝時 `sed` 換成 `$PWD`）。
  不要改回寫死路徑 —— repo 放哪都該能跑，WorkingDirectory 只是為了找 `daemon.ts`
  與 `node_modules`，憑證與狀態都不看 CWD。
- omarchy 外掛：獨立 repo [omarchy-line](https://github.com/Unayung/omarchy-line)，
  安裝後就在 `~/.config/omarchy/plugins/io.github.unayung.line/`（那個目錄本身是 git repo）
- agent 用的說明：`SKILL.md`，同一份也放在 `~/.claude/skills/line/`
