//! enil-tui —— 在終端機用 LINE。
//!
//! 這支程式不碰 LINE 協定：它只是 enil daemon 的消費者，
//! 走 unix socket 收發一行一個 JSON。跟 omarchy 外掛是同一個契約。
//!
//! 版面：左邊聊天室清單、右邊訊息、底下輸入列。
//! 網路請求都丟到背景執行緒，UI 執行緒不阻塞 —— daemon 抓歷史要跟 LINE
//! 來回一趟，同步做會整個卡住。

mod archive;
mod client;
mod model;

use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use model::{Chat, Msg};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Wrap,
};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::{Protocol, StatefulProtocol};
use ratatui_image::{Resize, StatefulImage};
use std::collections::{HashMap, HashSet};
use serde_json::Value;
use std::io::{stdout, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

enum Update {
    Chats(Vec<Chat>),
    Messages(Vec<Msg>),
    Older(Vec<Msg>),
    /// daemon 推來的新訊息（chat mid、訊息本身）。
    Incoming(String, Msg),
    /// 其他推送：已讀、收回、表情、編輯、在手機上讀了（事件名稱, 資料）。
    Event(String, Value),
    /// 群組成員載入好了（聊天室, [(mid, 名稱)]）。
    Members(String, Vec<(String, String)>),
    /// 原檔下載好了，交給系統開。
    Downloaded(String),
    /// 語音下載好了，播放。
    Play(String),
    /// 推送連線接上／斷了。
    Online(bool),
    /// 清單游標所在聊天室的預覽（mid、訊息）。
    Preview(String, Vec<Msg>),
    /// daemon 有沒有登入 LINE。連得上 socket 不代表有登入。
    Session(bool),
    /// 全部的聊天室（搜尋用；平常的清單只有最近 60 個）。
    AllChats(Vec<Chat>),
    Sent,
    Error(String),
}

/// Ctrl-S：搜尋訊息存檔。
struct MsgSearch {
    query: String,
    pick: usize,
    all: Vec<archive::Hit>,
}

#[derive(Default)]
struct Search {
    query: String,
    /// 結果裡選中第幾筆。
    pick: usize,
}

#[derive(Default)]
struct PathPrompt {
    text: String,
    /// 目前這個前綴補得出來的候選。只在內容變動時重算（要讀目錄）。
    matches: Vec<String>,
}

impl PathPrompt {
    fn set(&mut self, text: String) {
        self.matches = complete_path(&text).1;
        self.text = text;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Sidebar {
    Full,
    /// 一行一筆，只有名稱與未讀。
    Names,
    /// 不顯示：跟窄視窗一樣一次只看一邊，對話佔滿整個視窗，Esc 回清單。
    Hidden,
}

fn vip_path() -> std::path::PathBuf {
    client::state_dir().join("vip")
}

fn load_vip() -> HashSet<String> {
    std::fs::read_to_string(vip_path())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

struct App {
    chats: Vec<Chat>,
    selected: usize,
    msgs: Vec<Msg>,
    scroll: u16,
    input: String,
    status: String,
    me: String,
    open: Option<String>,
    busy: bool,
    /// 終端機視窗有沒有焦點。決定要不要跳桌面通知。
    focused: bool,
    /// 推送那條連線通不通（daemon 還在不在）。
    online: bool,
    /// daemon 活著但沒登入 LINE（重啟後憑證失效、被踢下線）。主迴圈看到就切去登入畫面。
    needs_login: bool,
    /// 開著的聊天室的名稱。另外記一份：從搜尋開的聊天室不一定在 chats（最近 60 個）裡。
    open_name: String,
    /// `/` 搜尋。None 是沒在搜。
    search: Option<Search>,
    msg_search: Option<MsgSearch>,
    /// 開著的聊天室有已讀／表情／編輯的推送：過一下再重讀（一次把一陣子的變化收齊，
    /// 不要大群組裡每有人讀一次就打一次 LINE）。
    reload_at: Option<Instant>,
    /// 選取訊息後按 d 的第一下：記住是哪則，再按一次才真的收回。
    unsend_armed: Option<String>,
    /// 選取訊息後按 r：表情選單。
    react_picker: bool,
    /// 打 @ 之後的成員選單。
    mention: Option<Search>,
    /// 群組成員（打 @ 用），依聊天室快取。
    members: HashMap<String, Vec<(String, String)>>,
    /// 這次要送的訊息裡挑過的 @（mid, 名稱）。送出時才換算位置。
    pending_mentions: Vec<(String, String)>,
    /// 正在播的語音（mpv）。再按一次同一則就停。
    player: Option<std::process::Child>,
    /// 搜尋的範圍：全部聊天室。第一次按 `/` 才跟 daemon 要，之後留著用。
    all_chats: Vec<Chat>,
    /// 按鍵說明開著。
    help: bool,
    /// 說明捲到第幾列。小螢幕（14 吋筆電）放不下整份說明時用。
    help_scroll: u16,
    /// 上次畫說明時最多能捲幾列。畫的時候才知道（看終端機多高），按鍵處理拿它來夾限。
    help_max: std::cell::Cell<u16>,
    /// 掛著等送出的附件（本機路徑）。Enter 送出，Esc 或在空的輸入框按 Backspace 取消。
    attach: Option<String>,
    /// Ctrl-F 的路徑輸入框。None 是沒開。
    path_prompt: Option<PathPrompt>,
    /// 開著聊天室時，使用者用 ↑↓／滾輪動了左邊清單。這時右邊預覽清單上選中的那個
    /// （見 peeking）；Enter 進去、Esc 回到開著的那個。
    peek: bool,
    /// 各聊天室打到一半的字。離開聊天室時收起來，回來時放回輸入框。只在記憶體裡。
    drafts: HashMap<String, String>,
    /// 預覽抓過的訊息，依聊天室。同一個聊天室回頭再看就不再問 LINE。
    /// 不會過期：daemon 會推送每一則新訊息，收到時把那個聊天室丟掉就好（見 Incoming）。
    peek_cache: HashMap<String, Vec<Msg>>,
    /// 右邊目前預覽的是哪個聊天室（沒開聊天室時，跟著清單游標）。None = 還沒抓。
    preview: Option<String>,
    /// 上一輪看到的游標所在聊天室，與它從什麼時候開始停在那裡。停下來一會兒才抓預覽，
    /// 快速掃過時不必每個都打一次 LINE。
    preview_seen: Option<String>,
    preview_moved: Instant,
    /// 視窗窄到只能顯示一欄（draw 每一幀更新）。
    narrow: bool,
    /// Ctrl-B 輪流切：展開 → 只有名稱 → 隱藏（對話最大化）。
    sidebar: Sidebar,
    /// VIP 聊天室的 mid。置頂、通知不會自己消失、未讀永遠顯示在狀態列。存在 state 目錄的
    /// `vip` 檔（一行一個 mid）—— state 目錄是跟著帳號走的，mid 也是。
    vip: HashSet<String>,
    theme: Theme,
    tx: Sender<Update>,
    // 上次繪製的版面。滑鼠事件只有座標，要靠這個判斷點到哪一區。
    list_area: Rect,
    msg_area: Rect,
    list_offset: usize,
    /// 上次畫出來的每則訊息佔哪一塊（區域、msgs 索引）。點擊要靠它對回訊息。
    msg_rects: Vec<(Rect, usize)>,
    /// Tab 選中的訊息（msgs 索引）。Enter 開啟它的附件或連結。
    cursor: Option<usize>,
    /// 終端機支援的圖片協定（ghostty→kitty、foot→sixel，偵測不到就半格字元）。
    picker: Picker,
    /// 解碼後的圖片，依訊息 id 快取 —— 每幀重新解碼會很慢。
    /// None 是「解不開」：也要記住，不然每一幀都會重讀一次檔案。
    images: HashMap<String, Option<StatefulProtocol>>,
    /// 大頭貼，依發話者 mid 快取。固定 4×2 格，同一張會在畫面上出現很多次。
    avatars: HashMap<String, Option<Protocol>>,
}

impl App {
    /// 清單裡一個聊天室佔幾列。滑鼠座標換算成第幾筆要用。
    fn list_item_rows(&self) -> u16 {
        if self.sidebar == Sidebar::Names && !self.narrow { 1 } else { 2 }
    }

    fn spawn<F>(&self, f: F)
    where
        F: FnOnce() -> Update + Send + 'static,
    {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(f());
        });
    }

    fn load_chats(&mut self) {
        self.spawn(|| match client::chats(60) {
            Ok(v) => Update::Chats(model::parse_chats(&v)),
            Err(e) => Update::Error(e),
        });
    }

    /// 問 daemon 有沒有登入。連不上就當離線，那是另一回事（狀態列會顯示）。
    ///
    /// 順便回報「使用者現在正盯著哪個聊天室」：視窗有焦點而且開著的那個，否則是 null。
    /// daemon 靠這個決定 VIP 的訊息要不要彈系統通知 —— 正在看的不必彈。
    /// 所以開關聊天室、焦點變化時都要馬上叫一次，不能只靠定期的那一次。
    fn check_session(&self) {
        let viewing = if self.focused { self.open.clone() } else { None };
        self.spawn(move || match client::request(serde_json::json!({ "cmd": "state", "viewing": viewing })) {
            Ok(state) => Update::Session(logged_in_mid(&state).is_some()),
            Err(_) => Update::Online(false),
        });
    }

    /// VIP 排最前面（VIP 之間、其餘之間都還是照最近活動排）。
    /// selected 是索引，排完要指回原本那個聊天室。
    fn pin_vips(&mut self) {
        let selected = self.chats.get(self.selected).map(|c| c.mid.clone());
        let vip = &self.vip;
        self.chats.sort_by_key(|c| !vip.contains(&c.mid)); // 穩定排序，組內順序不變
        if let Some(i) = selected.and_then(|mid| self.chats.iter().position(|c| c.mid == mid)) {
            self.selected = i;
        }
    }

    /// 開著聊天室，但右邊正在預覽清單上選中的另一個。輸入框這時不收字：
    /// 它屬於開著的那個，畫面上卻是別人 —— 一個 Enter 就送錯人。
    fn peeking(&self) -> Option<&Chat> {
        let open = self.open.as_ref()?;
        let chat = self.chats.get(self.selected)?;
        (self.peek && !self.narrow && &chat.mid != open).then_some(chat)
    }

    /// Esc：不預覽了，回到開著的那個；清單游標也移回它。
    fn end_peek(&mut self) {
        self.peek = false;
        if let Some(i) = self.open.as_ref().and_then(|mid| self.chats.iter().position(|c| &c.mid == mid)) {
            self.selected = i;
        }
    }

    /// Ctrl-P：把畫面上的那個聊天室（開著的，或正在預覽的）設成 VIP，或取消。
    fn toggle_vip(&mut self) {
        let target = match (self.peeking(), &self.open) {
            (Some(c), _) => Some((c.mid.clone(), c.name.clone())),
            (None, Some(mid)) => Some((mid.clone(), self.open_name.clone())),
            (None, None) => self.chats.get(self.selected).map(|c| (c.mid.clone(), c.name.clone())),
        };
        let Some((mid, name)) = target else { return };
        self.status = if self.vip.remove(&mid) {
            format!("已取消 VIP：{name}")
        } else {
            self.vip.insert(mid);
            format!("★ {name} 設為 VIP")
        };
        let mut lines: Vec<&str> = self.vip.iter().map(String::as_str).collect();
        lines.sort();
        if let Err(e) = std::fs::write(vip_path(), lines.join("\n") + "\n") {
            self.status = format!("VIP 名單存不了：{e}");
        }
        self.pin_vips();
    }

    /// 主迴圈每一輪呼叫：游標停在一個還沒預覽的聊天室上就抓它的訊息。
    ///
    /// 兩種情況：沒開聊天室（訊息直接放進 msgs），或開著聊天室但在預覽別的（peeking ——
    /// msgs 是開著那個的，不能動，畫面直接讀 peek_cache）。
    /// 已讀、收回、表情、編輯、在手機上讀了。
    fn on_event(&mut self, kind: &str, data: &Value) {
        let field = |k: &str| data.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let chat = field("chat");
        // 預覽快取是那個聊天室舊的樣子了。
        self.peek_cache.remove(&chat);
        if self.preview.as_deref() == Some(chat.as_str()) && self.open.is_none() {
            self.preview = None; // 清單預覽中的那個：重抓
        }
        let is_open = self.open.as_deref() == Some(chat.as_str());
        match kind {
            "chat-read" => {
                if let Some(c) = self.chats.iter_mut().find(|c| c.mid == chat) {
                    c.unread = 0;
                }
            }
            // 收回：馬上在畫面上換掉，不等重讀 —— 收回常常是「送錯了」，越快消失越好。
            "unsend" => {
                let id = field("messageId");
                if let Some(m) = self.msgs.iter_mut().find(|m| m.id == id) {
                    m.text = "[已收回]".into();
                    m.kind = "UNSENT".into();
                    m.media_path = None;
                    m.has_media = false;
                    m.link = None;
                    m.reactions.clear();
                }
            }
            // 已讀、表情、編輯：開著的那個過一下重讀，一次收齊。
            _ if is_open => {
                self.reload_at.get_or_insert_with(|| Instant::now() + Duration::from_millis(1500));
            }
            _ => {}
        }
    }

    fn update_preview(&mut self) {
        if self.narrow {
            return; // 單欄時右邊根本不顯示
        }
        if self.open.is_some() {
            let Some(mid) = self.peeking().map(|c| c.mid.clone()) else { return };
            if self.peek_cache.contains_key(&mid) || self.preview.as_deref() == Some(mid.as_str()) {
                return; // 已經有了，或正在抓
            }
            if self.preview_seen.as_deref() != Some(mid.as_str()) {
                self.preview_seen = Some(mid);
                self.preview_moved = Instant::now();
                return;
            }
            if self.preview_moved.elapsed() < Duration::from_millis(200) {
                return;
            }
            self.preview = Some(mid.clone());
            let me = self.me.clone();
            self.spawn(move || match client::peek(&mid, 30) {
                Ok(v) => Update::Preview(mid, model::parse_msgs(&v, &me)),
                Err(e) => Update::Error(e),
            });
            return;
        }
        let Some(mid) = self.chats.get(self.selected).map(|c| c.mid.clone()) else { return };
        if self.preview.as_deref() == Some(mid.as_str()) {
            return;
        }
        // 看過的直接用，不必等游標停下來。
        if let Some(cached) = self.peek_cache.get(&mid) {
            self.msgs = cached.clone();
            self.scroll = 0;
            self.busy = false;
            self.preview = Some(mid);
            return;
        }
        if self.preview_seen.as_deref() != Some(mid.as_str()) {
            self.preview_seen = Some(mid);
            self.preview_moved = Instant::now();
            return;
        }
        if self.preview_moved.elapsed() < Duration::from_millis(200) {
            return;
        }
        // 先清掉上一個的訊息：抓的這段時間右邊顯示「載入中」，不要掛著別人的對話。
        self.preview = Some(mid.clone());
        self.msgs.clear();
        self.busy = true;
        let me = self.me.clone();
        self.spawn(move || match client::peek(&mid, 30) {
            Ok(v) => Update::Preview(mid, model::parse_msgs(&v, &me)),
            Err(e) => Update::Error(e),
        });
    }

    /// 別的程式要求打開某個聊天室（VIP 彈窗的「前往回覆」）：state 目錄裡的 open-request，
    /// 內容是「mid<Tab>名稱」。讀到就刪，免得下次啟動又開一次。
    fn take_open_request(&mut self) {
        let path = client::state_dir().join("open-request");
        let Ok(raw) = std::fs::read_to_string(&path) else { return };
        let _ = std::fs::remove_file(&path);
        let (mid, name) = raw.trim().split_once('\t').unwrap_or((raw.trim(), ""));
        if mid.is_empty() {
            return;
        }
        let name = self.chats.iter().find(|c| c.mid == mid).map_or(name.to_string(), |c| c.name.clone());
        self.open_chat(mid.to_string(), name);
    }

    fn open_selected(&mut self) {
        if let Some(chat) = self.chats.get(self.selected) {
            self.open_chat(chat.mid.clone(), chat.name.clone());
        }
    }

    /// 把輸入框收進目前聊天室的草稿，清空輸入框。離開聊天室（切到別的、回清單）時呼叫。
    fn stash_draft(&mut self) {
        let text = std::mem::take(&mut self.input);
        if let Some(mid) = &self.open {
            if text.trim().is_empty() {
                self.drafts.remove(mid);
            } else {
                self.drafts.insert(mid.clone(), text);
            }
        }
    }

    fn open_chat(&mut self, mid: String, name: String) {
        self.stash_draft();
        self.peek = false;
        self.preview = None;
        // 清單游標跟著開的那個走（從搜尋開的也是），不然下一個 ↑↓ 會從奇怪的地方開始。
        if let Some(i) = self.chats.iter().position(|c| c.mid == mid) {
            self.selected = i;
        }
        self.input = self.drafts.remove(&mid).unwrap_or_default();
        let me = self.me.clone();
        self.open = Some(mid.clone());
        self.msgs.clear();
        self.cursor = None;
        // 附件是掛給「剛才那個聊天室」的。帶到下一個聊天室，一個 Enter 就送錯人。
        self.attach = None;
        self.pending_mentions.clear();
        self.mention = None;
        self.unsend_armed = None;
        self.react_picker = false;
        self.reload_at = None;
        self.scroll = 0;
        self.busy = true;
        self.status = format!("載入 {name}…");
        self.open_name = name;
        // 進聊天室就是讀了：下面的 history 會請 daemon 標已讀，這裡先把清單上的數字清掉，
        // 不然要等下一次定期重整（最長一分鐘）徽章才會消失。
        if let Some(chat) = self.chats.iter_mut().find(|c| c.mid == mid) {
            chat.unread = 0;
        }
        self.check_session();
        self.spawn(move || match client::history(&mid, 40, None) {
            Ok(v) => Update::Messages(model::parse_msgs(&v, &me)),
            Err(e) => Update::Error(e),
        });
    }

    /// `/`：開搜尋。先用手上有的，同時跟 daemon 要全部聊天室 —— 那要翻完所有分頁，
    /// 所以只在開搜尋時要一次，打字過濾都在本機做。
    fn start_search(&mut self) {
        self.search = Some(Search::default());
        self.spawn(|| match client::chats(5000) {
            Ok(v) => Update::AllChats(model::parse_chats(&v)),
            Err(e) => Update::Error(e),
        });
    }

    /// Ctrl-S：開訊息搜尋。存檔整份讀進來；聊天室名稱要用完整清單才查得到，順便跟 daemon 要。
    fn start_msg_search(&mut self) {
        let all = archive::load(&client::state_dir().join("archive.jsonl"));
        self.msg_search = Some(MsgSearch { query: String::new(), pick: 0, all });
        if self.all_chats.is_empty() {
            self.spawn(|| match client::chats(5000) {
                Ok(v) => Update::AllChats(model::parse_chats(&v)),
                Err(e) => Update::Error(e),
            });
        }
    }

    /// 聊天室名稱。存檔裡只有 mid；一對一的找不到時用發話者名字（就是對方）。
    fn chat_name(&self, mid: &str) -> Option<String> {
        self.chats.iter().chain(self.all_chats.iter()).find(|c| c.mid == mid).map(|c| c.name.clone())
    }

    /// 搜尋結果：名稱包含關鍵字（不分大小寫），維持「最近有動靜的在前」。
    fn search_hits(&self) -> Vec<&Chat> {
        let Some(search) = &self.search else { return vec![] };
        let q = search.query.to_lowercase();
        let pool = if self.all_chats.is_empty() { &self.chats } else { &self.all_chats };
        pool.iter().filter(|c| c.name.to_lowercase().contains(&q)).collect()
    }

    /// 重讀開著的聊天室最新一頁（會標已讀）。
    fn reload_open(&mut self) {
        let Some(mid) = self.open.clone() else { return };
        let me = self.me.clone();
        self.spawn(move || match client::history(&mid, 40, None) {
            Ok(v) => Update::Messages(model::parse_msgs(&v, &me)),
            Err(e) => Update::Error(e),
        });
    }

    fn load_older(&mut self) {
        let (Some(mid), Some(first)) = (self.open.clone(), self.msgs.first().cloned()) else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        let me = self.me.clone();
        self.spawn(move || match client::history(&mid, 40, Some(&first.id)) {
            Ok(v) => Update::Older(model::parse_msgs(&v, &me)),
            Err(e) => Update::Error(e),
        });
    }

    /// scroll 是「從最新往回跳過幾則」。往上滾到底時要停在最舊那則，
    /// 不然 end 會變 0、整個訊息區空白。
    fn clamp_scroll(&mut self) {
        let max = self.msgs.len().saturating_sub(1) as u16;
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// 捲動之後呼叫：夾限，並在接近最舊時預先往前抓一頁 ——
    /// 否則捲到頂就停住，使用者得記得按 g 才有更多。
    fn on_scrolled(&mut self) {
        self.clamp_scroll();
        if (self.scroll as usize) + 5 >= self.msgs.len() {
            self.load_older();
        }
    }

    /// Tab／Shift-Tab：在可開啟的訊息之間移動。從最新的開始往舊的走，走到頭就取消選取。
    fn move_cursor(&mut self, older: bool) {
        let len = self.msgs.len();
        // 每一則都能選：可以開的（↗）Enter 開，任何一則都能回表情，自己的能收回。
        let ok = |i: &usize| self.msgs[*i].kind != "UNSENT";
        self.cursor = if older {
            (0..self.cursor.unwrap_or(len)).rev().find(ok)
        } else {
            (self.cursor.map_or(len, |c| c + 1)..len).find(ok)
        };
        // 選到畫面外的就捲過去，把它放在最底下。
        if let Some(i) = self.cursor {
            if !self.msg_rects.iter().any(|(_, idx)| *idx == i) {
                self.scroll = (len - 1 - i) as u16;
            }
        }
    }

    /// 開啟一則訊息：有原檔的下載後交給系統，否則開它的連結。
    fn open_msg(&mut self, i: usize) {
        let Some(m) = self.msgs.get(i) else { return };
        let audio = m.kind == "AUDIO";
        // 語音正在播：這一下是「停」。
        if audio && self.player.as_mut().is_some_and(|p| matches!(p.try_wait(), Ok(None))) {
            if let Some(mut p) = self.player.take() {
                let _ = p.kill();
                let _ = p.wait();
            }
            self.status = "已停止".into();
            return;
        }
        if m.has_media {
            let id = m.id.clone();
            self.status = "下載中…".into();
            self.spawn(move || {
                match client::download(&id)
                    .and_then(|v| v.get("path").and_then(Value::as_str).map(str::to_string).ok_or("daemon 沒回路徑".into()))
                {
                    Ok(path) if audio => Update::Play(path),
                    Ok(path) => Update::Downloaded(path),
                    Err(e) => Update::Error(e),
                }
            });
        } else if let Some(url) = m.link.clone() {
            self.status = system_open(&url);
        }
    }

    /// 選中的那則：r 表情、d 收回。
    fn react_selected(&mut self, kind: &'static str) {
        let Some(m) = self.cursor.and_then(|i| self.msgs.get(i)) else { return };
        let id = m.id.clone();
        self.react_picker = false;
        self.status = "送出表情…".into();
        self.spawn(move || match client::react(&id, kind) {
            Ok(_) => Update::Error(String::new()), // 成功不用說什麼；畫面靠推送回來的事件重讀
            Err(e) => Update::Error(format!("表情送不出去：{e}")),
        });
    }

    fn unsend_selected(&mut self) {
        let Some(m) = self.cursor.and_then(|i| self.msgs.get(i)) else { return };
        if !m.mine {
            self.status = "只能收回自己的訊息".into();
            return;
        }
        let id = m.id.clone();
        // 收回是送不回來的：第一下只提示，同一則再按一次才送。
        if self.unsend_armed.as_deref() != Some(id.as_str()) {
            self.unsend_armed = Some(id);
            self.status = "再按一次 d 收回這則（LINE 只能收回 24 小時內的）".into();
            return;
        }
        self.unsend_armed = None;
        self.status = "收回中…".into();
        self.spawn(move || match client::unsend(&id) {
            Ok(_) => Update::Error(String::new()),
            Err(e) => Update::Error(format!("收回失敗：{e}")),
        });
    }

    /// 打 @：開成員選單。群組的成員第一次用時才跟 daemon 要。
    fn start_mention(&mut self) {
        let Some(chat) = self.open.clone() else { return };
        self.mention = Some(Search::default());
        if !self.members.contains_key(&chat) {
            self.spawn(move || match client::members(&chat) {
                Ok(v) => Update::Members(
                    chat,
                    v.as_array()
                        .map(|a| {
                            a.iter()
                                .map(|m| {
                                    let s = |k| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                                    (s("mid"), s("name"))
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                ),
                Err(e) => Update::Error(format!("成員載入失敗：{e}")),
            });
        }
    }

    /// 成員選單的候選：名稱包含已經打的字。
    fn mention_hits(&self) -> Vec<(String, String)> {
        let (Some(m), Some(chat)) = (&self.mention, &self.open) else { return vec![] };
        let q = m.query.to_lowercase();
        self.members
            .get(chat)
            .map(|list| list.iter().filter(|(_, n)| n.to_lowercase().contains(&q)).cloned().collect())
            .unwrap_or_default()
    }

    /// 在背景用 mpv 播語音（不開視窗）。沒有 mpv 就交給系統的預設程式。
    fn play(&mut self, path: &str) {
        if let Some(mut p) = self.player.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
        let spawned = Command::new("mpv")
            .args(["--no-video", "--really-quiet", "--"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawned {
            Ok(child) => {
                self.player = Some(child);
                self.status = "▶ 播放中（再按一次停止）".into();
            }
            Err(_) => self.status = system_open(path),
        }
    }

    fn send_input(&mut self) {
        let text = self.input.trim().to_string();
        let Some(mid) = self.open.clone() else { return };
        let attach = self.attach.take();
        if text.is_empty() && attach.is_none() {
            return;
        }
        self.input.clear();
        let mentions = mention_spans(&text, &std::mem::take(&mut self.pending_mentions));
        self.status = "傳送中…".into();
        // 附件與文字分開送，附件先：LINE 沒有「圖片加說明」這種單一訊息。
        self.spawn(move || {
            let sent = attach
                .map_or(Ok(()), |path| client::send_file(&mid, &path).map(|_| ()))
                .and_then(|()| if text.is_empty() { Ok(()) } else { client::send(&mid, &text, mentions).map(|_| ()) });
            match sent {
                Ok(()) => Update::Sent,
                Err(e) => Update::Error(e),
            }
        });
    }

    /// 把一個檔案掛成附件。給的可以是路徑、~ 開頭的路徑、或 file:// 網址。
    fn attach_file(&mut self, raw: &str) -> bool {
        let path = local_path(raw);
        if Path::new(&path).is_file() {
            self.status = "已附加，Enter 送出".into();
            self.attach = Some(path);
            true
        } else {
            self.status = format!("找不到檔案：{}", fit(raw.trim(), 50));
            false
        }
    }

    /// Ctrl-V：依序看剪貼簿裡有沒有「複製的檔案」、圖片、文字。
    fn paste(&mut self) {
        match clipboard() {
            Clip::File(path) => {
                self.attach_file(&path);
            }
            Clip::Image(path) => {
                self.status = "已附加剪貼簿的圖片，Enter 送出".into();
                self.attach = Some(path);
            }
            Clip::Text(text) => self.paste_text(&text),
            Clip::Empty => self.status = "剪貼簿是空的（或讀不到，需要 wl-paste）".into(),
        }
    }

    /// 貼上文字。如果整段就是一個存在的檔案路徑（把檔案拖進終端機就是貼路徑），當成附件。
    fn paste_text(&mut self, text: &str) {
        let one_line = !text.trim().contains('\n');
        if one_line && Path::new(&local_path(text)).is_file() {
            self.attach_file(text);
        } else {
            self.input.push_str(text.trim_end_matches('\n'));
        }
    }

    /// 終端機的貼上（bracketed paste）。浮層開著時貼進浮層的輸入框。
    fn on_paste(&mut self, text: String) {
        // 終端機貼上時常把換行送成 \r（跟按 Enter 同一個字元）。轉回來，不然送出去的
        // 訊息裡是 \r，在 LINE 上不會換行。
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let first_line = text.lines().next().unwrap_or("").to_string();
        if let Some(prompt) = self.path_prompt.as_mut() {
            let joined = format!("{}{}", prompt.text, first_line.trim());
            prompt.set(joined);
        } else if let Some(ms) = self.msg_search.as_mut() {
            ms.query.push_str(&first_line);
            ms.pick = 0;
        } else if let Some(search) = self.search.as_mut() {
            search.query.push_str(&first_line);
            search.pick = 0;
        } else if self.open.is_some() {
            // 剪貼簿只有圖片時，終端機貼出來的是空的 —— 那就自己去剪貼簿拿。
            if text.trim().is_empty() { self.paste() } else { self.paste_text(&text) }
        }
    }

    fn apply(&mut self, u: Update) {
        match u {
            Update::Chats(c) => {
                // 選取要跟著聊天室走，不是跟著位置。清單每分鐘重整一次，期間誰收到新訊息
                // 誰就排到前面 —— 只記位置的話，選取會自己跳到別的聊天室，右邊的預覽也跟著換
                // （「沒人碰，TUI 自己換了聊天室」就是這個）。所以先記住是哪一個，換完再找回來。
                let keep = self.chats.get(self.selected).map(|c| c.mid.clone());
                self.chats = c;
                if let Some(i) = keep.and_then(|mid| self.chats.iter().position(|c| c.mid == mid)) {
                    self.selected = i;
                } else if self.selected >= self.chats.len() {
                    self.selected = self.chats.len().saturating_sub(1);
                }
                self.pin_vips();
                self.status.clear();
            }
            Update::Downloaded(path) => self.status = system_open(&path),
            Update::Members(chat, list) => {
                self.members.insert(chat, list);
            }
            Update::Event(kind, data) => self.on_event(&kind, &data),
            Update::Play(path) => self.play(&path),
            Update::Online(up) => {
                self.online = up;
                // 重新接上的可能是剛重啟、還沒登入的 daemon —— 馬上問一次。
                if up {
                    self.check_session();
                }
            }
            Update::Session(logged_in) => self.needs_login = !logged_in,
            Update::AllChats(c) => self.all_chats = c,
            // 回來時游標可能已經移走、或已經開了聊天室 —— 那就是過期的，丟掉。
            Update::Preview(mid, m) => {
                // 游標已經移走也照樣留著：等一下回來看就不用再抓。
                self.peek_cache.insert(mid.clone(), m.clone());
                let still = self.open.is_none() && self.chats.get(self.selected).is_some_and(|c| c.mid == mid);
                if still {
                    self.msgs = m;
                    self.scroll = 0;
                    self.busy = false;
                }
            }
            Update::Messages(m) => {
                self.msgs = m;
                self.cursor = None; // 索引是舊清單的
                self.busy = false;
                self.status.clear();
            }
            Update::Older(m) => {
                self.busy = false;
                // before 會把錨點那則一起回傳，去重才不會多一筆。
                let known: Vec<String> = self.msgs.iter().map(|x| x.id.clone()).collect();
                let fresh: Vec<Msg> = m.into_iter().filter(|x| !known.contains(&x.id)).collect();
                if fresh.is_empty() {
                    self.status = "沒有更舊的了".into();
                } else {
                    // 前面多了幾則，索引要跟著往後移。
                    self.cursor = self.cursor.map(|c| c + fresh.len());
                    let mut merged = fresh;
                    merged.extend(self.msgs.drain(..));
                    self.msgs = merged;
                }
            }
            Update::Incoming(chat, msg) => {
                let is_open = self.open.as_deref() == Some(chat.as_str());
                let mut name = chat.clone();
                match self.chats.iter().position(|c| c.mid == chat) {
                    Some(i) => {
                        // 移到最上面。selected 是索引，清單動了要跟著修，
                        // 不然游標會默默換到另一個聊天室。
                        let mut c = self.chats.remove(i);
                        c.last_text = msg.text.replace('\n', " ");
                        c.last_from = msg.from_name.clone();
                        c.last_time = msg.time;
                        if !msg.mine && !is_open {
                            c.unread += 1;
                        }
                        name = c.name.clone();
                        self.chats.insert(0, c);
                        if i == self.selected {
                            self.selected = 0;
                        } else if i > self.selected {
                            self.selected += 1;
                        }
                        self.pin_vips();
                    }
                    // 不在清單裡（新聊天室，或排在 60 名之外）—— 整個重抓。
                    None => self.load_chats(),
                }
                // VIP 的提醒是 daemon 發的（不會自己消失的系統通知），這裡再發就重複了。
                self.peek_cache.remove(&chat);
                if self.preview.as_deref() == Some(chat.as_str()) {
                    self.preview = None; // 下一輪 update_preview 會重抓
                }
                if should_notify(&msg, is_open, self.focused) && !self.vip.contains(&chat) {
                    notify(&name, &msg);
                }
                if is_open && self.scroll == 0 {
                    // 重讀而不是直接 append：推送的訊息沒有縮圖路徑，重讀才有，也順便標已讀。
                    self.reload_open();
                } else if is_open && !self.msgs.iter().any(|m| m.id == msg.id) {
                    // 正在往回看歷史：重讀會把載入的舊訊息丟掉，所以只接在後面，
                    // scroll 加一讓畫面停在原處。
                    self.msgs.push(msg);
                    self.scroll += 1;
                }
            }
            Update::Sent => {
                self.status.clear();
                // 推送也會帶回自己送的那則，這裡先丟，免得推送晚到時回清單看到舊的。
                if let Some(mid) = &self.open {
                    self.peek_cache.remove(mid);
                }
                // 自己送的訊息也會從 listen() 回來，但重讀一次不用等那一拍。
                self.reload_open();
            }
            Update::Error(e) => {
                self.busy = false;
                self.status = e;
            }
        }
    }
}

/// 依字元數截斷（不是 byte）—— 中文一個字算一個，切在 byte 邊界會 panic。
/// 送出時把挑過的 @ 換算成 LINE 要的位置（UTF-16 索引）。挑了卻在送出前刪掉的就不算；
/// 同一個人提及兩次就各算一次（依序往後找）。
fn mention_spans(text: &str, picked: &[(String, String)]) -> Value {
    let mut from = 0;
    let mut spans = Vec::new();
    for (mid, name) in picked {
        let needle = format!("@{name}");
        let Some(at) = text[from..].find(&needle).map(|i| i + from) else { continue };
        let start = text[..at].encode_utf16().count();
        spans.push(serde_json::json!({ "mid": mid, "start": start, "end": start + needle.encode_utf16().count() }));
        from = at + needle.len();
    }
    Value::Array(spans)
}

/// 剪貼簿裡的東西。
enum Clip {
    /// 在檔案管理員裡複製的檔案（text/uri-list 的第一個）。
    File(String),
    /// 圖片內容，已經存成檔案。
    Image(String),
    Text(String),
    Empty,
}

/// 讀剪貼簿。終端機的貼上只會送文字，圖片與「複製的檔案」要自己問。
/// ponytail: 只支援 Wayland（wl-paste）。X11 要 xclip、macOS 要 pngpaste，用到再加；
/// 複製了多個檔案時只取第一個。
fn clipboard() -> Clip {
    let run = |args: &[&str]| {
        Command::new("wl-paste").args(args).stderr(Stdio::null()).output().ok().map(|o| o.stdout)
    };
    let types = String::from_utf8_lossy(&run(&["--list-types"]).unwrap_or_default()).into_owned();
    let has = |mime: &str| types.lines().any(|t| t == mime);

    if has("text/uri-list") {
        let list = String::from_utf8_lossy(&run(&["--type", "text/uri-list"]).unwrap_or_default()).into_owned();
        if let Some(uri) = list.lines().find(|l| l.starts_with("file://")) {
            return Clip::File(uri.trim().to_string());
        }
    }
    for (mime, ext) in [("image/png", "png"), ("image/jpeg", "jpg"), ("image/webp", "webp"), ("image/gif", "gif")] {
        if !has(mime) {
            continue;
        }
        let bytes = run(&["--type", mime]).unwrap_or_default();
        let dir = client::state_dir().join("media");
        let path = dir.join(format!("paste-{}.{ext}", now_ms()));
        if !bytes.is_empty() && std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&path, bytes).is_ok() {
            // 跟 daemon 的媒體快取一樣只給自己讀。
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            return Clip::Image(path.to_string_lossy().into_owned());
        }
    }
    match run(&["--no-newline", "--type", "text"]) {
        Some(bytes) if !bytes.is_empty() => Clip::Text(String::from_utf8_lossy(&bytes).into_owned()),
        _ => Clip::Empty,
    }
}

/// 把使用者給的東西變成本機路徑：去掉 file://（並解開 %XX，中文檔名在 URI 裡都是編碼過的）、
/// 展開開頭的 ~。
fn local_path(raw: &str) -> String {
    let raw = raw.trim();
    let path = match raw.strip_prefix("file://") {
        Some(rest) => percent_decode(rest),
        None => raw.to_string(),
    };
    match path.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => path,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 路徑補完：回傳（補到共同前綴之後的輸入, 候選清單）。目錄的候選帶結尾的 /。
/// 輸入裡的 ~ 原樣保留，只在查目錄時展開。
fn complete_path(input: &str) -> (String, Vec<String>) {
    let split = input.rfind('/').map_or(0, |i| i + 1);
    let (base, prefix) = input.split_at(split);
    let dir = if base.is_empty() { ".".to_string() } else { local_path(base) };
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // 隱藏檔要自己打出那個點才列。
            let wanted = name.starts_with(prefix) && (prefix.starts_with('.') || !name.starts_with('.'));
            wanted.then(|| if e.path().is_dir() { name + "/" } else { name })
        })
        .collect();
    names.sort();
    let common = names.iter().skip(1).fold(names.first().cloned().unwrap_or_default(), |acc, n| {
        acc.chars().zip(n.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect()
    });
    let completed = if common.len() > prefix.len() { format!("{base}{common}") } else { input.to_string() };
    (completed, names)
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / 1048576.0),
    }
}

/// 把檔案或網址交給系統的預設程式，回傳要顯示在狀態列的字。
/// 輸出全部丟掉：xdg-open 往終端機寫任何東西都會把畫面弄花。
fn system_open(target: &str) -> String {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    match Command::new(opener)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // 收屍，不然每開一次留一個殭屍行程。
            thread::spawn(move || {
                let _ = child.wait();
            });
            format!("已開啟 {}", fit(target, 70))
        }
        Err(e) => format!("{opener} 失敗：{e}"),
    }
}

/// 換滑鼠指標的形狀（OSC 22，名稱同 CSS cursor）。ghostty、kitty、foot 認得，
/// 不認得的終端機會安靜忽略。
fn set_pointer(shape: &str) {
    let mut out = stdout();
    let _ = write!(out, "\x1b]22;{shape}\x1b\\");
    let _ = out.flush();
}

/// 折行後佔幾列。ratatui 的 Wrap 以字詞為單位折，寬字元塞不下時也會提早換行，
/// ponytail: 這裡用「寬度 − 1」當每列容量留一點餘裕，不重做一遍它的折行。
/// 英文長句仍可能少算一列，最後一行會被切掉；真的常遇到再改用 Paragraph::line_count。
fn wrapped_rows(text: &str, width: u16) -> u16 {
    let cap = width.saturating_sub(1).max(1) as usize;
    text.lines()
        .map(|l| Line::from(l).width().div_ceil(cap).max(1) as u16)
        .sum::<u16>()
        .max(1)
}

/// 這則新訊息要不要跳桌面通知。
fn should_notify(msg: &Msg, is_open: bool, focused: bool) -> bool {
    // 自己發的（包含從手機發的）不通知；正盯著那個聊天室看也不通知。
    !msg.mine && !(focused && is_open)
}

/// 桌面通知：OSC 9，由終端機自己轉成系統通知，不用另外叫 notify-send。
/// ponytail: 只送 OSC 9（ghostty、WezTerm、iTerm2 認得）。kitty 要 OSC 99、foot 要
/// OSC 777，那些終端機上這行會被安靜忽略；真的要用再依 TERM 分流。
fn notify(chat: &str, msg: &Msg) {
    let who = if msg.from_name.is_empty() || msg.from_name == chat {
        chat.to_string()
    } else {
        format!("{chat}｜{}", msg.from_name)
    };
    // 訊息內文是別人寫的，直接塞進 escape sequence 等於讓對方對你的終端機下指令。
    // 控制字元全部拿掉（含 ESC 與 BEL，後者會提前結束這個 OSC）。
    let body: String = format!("{who}：{}", fit(&msg.text, 80))
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut out = stdout();
    let _ = write!(out, "\x1b]9;{body}\x07");
    let _ = out.flush();
}

/// 截到顯示寬度 max 以內（寬字元算兩格），放不下就補 …
fn fit(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > max {
            break;
        }
        out.push(c);
        used += w;
    }
    out + "…"
}

/// 配色。在 omarchy 上讀目前主題的 colors.toml，換主題時跟著換（見 refresh）；
/// 其他環境沒有那個檔，退回終端機調色盤的色號。
///
/// 發話者顏色（sender_color）與調暗一律用調色盤色號，不放在這裡：omarchy 換主題時會
/// 叫終端機重載調色盤，那些顏色本來就會自己變。這裡只管調色盤裡沒有的語意色。
struct Theme {
    /// 強調色：選取、未讀、焦點所在的面板、連結。
    accent: Color,
    /// 印在強調色底上的字。
    on_accent: Color,
    /// 選取列的底色。沒有主題檔時是 None，只靠色條與粗體標示。
    selection: Option<Color>,
    stamp: Option<SystemTime>,
}

impl Theme {
    fn path() -> std::path::PathBuf {
        client::state_dir().with_file_name("omarchy").join("current/theme/colors.toml")
    }

    fn load() -> Theme {
        let path = Theme::path();
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let mut theme = parse_theme(&std::fs::read_to_string(&path).unwrap_or_default());
        theme.stamp = stamp;
        theme
    }

    /// 主題檔換了就重讀。omarchy 換主題是把整個目錄刪掉再搬新的進來，中間有一瞬間
    /// 檔案不存在 —— 那時先維持原樣，不要閃回預設色。
    fn refresh(&mut self) {
        let stamp = std::fs::metadata(Theme::path()).and_then(|m| m.modified()).ok();
        if stamp.is_some() && stamp != self.stamp {
            *self = Theme::load();
        }
    }
}

/// colors.toml 是一行一個 `key = "#rrggbb"`。只取用得到的幾個鍵，不為此拉進 toml。
fn parse_theme(raw: &str) -> Theme {
    let get = |key: &str| {
        raw.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            let hex = v.trim().trim_matches('"').strip_prefix('#')?;
            let n = u32::from_str_radix(hex, 16).ok().filter(|_| hex.len() == 6)?;
            (k.trim() == key).then(|| Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
        })
    };
    Theme {
        accent: get("accent").or_else(|| get("color4")).unwrap_or(Color::Green),
        on_accent: get("background").unwrap_or(Color::Black),
        selection: get("selection"),
        stamp: None,
    }
}

/// 把網址標成底線＋強調色，看得出哪裡點得開。
fn rich(text: &str, accent: Color) -> Text<'static> {
    let link = Style::new().fg(accent).add_modifier(Modifier::UNDERLINED);
    text.lines()
        .map(|l| {
            let mut spans = Vec::new();
            let mut rest = l;
            while let Some(at) = [rest.find("https://"), rest.find("http://")].into_iter().flatten().min() {
                let end = rest[at..].find(char::is_whitespace).map_or(rest.len(), |e| at + e);
                spans.push(Span::raw(rest[..at].to_string()));
                spans.push(Span::styled(rest[at..end].to_string(), link));
                rest = &rest[end..];
            }
            spans.push(Span::raw(rest.to_string()));
            Line::from(spans)
        })
        .collect::<Vec<_>>()
        .into()
}

/// 發話者的顏色。同一個名字永遠同一色，群組裡才分得出誰是誰。
fn sender_color(name: &str) -> Color {
    const PALETTE: [Color; 8] = [
        Color::Cyan,
        Color::Magenta,
        Color::Yellow,
        Color::Blue,
        Color::LightRed,
        Color::LightCyan,
        Color::LightMagenta,
        Color::LightBlue,
    ];
    let h = name.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
    PALETTE[h % PALETTE.len()]
}

/// 視窗寬度在這以內就改成單欄，清單與對話二選一。
const NARROW_COLS: u16 = 50;

/// 大頭貼的寬度（格）。高度固定兩列，格子大約是 1:2，所以畫出來接近正方形。
const AVATAR_W: u16 = 4;

/// 畫一張大頭貼。同一張圖只解一次（依 mid 快取，訊息區與清單共用 —— 一對一聊天室的
/// mid 就是對方的 mid）。沒有照片就畫名字的第一個字，底色用 color。
#[allow(clippy::too_many_arguments)]
fn draw_avatar(
    f: &mut Frame,
    cache: &mut HashMap<String, Option<Protocol>>,
    picker: &Picker,
    slot: Rect,
    key: &str,
    path: Option<&str>,
    name: &str,
    color: Color,
    on_color: Color,
) {
    if let Some(path) = path.filter(|_| !cache.contains_key(key)) {
        // with_guessed_format：快取檔沒有副檔名，要看內容判斷格式。
        let proto = image::ImageReader::open(path)
            .ok()
            .and_then(|r| r.with_guessed_format().ok())
            .and_then(|r| r.decode().ok())
            .and_then(|img| picker.new_protocol(img, Size::new(AVATAR_W, 2), Resize::Fit(None)).ok());
        cache.insert(key.to_string(), proto);
    }
    match cache.get(key) {
        Some(Some(proto)) => f.render_widget(ratatui_image::Image::new(proto), slot),
        _ => {
            let initial = name.chars().next().unwrap_or('?');
            f.render_widget(
                Paragraph::new(format!(" {initial}"))
                    .style(Style::new().fg(on_color).bg(color).add_modifier(Modifier::BOLD)),
                slot,
            );
        }
    }
}

fn dim() -> Style {
    Style::new().add_modifier(Modifier::DIM)
}

/// 面板外框：圓角，焦點所在的那個用強調色。
fn pane(title: Line<'static>, active: bool, accent: Color) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if active { Style::new().fg(accent) } else { dim() })
        .title(title)
}

/// 底下那行按鍵提示：鍵名加粗、說明調暗。
fn keys(pairs: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (key, what) in pairs {
        spans.push(Span::styled(*key, Style::new().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(format!(" {what}   "), dim()));
    }
    Line::from(spans)
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn draw(f: &mut Frame, app: &mut App) {
    // 上面左右兩欄（清單｜訊息＋輸入列），最底下一整條狀態列。
    let rows = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(f.area());
    let in_chat = app.open.is_some();
    // 窄視窗（50 欄以內）放不下兩欄：一次只顯示一邊，沒開聊天室時是清單，開了就是對話，
    // Esc 回清單。另一邊的寬度是 0 —— 照樣走過下面的繪製，只是什麼都畫不出來。
    // tight：視窗真的窄，狀態列的字也要省。single：一次只顯示一邊（窄，或使用者把清單藏起來）。
    let tight = f.area().width <= NARROW_COLS;
    let narrow = tight || app.sidebar == Sidebar::Hidden;
    app.narrow = narrow;
    let compact = app.sidebar == Sidebar::Names && !narrow;
    let cols = Layout::horizontal(match (narrow, in_chat) {
        (true, true) => [Constraint::Length(0), Constraint::Min(0)],
        (true, false) => [Constraint::Min(0), Constraint::Length(0)],
        (false, _) => [Constraint::Length(if compact { 22 } else { 34 }), Constraint::Min(20)],
    })
    .split(rows[0]);
    let right = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).split(cols[1]);
    let accent = app.theme.accent;
    let on_accent = app.theme.on_accent;
    let selection = app.theme.selection;

    // ---------------------------------------------------------------- 聊天室清單
    // 兩行一筆：名稱＋時間，底下預覽＋未讀徽章。收窄時一行一筆：名稱＋未讀徽章。
    // 滑鼠點擊靠 list_item_rows() 換算，改行數要一起改那裡。
    let list_w = cols[0].width.saturating_sub(2) as usize;
    // 展開時每筆左邊留給大頭貼的寬度（收窄時只有名稱，不畫）。
    let lead = if compact { 0 } else { AVATAR_W as usize };
    let today = model::day_index(now_ms());
    let items: Vec<ListItem> = app
        .chats
        .iter()
        .enumerate()
        .map(|(idx, c)| {
            let selected = idx == app.selected;
            let bar = if selected {
                Span::styled("▌", Style::new().fg(accent))
            } else {
                Span::raw(" ")
            };
            let when = match c.last_time {
                0 => String::new(),
                t if model::day_index(t) == today => model::clock(t),
                t => model::day_label(t),
            };
            let label = if app.vip.contains(&c.mid) { format!("★ {}", c.name) } else { c.name.clone() };
            let name = fit(&label, list_w.saturating_sub(3 + lead + when.width()));
            let name_style = match (selected, c.unread > 0) {
                (true, _) => Style::new().fg(accent).add_modifier(Modifier::BOLD),
                (_, true) => Style::new().add_modifier(Modifier::BOLD),
                _ => Style::new(),
            };
            let pad = list_w.saturating_sub(2 + lead + name.width() + when.width());

            let badge = match c.unread {
                n if n <= 0 => String::new(),
                n if n > 99 => " 99+ ".into(),
                n => format!(" {n} "),
            };
            // 群組的預覽帶上是誰說的；一對一時發話者就是聊天室名稱，不重複。
            let preview = if c.last_from.is_empty() || c.last_from == c.name {
                c.last_text.clone()
            } else {
                format!("{}：{}", c.last_from, c.last_text)
            };
            let preview = fit(&preview, list_w.saturating_sub(3 + lead + badge.width()));
            let pad2 = list_w.saturating_sub(2 + lead + preview.width() + badge.width());

            if compact {
                let name = fit(&label, list_w.saturating_sub(3 + badge.width()));
                let pad = list_w.saturating_sub(2 + name.width() + badge.width());
                return ListItem::new(Line::from(vec![
                    bar,
                    Span::raw(" "),
                    Span::styled(name, name_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(
                        badge,
                        Style::new().fg(on_accent).bg(accent).add_modifier(Modifier::BOLD),
                    ),
                ]))
                .style(match selection {
                    Some(bg) if selected => Style::new().bg(bg),
                    _ => Style::new(),
                });
            }

            ListItem::new(vec![
                Line::from(vec![
                    bar.clone(),
                    Span::raw(" ".repeat(1 + lead)),
                    Span::styled(name, name_style),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(when, dim()),
                ]),
                Line::from(vec![
                    bar,
                    Span::raw(" ".repeat(1 + lead)),
                    Span::styled(preview, dim()),
                    Span::raw(" ".repeat(pad2)),
                    Span::styled(
                        badge,
                        Style::new().fg(on_accent).bg(accent).add_modifier(Modifier::BOLD),
                    ),
                ]),
            ])
            // 選取列鋪上主題的選取底色（有主題檔才有）。
            .style(match selection {
                Some(bg) if selected => Style::new().bg(bg),
                _ => Style::new(),
            })
        })
        .collect();

    app.list_area = cols[0];
    app.msg_area = right[0];

    let unread: i64 = app.chats.iter().map(|c| c.unread.max(0)).sum();
    let mut list_title = vec![Span::styled(" LINE ", Style::new().add_modifier(Modifier::BOLD))];
    if unread > 0 {
        let label = if compact { format!("· {unread} ") } else { format!("· 未讀 {unread} ") };
        list_title.push(Span::styled(label, Style::new().fg(accent)));
    }
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected));
    *list_state.offset_mut() = app.list_offset;
    f.render_stateful_widget(
        List::new(items).block(pane(Line::from(list_title), !in_chat, accent)),
        cols[0],
        &mut list_state,
    );
    // ratatui 會依選取項調整 offset，記回來給滑鼠換算用。
    app.list_offset = list_state.offset();

    // 大頭貼疊在清單上：圖片要自己的 Rect，塞不進 ListItem，所以清單先留空格、
    // 畫完再照「兩行一筆」算出每筆的位置把圖放上去。
    if !compact {
        let inner = cols[0].inner(Margin::new(1, 1));
        for (row, c) in app.chats.iter().skip(app.list_offset).enumerate() {
            let y = inner.y + row as u16 * 2;
            if y + 2 > inner.bottom() {
                break;
            }
            let slot = Rect { x: inner.x + 1, y, width: AVATAR_W, height: 2 }.intersection(inner);
            draw_avatar(
                f, &mut app.avatars, &app.picker, slot,
                &c.mid, c.avatar_path.as_deref(), &c.name, sender_color(&c.name), on_accent,
            );
        }
    }

    // ---------------------------------------------------------------- 訊息
    // 開著聊天室但在預覽別的：這一段畫的是被預覽的那個。msgs／scroll／cursor 是開著那個的，
    // 先換出來，畫完換回去（下面整段都讀 app.msgs，這樣不必每處都分兩種情況）。
    let peeking = app.peeking().map(|c| (c.mid.clone(), c.name.clone()));
    let saved = peeking.as_ref().map(|(mid, _)| {
        let shown = app.peek_cache.get(mid).cloned().unwrap_or_default();
        (std::mem::replace(&mut app.msgs, shown), app.scroll, app.cursor.take(), app.busy)
    });
    if saved.is_some() {
        app.scroll = 0;
        app.busy = app.msgs.is_empty(); // 還沒抓到就是「載入中」
    }
    let title = match &peeking {
        Some(_) => None,
        None => app.open.as_ref().map(|_| app.open_name.clone()),
    };
    let preview_name = match (&peeking, &app.open, &app.preview) {
        (Some((_, name)), _, _) => Some(name.clone()),
        (None, None, Some(mid)) => app.chats.iter().find(|c| &c.mid == mid).map(|c| c.name.clone()),
        _ => None,
    };
    let mut msg_block = pane(
        match &title {
            Some(name) => Line::from(Span::styled(
                format!(" {name} "),
                Style::new().add_modifier(Modifier::BOLD),
            )),
            // 預覽：名稱調暗，加一個「Enter 進入」，看得出還沒進去。
            None => match &preview_name {
                Some(name) => Line::from(vec![
                    Span::styled(format!(" {name} "), dim().add_modifier(Modifier::BOLD)),
                    Span::styled("· Enter 進入 ", dim()),
                ]),
                None => Line::default(),
            },
        },
        in_chat,
        accent,
    );
    if app.scroll > 0 {
        msg_block = msg_block
            .title_bottom(Line::from(format!(" ↓ 下面還有 {} 則 ", app.scroll)).right_aligned());
    }
    let inner = msg_block.inner(right[0]);
    f.render_widget(msg_block, right[0]);

    if app.msgs.is_empty() {
        let note = if app.busy {
            "載入中…"
        } else if !in_chat && app.preview.is_none() {
            "選一個聊天室"
        } else {
            "沒有訊息（LINE 只保留約兩週）"
        };
        let mid = Rect { y: inner.y + inner.height / 2, height: 1.min(inner.height), ..inner };
        f.render_widget(Paragraph::new(note).style(dim()).alignment(Alignment::Center), mid);
    }

    // 由下往上排：聊天室看的是最新的，捲動就是「從底往上跳過幾則」。
    // 每則是自己的一塊（圖片需要自己的 Rect）。全部靠左，靠名字的顏色分人。
    const IMAGE_ROWS: u16 = 12;
    const STICKER_ROWS: u16 = 6;
    /// 一則訊息排版後的尺寸。sep＝上面要不要日期線；head＝要不要「名字 時間」那行。
    struct Slot {
        i: usize,
        sep: bool,
        head: bool,
        gap: u16,
        img: Size,
        text: Size,
        /// 上面被切掉幾列。最舊那則放不下時只畫它的尾端，跟下面接起來。
        skip: u16,
    }
    // 每則的橫向配置：選取標記(1) 空(1) 大頭貼(4) 空(1) 內容… ↗ 捲軸
    let max_w = inner.width.saturating_sub(AVATAR_W + 6);
    let end = app.msgs.len().saturating_sub(app.scroll as usize);
    let mut y = inner.bottom();
    let mut stack: Vec<(Slot, u16)> = Vec::new();

    for i in (0..end).rev() {
        let m = &app.msgs[i];
        let prev = i.checked_sub(1).map(|p| &app.msgs[p]);
        let sep = prev.is_none_or(|p| model::day_index(p.time) != model::day_index(m.time));
        // 同一個人五分鐘內連續發的，只有第一則顯示名字與時間。
        let head = sep
            || prev.is_some_and(|p| {
                p.mine != m.mine || p.from_name != m.from_name || m.time - p.time > 5 * 60_000
            });
        let gap = u16::from(head && !sep);

        let img = match &m.media_path {
            None => Size::ZERO,
            Some(path) => {
                // with_guessed_format：貼圖與 FLEX 的快取檔沒有副檔名，要看內容判斷格式。
                let proto = app.images.entry(m.id.clone()).or_insert_with(|| {
                    image::ImageReader::open(path)
                        .ok()
                        .and_then(|r| r.with_guessed_format().ok())
                        .and_then(|r| r.decode().ok())
                        .map(|img| app.picker.new_resize_protocol(img))
                });
                let rows = if m.kind == "STICKER" { STICKER_ROWS } else { IMAGE_ROWS };
                match proto {
                    // 問它實際會畫多大，才能靠右對齊，也不會在圖下面留空行。
                    Some(p) => p.size_for(Resize::Fit(None), Size::new(max_w.min(60), rows)),
                    None => Size::new(14.min(max_w), 1), // 「[圖片讀不到]」
                }
            }
        };
        let text = if m.text.is_empty() {
            Size::ZERO
        } else {
            let longest = m.text.lines().map(|l| l.width()).max().unwrap_or(0) as u16;
            let w = (longest + 1).min(max_w).max(1);
            Size::new(w, wrapped_rows(&m.text, w))
        };
        let react = u16::from(!m.reactions.is_empty());
        let full = u16::from(sep) + gap + u16::from(head) + img.height + text.height + react;
        let room = y.saturating_sub(inner.y);
        if full == 0 || room == 0 {
            break;
        }
        if full <= room {
            y -= full;
            stack.push((Slot { i, sep, head, gap, img, text, skip: 0 }, full));
            continue;
        }
        // 放不下。以前在這裡直接停，一則長訊息就讓上面留一大片空白、更舊的全部不見。
        // 最底下那則（整個區域都放不下它）畫開頭，其餘畫尾端：視覺上都是從下往上接。
        let skip = if stack.is_empty() { 0 } else { full - room };
        y = inner.y;
        stack.push((Slot { i, sep, head, gap, img, text, skip }, room));
        break;
    }

    if stack.len() < app.msgs.len() {
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None),
            right[0].inner(Margin::new(0, 1)),
            &mut ScrollbarState::new(app.msgs.len()).position(end.saturating_sub(1)),
        );
    }

    app.msg_rects.clear();
    for (s, h) in stack.into_iter().rev() {
        let m = &app.msgs[s.i];
        let area = Rect { x: inner.x, y, width: inner.width, height: h };
        y += h;
        // 所有東西都夾在這一則分到的區域裡 —— 被截短的訊息不能畫到別人頭上。
        let put = |r: Rect| r.intersection(area);
        // cy 是「沒被切掉時」的位置，上面切掉的列在區域外面（cy 可能比區域頂端還小）。
        // whole：整個放得進這一則的區域才畫 —— 日期線、名字、大頭貼、圖片切一半沒有意義。
        let mut cy = area.y as i32 - s.skip as i32;
        let whole = |y: i32, h: u16| (y >= area.y as i32 && y + h as i32 <= area.bottom() as i32).then_some(y as u16);

        if let Some(y) = whole(cy, 1).filter(|_| s.sep) {
            f.render_widget(
                Paragraph::new(format!("──────  {}  ──────", model::day_label_long(m.time)))
                    .style(dim())
                    .alignment(Alignment::Center),
                put(Rect { y, height: 1, ..area }),
            );
        }
        cy += i32::from(s.sep) + s.gap as i32;

        let selected = app.cursor == Some(s.i);
        let color = if m.mine { accent } else { sender_color(&m.from_name) };
        let x = inner.x + 3 + AVATAR_W;
        let top = cy.max(area.y as i32) as u16;
        let body = put(Rect { y: top, height: area.bottom().saturating_sub(top), ..area });
        app.msg_rects.push((body, s.i));

        // Tab 選中的那則，最左邊畫一條。
        if selected {
            f.render_widget(
                Paragraph::new(vec![Line::from("┃"); body.height as usize]).style(Style::new().fg(accent)),
                Rect { width: 1.min(body.width), ..body },
            );
        }

        if s.head {
            // 大頭貼：佔名字那行與下一行。
            if let Some(y) = whole(cy, 2) {
                let slot = put(Rect { x: inner.x + 2, y, width: AVATAR_W, height: 2 });
                draw_avatar(
                    f, &mut app.avatars, &app.picker, slot,
                    &m.from, m.avatar_path.as_deref(), &m.from_name, color, on_accent,
                );
            }

            let who = if m.mine { "我" } else { &m.from_name };
            let line = Line::from(vec![
                Span::styled(
                    fit(who, max_w.saturating_sub(7) as usize),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(model::clock(m.time), dim().add_modifier(Modifier::ITALIC)),
            ]);
            let w = line.width() as u16;
            if let Some(y) = whole(cy, 1) {
                f.render_widget(Paragraph::new(line), put(Rect { x, y, width: w, height: 1 }));
            }
            cy += 1;
        }

        // ↗：這則點得開（附件或連結）。放在內容第一行的外側。
        if m.openable() {
            let first_w = if s.img.height > 0 { s.img.width } else { s.text.width };
            let style = if selected {
                Style::new().fg(accent).add_modifier(Modifier::REVERSED)
            } else {
                Style::new().fg(accent)
            };
            if let Some(y) = whole(cy, 1) {
                f.render_widget(
                    Paragraph::new("↗").style(style),
                    put(Rect { x: x + first_w + 1, y, width: 1, height: 1 }),
                );
            }
        }

        if s.img.height > 0 {
            if let Some(y) = whole(cy, s.img.height) {
                let r = put(Rect { x, y, width: s.img.width, height: s.img.height });
                match app.images.get_mut(&m.id) {
                    Some(Some(proto)) => f.render_stateful_widget(StatefulImage::default(), r, proto),
                    _ => f.render_widget(Paragraph::new("[圖片讀不到]").style(dim()), r),
                }
            }
            cy += s.img.height as i32;
        }
        // 靠右放在內容第一列的小標籤：自己發的顯示已讀（一對一只說「已讀」，群組帶人數），
        // 別人的訊息 @ 了我（或 @所有人）就標「@你」。
        let tag = if m.mine && m.read_count > 0 {
            let one_to_one = app.open.as_deref().is_some_and(|c| c.starts_with('u'));
            Some((if one_to_one { "已讀".to_string() } else { format!("已讀 {}", m.read_count) }, dim()))
        } else if !m.mine && m.mentions.iter().any(|x| *x == app.me || x == "all") {
            Some(("@你".to_string(), Style::new().fg(accent).add_modifier(Modifier::BOLD)))
        } else {
            None
        };
        if let (Some((tag, style)), Some(y)) = (tag, whole(cy, 1)) {
            let w = tag.width() as u16;
            f.render_widget(
                Paragraph::new(tag).style(style),
                put(Rect { x: inner.right().saturating_sub(w + 2), y, width: w, height: 1 }),
            );
        }
        if s.text.height > 0 {
            // 上面被切掉的話，從第 cut 列開始畫（Paragraph 的 scroll），顯示的是尾端。
            let cut = (area.y as i32 - cy).max(0) as u16;
            let y = cy.max(area.y as i32) as u16;
            let body = if m.kind == "UNSENT" { Text::from(Span::styled(m.text.clone(), dim())) } else { rich(&m.text, accent) };
            f.render_widget(
                Paragraph::new(body).wrap(Wrap { trim: false }).scroll((cut, 0)),
                put(Rect { x, y, width: s.text.width, height: s.text.height.saturating_sub(cut) }),
            );
            cy += s.text.height as i32;
        }
        if !m.reactions.is_empty() {
            if let Some(y) = whole(cy, 1) {
                f.render_widget(
                    Paragraph::new(model::reaction_summary(&m.reactions)),
                    put(Rect { x, y, width: inner.right().saturating_sub(x), height: 1 }),
                );
            }
        }
    }

    if let Some((msgs, scroll, cursor, busy)) = saved {
        app.msgs = msgs;
        app.scroll = scroll;
        app.cursor = cursor;
        app.busy = busy;
        app.msg_rects.clear(); // 點擊對應的是開著那個的索引，預覽中的不能點
    }
    // 輸入框與狀態列用的是開著的那個。
    let title = app.open.as_ref().map(|_| app.open_name.clone());

    // ---------------------------------------------------------------- 輸入列
    let input_block = pane(Line::default(), in_chat, accent);
    let input_inner = input_block.inner(right[1]);
    f.render_widget(input_block, right[1]);
    // 掛著的附件：輸入框最前面一個標籤，檔名與大小。
    let chip = app.attach.as_ref().map(|path| {
        let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        format!(" {} · {} ", fit(&name, 28), human_size(size))
    });
    let chip_w = chip.as_ref().map_or(0, |c| c.width() + 1);
    let room = (input_inner.width.saturating_sub(3) as usize).saturating_sub(chip_w); // 「› 」與游標那一格
    // 打超過寬度時顯示尾巴：正在打的那幾個字要看得到。
    let mut shown = String::new();
    let mut used = 0;
    for c in app.input.chars().rev() {
        // 貼進來的多行文字：輸入框只有一行，換行用符號表示，送出時還是真的換行。
        let c = if c == '\n' { '⏎' } else { c };
        let w = c.width().unwrap_or(0);
        if used + w > room {
            break;
        }
        shown.insert(0, c);
        used += w;
    }
    let body = match (in_chat, app.input.is_empty()) {
        _ if peeking.is_some() => Span::styled(
            format!(
                "Enter 進入 {}　Esc 回到 {}",
                fit(&peeking.as_ref().unwrap().1, 20),
                fit(title.as_deref().unwrap_or(""), 20)
            ),
            dim(),
        ),
        (false, _) => Span::styled("先選一個聊天室", dim()),
        (true, true) if chip.is_some() => Span::styled("Enter 送出　Esc 取消附件", dim()),
        (true, true) => Span::styled(
            format!("傳訊息給 {}…", fit(title.as_deref().unwrap_or(""), room.saturating_sub(12))),
            dim(),
        ),
        (true, false) => Span::raw(shown),
    };
    let prompt = Span::styled("› ", if in_chat { Style::new().fg(accent) } else { dim() });
    let mut input_line = vec![prompt];
    if let Some(chip) = chip {
        input_line.push(Span::styled(chip, Style::new().fg(on_accent).bg(accent)));
        input_line.push(Span::raw(" "));
    }
    input_line.push(body);
    f.render_widget(Paragraph::new(Line::from(input_line)), input_inner);
    // 游標放在輸入位置：輸入法（fcitx5）的候選字視窗是跟著游標走的。
    if in_chat && peeking.is_none() && input_inner.width > 2 {
        f.set_cursor_position((input_inner.x + 2 + (chip_w + used) as u16, input_inner.y));
    }

    // ---------------------------------------------------------------- 狀態列
    // 左：模式標籤＋目前聊天室＋按鍵提示（或狀態訊息）。右：跟 daemon 的連線。
    let (mode, hints) = if app.path_prompt.is_some() {
        ("附檔", keys(&[("Tab", "補完"), ("Enter", "附加"), ("Esc", "取消")]))
    } else if app.search.is_some() {
        ("搜尋", keys(&[("↑↓", "選擇"), ("Enter", "開啟"), ("Esc", "取消")]))
    } else if peeking.is_some() {
        ("預覽", keys(&[("↑↓", "選擇"), ("Enter", "進入"), ("Esc", "回到這個聊天室")]))
    } else if app.cursor.is_some() {
        ("選取", keys(&[("r", "表情"), ("d", "收回"), ("Enter", "開啟"), ("Tab", "上一個"), ("Esc", "取消")]))
    } else if in_chat {
        if tight {
            // 放不下全部：先給離開這個畫面的鍵，其餘看 F1。
            ("對話", keys(&[("Esc", "清單"), ("Enter", "送出"), ("F1", "按鍵")]))
        } else {
            ("對話", keys(&[("Enter", "送出"), ("^V", "貼上"), ("^F", "附檔"), ("Tab", "選附件 ↗"), ("/", "搜尋"), ("F1", "按鍵"), ("Esc", "返回")]))
        }
    } else if tight {
        ("清單", keys(&[("Enter", "開啟"), ("/", "搜尋"), ("?", "按鍵")]))
    } else {
        ("清單", keys(&[("↑↓", "選擇"), ("Enter", "開啟"), ("/", "搜尋"), ("?", "按鍵"), ("q", "離開")]))
    };
    let mut left = vec![
        Span::styled(
            format!(" {mode} "),
            Style::new().fg(on_accent).bg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    // 單欄時聊天室名稱已經在外框標題上，這裡不重複。
    if let Some(name) = title.as_ref().filter(|_| !narrow) {
        left.push(Span::raw(format!("{}  ", fit(name, 24))));
    }
    if app.status.is_empty() {
        left.extend(hints.spans);
    } else {
        left.push(Span::styled(app.status.clone(), Style::new().fg(Color::Yellow)));
    }
    f.render_widget(Paragraph::new(Line::from(left)), rows[1]);
    let (dot, label) = if app.online { (Color::Green, "已連線") } else { (Color::Red, "離線") };
    let link = Line::from(vec![
        Span::styled(" ●", Style::new().fg(dot)),
        Span::styled(if tight { " ".to_string() } else { format!(" {label} ") }, dim()),
    ]);
    // VIP 的未讀：不管現在看的是哪個畫面（對話最大化時看不到清單）都在這裡。
    let waiting: Vec<&Chat> = app.chats.iter().filter(|c| c.unread > 0 && app.vip.contains(&c.mid)).collect();
    let link = match waiting.first() {
        None => link,
        Some(first) => {
            let total: i64 = waiting.iter().map(|c| c.unread).sum();
            let text = match (tight, waiting.len()) {
                (true, _) => format!(" ★ {total} "),
                (false, 1) => format!(" ★ {} {total} ", fit(&first.name, 16)),
                (false, n) => format!(" ★ {} 等 {n} 個 · {total} ", fit(&first.name, 12)),
            };
            let mut spans = vec![Span::styled(
                text,
                Style::new().fg(on_accent).bg(accent).add_modifier(Modifier::BOLD),
            )];
            spans.extend(link.spans);
            Line::from(spans)
        }
    };
    let w = (link.width() as u16).min(rows[1].width);
    f.render_widget(
        Paragraph::new(link),
        Rect { x: rows[1].right() - w, width: w, ..rows[1] },
    );

    draw_overlays(f, app);
}

/// 搜尋結果的內文：從關鍵字前幾個字開始，截到 max 寬，關鍵字反白。
fn snippet(text: &str, term: &str, max: usize, accent: Color) -> Vec<Span<'static>> {
    let lower = text.to_lowercase();
    // 小寫化可能改變位元組長度（少數字元），對不上就退回不反白、從頭顯示。
    let at = (!term.is_empty() && lower.len() == text.len()).then(|| lower.find(term)).flatten();
    let Some(at) = at else { return vec![Span::raw(fit(text, max))] };
    let lead: String = text[..at].chars().rev().take(8).collect::<Vec<_>>().into_iter().rev().collect();
    let prefix = if lead.len() < at { format!("…{lead}") } else { lead };
    let hit = &text[at..at + term.len()];
    let rest = fit(&text[at + term.len()..], max.saturating_sub(prefix.width() + hit.width()));
    vec![
        Span::raw(prefix),
        Span::styled(hit.to_string(), Style::new().fg(accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)),
        Span::raw(rest),
    ]
}

/// 在 area 正中間切一塊 w×h（放不下就縮到放得下）。
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(area.width), h.min(area.height));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

/// 搜尋與按鍵說明的浮層。draw() 最後呼叫，蓋在其他東西上面。
fn draw_overlays(f: &mut Frame, app: &App) {
    let accent = app.theme.accent;
    let bold = Style::new().add_modifier(Modifier::BOLD);

    if let Some(search) = &app.search {
        let hits = app.search_hits();
        let area = centered(f.area(), 56, 18);
        f.render_widget(Clear, area);
        let block = pane(Line::from(Span::styled(" 搜尋聊天室 ", bold)), true, accent)
            .title_bottom(Line::from(format!(" {} 筆 ", hits.len())).right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height < 2 || inner.width < 6 {
            return;
        }

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("/ ", Style::new().fg(accent)),
                Span::raw(search.query.clone()),
            ])),
            Rect { height: 1, ..inner },
        );
        // 游標放在關鍵字後面，輸入法的候選字視窗才會出現在這裡。
        let cx = inner.x + 2 + search.query.width() as u16;
        f.set_cursor_position((cx.min(inner.right() - 1), inner.y));

        let rows = (inner.height - 1) as usize;
        let start = (search.pick + 1).saturating_sub(rows); // 選中的那筆永遠看得到
        let w = inner.width as usize;
        let lines: Vec<Line> = hits
            .iter()
            .enumerate()
            .skip(start)
            .take(rows)
            .map(|(i, c)| {
                let picked = i == search.pick;
                let badge = if c.unread > 0 { format!(" {} ", c.unread.min(999)) } else { String::new() };
                let name = fit(&c.name, w.saturating_sub(3 + badge.width()));
                let pad = w.saturating_sub(2 + name.width() + badge.width());
                let line = Line::from(vec![
                    Span::styled(if picked { "▌ " } else { "  " }, Style::new().fg(accent)),
                    Span::styled(name, if picked { bold.fg(accent) } else { Style::new() }),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(badge, Style::new().fg(app.theme.on_accent).bg(accent)),
                ]);
                match app.theme.selection {
                    Some(bg) if picked => line.style(Style::new().bg(bg)),
                    _ => line,
                }
            })
            .collect();
        let list = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
        if lines.is_empty() {
            f.render_widget(Paragraph::new("  沒有符合的聊天室").style(dim()), list);
        } else {
            f.render_widget(Paragraph::new(lines), list);
        }
    }

    if app.react_picker {
        const PICK: [(&str, &str); 7] = [
            ("1", "NICE"), ("2", "LOVE"), ("3", "FUN"), ("4", "AMAZING"), ("5", "SAD"), ("6", "OMG"), ("0", "UNDO"),
        ];
        let line = Line::from(
            PICK.iter()
                .flat_map(|(k, kind)| {
                    let glyph = if *kind == "UNDO" { "取消回應" } else { model::reaction_glyph(kind) };
                    [Span::styled(format!(" {k} "), bold.fg(accent)), Span::raw(format!("{glyph}  "))]
                })
                .collect::<Vec<_>>(),
        );
        let area = centered(f.area(), line.width() as u16 + 4, 3);
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(line).block(pane(Line::from(Span::styled(" 表情 ", bold)), true, accent)),
            area,
        );
    }

    if app.mention.is_some() {
        let hits = app.mention_hits();
        let pick = app.mention.as_ref().map_or(0, |m| m.pick);
        let query = app.mention.as_ref().map_or(String::new(), |m| m.query.clone());
        let loaded = app.open.as_ref().is_some_and(|c| app.members.contains_key(c));
        let area = centered(f.area(), 40, 12);
        f.render_widget(Clear, area);
        let block = pane(Line::from(Span::styled(" 提及 ", bold)), true, accent)
            .title_bottom(Line::from(" Enter 選　Esc 只打 @ ").right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let mut lines = vec![Line::from(vec![Span::styled("@", Style::new().fg(accent)), Span::raw(query)])];
        if !loaded {
            lines.push(Line::from(Span::styled(" 載入成員…", dim())));
        }
        let rows = inner.height.saturating_sub(1) as usize;
        let start = (pick + 1).saturating_sub(rows);
        for (i, (_, name)) in hits.iter().enumerate().skip(start).take(rows) {
            let picked = i == pick;
            lines.push(Line::from(vec![
                Span::styled(if picked { "▌ " } else { "  " }, Style::new().fg(accent)),
                Span::styled(fit(name, inner.width.saturating_sub(3) as usize), if picked { bold.fg(accent) } else { Style::new() }),
            ]));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    if let Some(ms) = &app.msg_search {
        let hits = archive::find(&ms.all, &ms.query);
        let area = centered(f.area(), 100, 26);
        f.render_widget(Clear, area);
        let block = pane(Line::from(Span::styled(" 搜尋訊息 ", bold)), true, accent).title_bottom(
            Line::from(format!(" {} 筆 · 存檔共 {} 則 · Enter 開聊天室 · Esc 關閉 ", hits.len(), ms.all.len()))
                .right_aligned(),
        );
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height >= 2 && inner.width > 10 {
            f.render_widget(
                Paragraph::new(Line::from(vec![Span::styled("⌕ ", Style::new().fg(accent)), Span::raw(ms.query.clone())])),
                Rect { height: 1, ..inner },
            );
            let cx = inner.x + 2 + ms.query.width() as u16;
            f.set_cursor_position((cx.min(inner.right() - 1), inner.y));

            // 一筆兩行：日期、聊天室、發話者；底下是內文，從第一個關鍵字前一點開始，關鍵字反白。
            let rows = ((inner.height - 1) / 2) as usize;
            let start = (ms.pick + 1).saturating_sub(rows);
            let w = inner.width as usize;
            let first = ms.query.split_whitespace().next().unwrap_or("").to_lowercase();
            let mut lines: Vec<Line> = Vec::new();
            for (i, h) in hits.iter().enumerate().skip(start).take(rows) {
                let picked = i == ms.pick;
                let bar = Span::styled(if picked { "▌ " } else { "  " }, Style::new().fg(accent));
                let chat = app.chat_name(&h.chat).unwrap_or_else(|| h.from_name.clone());
                let who = if h.from_name == chat { String::new() } else { format!(" · {}", h.from_name) };
                lines.push(Line::from(vec![
                    bar.clone(),
                    Span::styled(format!("{} {}  ", model::day_label(h.time), model::clock(h.time)), dim()),
                    Span::styled(fit(&format!("{chat}{who}"), w.saturating_sub(16)), if picked { bold.fg(accent) } else { bold }),
                ]));
                lines.push(Line::from([vec![bar], snippet(&h.text, &first, w.saturating_sub(4), accent)].concat()));
            }
            let list = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
            let note = if ms.query.trim().is_empty() { "  打字開始搜尋（空白分開的詞都要出現）" } else { "  沒有符合的訊息" };
            if lines.is_empty() {
                f.render_widget(Paragraph::new(note).style(dim()), list);
            } else {
                f.render_widget(Paragraph::new(lines), list);
            }
        }
    }

    if let Some(prompt) = &app.path_prompt {
        let area = centered(f.area(), 72, 14);
        f.render_widget(Clear, area);
        let block = pane(Line::from(Span::styled(" 附加檔案 ", bold)), true, accent)
            .title_bottom(Line::from(" Tab 補完   Enter 附加   Esc 取消 ").right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        if inner.height >= 1 && inner.width > 4 {
            // 路徑太長時顯示尾巴。
            let room = inner.width as usize - 3;
            let mut shown = String::new();
            let mut used = 0;
            for c in prompt.text.chars().rev() {
                let w = c.width().unwrap_or(0);
                if used + w > room {
                    break;
                }
                shown.insert(0, c);
                used += w;
            }
            let mut lines = vec![Line::from(vec![Span::styled("› ", Style::new().fg(accent)), Span::raw(shown)])];
            lines.extend(
                prompt.matches.iter().take(inner.height as usize - 1).map(|m| {
                    Line::from(Span::styled(format!("  {}", fit(m, room)), dim()))
                }),
            );
            f.render_widget(Paragraph::new(lines), inner);
            f.set_cursor_position((inner.x + 2 + used as u16, inner.y));
        }
    }

    if app.help {
        // 分組：每組一個標題加幾個按鍵。高度不夠時整組搬到右欄，組不會被拆開。
        const HELP: &[(&str, &[(&str, &str)])] = &[
            ("VIP", &[
                ("Ctrl-P", "把這個聊天室設為 VIP／取消"),
                ("", "VIP 置頂；他們的新訊息會在螢幕中央彈大卡片，"),
                ("", "TUI 沒開、縮小、沒焦點都會彈，讀過才消失"),
            ]),
            ("清單", &[
                ("↑ ↓  j k", "選聊天室"),
                ("Enter", "開啟聊天室"),
                ("/", "搜尋全部聊天室"),
                ("Ctrl-S", "搜尋訊息內容（本機存檔，比兩週更早的也找得到）"),
                ("r", "重整清單"),
                ("q", "離開"),
            ]),
            ("對話", &[
                ("Enter", "送出（輸入框有字時）"),
                ("Tab  ⇧Tab", "選訊息（之後 r 表情、d 收回、Enter 開）"),
                ("PgUp PgDn", "捲動訊息"),
                ("g", "載入更舊的訊息（輸入框空的時候）"),
                ("/", "搜尋全部聊天室（輸入框空的時候）"),
                ("Esc", "取消選取；再按一次回清單"),
            ]),
            ("訊息", &[
                ("Tab ⇧Tab", "選一則訊息，選中後："),
                ("  r", "表情回應（1–6 選，0 取消回應）"),
                ("  d", "收回自己的訊息（按兩次）"),
                ("  Enter", "開附件或連結（標 ↗ 的）"),
                ("@", "群組裡打 @ 挑人提及"),
            ]),
            ("附件", &[
                ("Ctrl-V", "貼上：圖片或複製的檔案變附件"),
                ("Ctrl-F", "附加檔案（輸入路徑，Tab 補完）"),
                ("拖檔案進來", "附加那個檔案"),
            ]),
            ("任何時候", &[
                ("Ctrl-B", "清單：展開 → 只有名稱 → 隱藏"),
                ("? 或 F1", "這個說明（聊天時 ? 是字，用 F1）；j/k 捲動"),
                ("Ctrl-C", "離開"),
            ]),
            ("滑鼠", &[
                ("點聊天室", "開啟"),
                ("點 ↗ 訊息", "開啟附件或連結"),
                ("滾輪", "捲動"),
            ]),
        ];
        let group_lines = |(title, rows): &(&'static str, &'static [(&'static str, &'static str)])| -> Vec<Line<'static>> {
            let mut out = vec![Line::from(Span::styled(format!(" {title}"), bold.fg(accent)))];
            out.extend(rows.iter().map(|(key, what)| {
                let pad = 12usize.saturating_sub(key.width());
                Line::from(vec![
                    Span::styled(format!("   {key}{}", " ".repeat(pad)), bold),
                    Span::raw(*what),
                ])
            }));
            out
        };
        const COL_W: u16 = 58;
        let total: u16 = HELP.iter().map(|(_, rows)| rows.len() as u16 + 1).sum();
        let screen = f.area();
        // 一欄放得下就一欄；不然兩欄，左欄放到剛好超過一半為止。
        let two = total + 2 > screen.height && screen.width >= COL_W * 2 + 3;
        let mut cols: Vec<Vec<Line>> = vec![vec![]];
        let half = total.div_ceil(2) as usize;
        for g in HELP {
            if two && cols.len() == 1 && cols[0].len() >= half {
                cols.push(vec![]);
            }
            let last = cols.len() - 1;
            if !cols[last].is_empty() {
                cols[last].push(Line::default()); // 組與組之間空一行
            }
            cols[last].extend(group_lines(g));
        }
        let total = cols.iter().map(Vec::len).max().unwrap_or(0) as u16;
        let area = centered(screen, COL_W * cols.len() as u16 + 2, total + 2);
        // 還是放不下就捲動（兩欄一起捲）。能捲多少要畫的時候才知道，記給按鍵處理用。
        let max = total.saturating_sub(area.height.saturating_sub(2));
        app.help_max.set(max);
        let scroll = app.help_scroll.min(max);
        let hint = if max > 0 { " j/k 捲動　Esc 關閉 " } else { " Esc 關閉 " };
        let mut block = pane(Line::from(Span::styled(" 按鍵 ", bold)), true, accent)
            .title_bottom(Line::from(hint).right_aligned());
        if scroll < max {
            block = block.title_bottom(Line::from(Span::styled(" ↓ 下面還有 ", Style::new().fg(accent))));
        }
        let inner = block.inner(area);
        f.render_widget(Clear, area);
        f.render_widget(block, area);
        for (i, lines) in cols.into_iter().enumerate() {
            let x = inner.x + i as u16 * COL_W;
            let col = Rect { x, width: COL_W.min(inner.right().saturating_sub(x)), ..inner };
            f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), col);
        }
    }
}

fn restore_terminal() -> std::io::Result<()> {
    set_pointer("text"); // 還給終端機平常的 I-beam
    disable_raw_mode()?;
    execute!(
        stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableFocusChange,
        DisableBracketedPaste
    )
}

/// daemon 的 state 裡，登入完成後自己的 mid。還沒登入就是 None。
fn logged_in_mid(state: &Value) -> Option<String> {
    let ok = state.pointer("/login/status").and_then(Value::as_str) == Some("ok");
    let mid = state.pointer("/me/mid").and_then(Value::as_str).unwrap_or("");
    (ok && !mid.is_empty()).then(|| mid.to_string())
}

/// daemon 還沒登入時的畫面。回傳登入後自己的 mid；使用者離開就是 None。
///
/// 登入本身還是 daemon 在做，這裡只是把它寫在 state 裡的進度畫出來：QR 的 PNG、
/// 要在手機上輸入的 PIN。QR 需要有人拿手機在旁邊，所以不自動產生 —— 按 Enter 才要。
fn login_screen(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    picker: &Picker,
) -> std::io::Result<Option<String>> {
    let theme = Theme::load();
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut state = Value::Null;
    let mut asked: Option<Instant> = None;
    // (PNG 路徑, 解好的圖)。daemon 每產生一次 QR 就換檔名，路徑變了才重新解。
    let mut qr: Option<(String, Option<StatefulProtocol>)> = None;

    loop {
        if asked.is_none_or(|t| t.elapsed() > Duration::from_secs(1)) {
            asked = Some(Instant::now());
            state = client::request(serde_json::json!({ "cmd": "state" })).unwrap_or(Value::Null);
        }
        if let Some(mid) = logged_in_mid(&state) {
            return Ok(Some(mid));
        }
        let field = |k: &str| {
            state.pointer(&format!("/login/{k}")).and_then(Value::as_str).unwrap_or("").to_string()
        };
        let status = field("status");
        let png = field("qrPng");
        if status != "qr" || png.is_empty() {
            qr = None;
        } else if qr.as_ref().is_none_or(|(path, _)| *path != png) {
            let image = image::ImageReader::open(&png)
                .ok()
                .and_then(|r| r.decode().ok())
                .map(|img| picker.new_resize_protocol(img));
            qr = Some((png.clone(), image));
        }

        let lines: Vec<Line> = match status.as_str() {
            "qr" => vec![
                Line::from(Span::styled("用手機的 LINE 掃這個 QR", bold)),
                // 圖畫不清楚時（半格字元）還有這條路：網址就是 QR 的內容。
                Line::from(Span::styled(field("url"), dim())),
            ],
            "pin" => vec![
                Line::from("到手機的 LINE 輸入這組數字"),
                Line::from(Span::styled(field("pin"), bold.fg(theme.accent))),
            ],
            "starting" => vec![Line::from("登入中…")],
            "" => vec![Line::from("連不上 daemon（systemctl --user start enil）")],
            // idle：沒有可用的憑證；error：上一次登入失敗。
            _ => vec![
                Line::from(Span::styled(field("error"), Style::new().fg(Color::Yellow))),
                Line::from(vec![Span::styled("Enter", bold), Span::raw(" 產生登入用的 QR")]),
            ],
        };

        term.draw(|f| {
            // 盡量大：QR 是正方形，格子大約 1:2，所以寬取高的兩倍。上限是為了超寬螢幕。
            let h = f.area().height.saturating_sub(2).min(46);
            let area = centered(f.area(), h * 2, h);
            let block = pane(Line::from(Span::styled(" 登入 LINE ", bold)), true, theme.accent)
                .title_bottom(Line::from(" Enter 登入   q 離開 ").right_aligned());
            let inner = block.inner(area).inner(Margin::new(2, 1));
            f.render_widget(block, area);
            let text_h = 4.min(inner.height);
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }),
                Rect { height: text_h, ..inner },
            );
            if let Some((_, Some(image))) = qr.as_mut() {
                let rest = Rect { y: inner.y + text_h, height: inner.height - text_h, ..inner };
                // Scale 會把小圖放大到填滿（預設的 Fit 只縮不放，QR 就只有原圖那麼大）。
                // Nearest：QR 是黑白方塊，內插會糊掉反而難掃。
                let scale = Resize::Scale(Some(ratatui_image::FilterType::Nearest));
                let size = image.size_for(scale.clone(), rest.as_size());
                let spot = Rect {
                    x: rest.x + rest.width.saturating_sub(size.width) / 2,
                    width: size.width.min(rest.width),
                    height: size.height.min(rest.height),
                    ..rest
                };
                f.render_stateful_widget(StatefulImage::default().resize(scale), spot, image);
            }
        })?;

        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
            KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
            KeyCode::Enter => {
                // daemon 自己會擋重複的請求（已經在登入中就不再開一次）。
                let _ = client::request(serde_json::json!({ "cmd": "login" }));
                asked = None;
            }
            _ => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 先確認 daemon 活著並拿到自己的 mid —— 沒有它就分不出哪些訊息是自己發的。
    let state = client::request(serde_json::json!({ "cmd": "state" }))?;
    let mut me = logged_in_mid(&state).unwrap_or_default();

    // 問終端機支援哪個圖片協定（ghostty→kitty、foot→sixel）。
    // 只在 stdin 真的是終端機時才問：from_query_stdio 會寫查詢並讀回應，
    // 管線裡沒有回應者，它會把輸入狀態弄壞（實測滑鼠事件之後全部收不到）。
    // ENIL_TUI_HALFBLOCKS=1 可略過查詢：某些終端機（或 pty 測試環境）沒有回應者，
    // 查詢會把輸入狀態弄壞。
    let skip_query = std::env::var("ENIL_TUI_HALFBLOCKS").is_ok();
    let picker = if !skip_query && std::io::stdin().is_terminal() {
        Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks())
    } else {
        Picker::halfblocks()
    };

    enable_raw_mode()?;
    // BracketedPaste：貼上的整段文字變成一個事件。沒開的話貼多行會被當成一個個按鍵，
    // 每個換行都是一次「送出」。
    execute!(
        stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;

    // 還沒登入就先過登入畫面。使用者在那裡離開的話，程式也就結束。
    if me.is_empty() {
        match login_screen(&mut term, &picker) {
            Ok(Some(mid)) => me = mid,
            other => {
                restore_terminal()?;
                return other.map(|_| ()).map_err(Into::into);
            }
        }
    }

    let (tx, rx): (Sender<Update>, Receiver<Update>) = channel();
    let mut app = App {
        chats: vec![],
        selected: 0,
        msgs: vec![],
        scroll: 0,
        input: String::new(),
        status: "載入聊天室…".into(),
        me,
        open: None,
        busy: false,
        focused: true,
        online: false,
        needs_login: false,
        narrow: false,
        peek_cache: HashMap::new(),
        drafts: HashMap::new(),
        peek: false,
        preview: None,
        preview_seen: None,
        preview_moved: Instant::now(),
        sidebar: Sidebar::Full,
        vip: load_vip(),
        open_name: String::new(),
        search: None,
        msg_search: None,
        player: None,
        reload_at: None,
        unsend_armed: None,
        react_picker: false,
        mention: None,
        members: HashMap::new(),
        pending_mentions: Vec::new(),
        all_chats: vec![],
        help: false,
        help_scroll: 0,
        help_max: std::cell::Cell::new(0),
        attach: None,
        path_prompt: None,
        theme: Theme::load(),
        tx,
        list_area: Rect::default(),
        msg_area: Rect::default(),
        list_offset: 0,
        msg_rects: vec![],
        cursor: None,
        picker,
        images: HashMap::new(),
        avatars: HashMap::new(),
    };
    app.load_chats();

    // 推送：一條常駐連線收新訊息。daemon 重啟會斷線，隔幾秒再連。
    {
        let (tx, me) = (app.tx.clone(), app.me.clone());
        thread::spawn(move || loop {
            let _ = client::subscribe(
                || {
                    let _ = tx.send(Update::Online(true));
                },
                |event, data| {
                    if event != "message" {
                        let _ = tx.send(Update::Event(event.to_string(), data));
                        return;
                    }
                    let chat = data.get("chat").and_then(Value::as_str).unwrap_or("").to_string();
                    if let Some(msg) = model::parse_msgs(&Value::Array(vec![data]), &me).pop() {
                        let _ = tx.send(Update::Incoming(chat, msg));
                    }
                },
            );
            let _ = tx.send(Update::Online(false));
            thread::sleep(Duration::from_secs(5));
        });
    }

    let mut last_refresh = Instant::now();
    let mut last_theme = Instant::now();
    let mut last_session = Instant::now();
    let mut hand = false; // 指標現在是不是手指，只在變的時候才送序列
    let res = loop {
        term.draw(|f| draw(f, &mut app))?;

        while let Ok(u) = rx.try_recv() {
            app.apply(u);
        }

        // 換主題要跟上：一秒看一次主題檔的修改時間（一次 stat，不讀檔）。
        if last_theme.elapsed() > Duration::from_secs(1) {
            last_theme = Instant::now();
            app.theme.refresh();
            app.take_open_request();
        }

        // daemon 沒登入：切到登入畫面，登入完回來把清單與開著的聊天室重讀一次。
        if app.needs_login {
            match login_screen(&mut term, &app.picker)? {
                Some(mid) => {
                    app.me = mid;
                    app.needs_login = false;
                    app.status.clear();
                    app.load_chats();
                    app.reload_open();
                }
                None => break Ok(()),
            }
        }
        app.update_preview();
        // 已讀／表情／編輯的推送：到時間重讀開著的聊天室。往上捲著的時候不重讀（會把載入的
        // 舊訊息丟掉），等捲回底部的下一個事件。
        if app.reload_at.is_some_and(|t| Instant::now() >= t) {
            app.reload_at = None;
            if app.scroll == 0 && app.cursor.is_none() {
                app.reload_open();
            }
        }
        // 播完的 mpv 收屍，不然留著殭屍行程，下次按也會誤判成「正在播」。
        if app.player.as_mut().is_some_and(|p| !matches!(p.try_wait(), Ok(None))) {
            app.player = None;
        }
        if last_session.elapsed() > Duration::from_secs(10) {
            last_session = Instant::now();
            app.check_session();
        }

        // 新訊息靠推送，這裡只是保底：跟伺服器對齊未讀數（例如在手機上讀掉的）。
        if last_refresh.elapsed() > Duration::from_secs(60) {
            last_refresh = Instant::now();
            app.load_chats();
        }

        if !event::poll(Duration::from_millis(120))? {
            continue;
        }
        let ev = event::read()?;
        match ev {
            Event::FocusGained => {
                app.focused = true;
                app.check_session();
            }
            Event::FocusLost => {
                app.focused = false;
                app.check_session();
            }
            _ => {}
        }
        if let Event::Paste(text) = ev {
            app.on_paste(text);
            continue;
        }
        // 說明開著時滾輪捲說明。
        if let (true, Event::Mouse(m)) = (app.help, &ev) {
            let max = app.help_max.get();
            match m.kind {
                MouseEventKind::ScrollDown => app.help_scroll = app.help_scroll.saturating_add(3).min(max),
                MouseEventKind::ScrollUp => app.help_scroll = app.help_scroll.saturating_sub(3),
                _ => {}
            }
            continue;
        }
        // 浮層開著時滑鼠不作用，免得點穿到底下的清單。
        if matches!(ev, Event::Mouse(_)) && (app.help || app.search.is_some() || app.msg_search.is_some() || app.path_prompt.is_some() || app.react_picker || app.mention.is_some()) {
            continue;
        }
        if let Event::Mouse(m) = ev {
            let (col, row) = (m.column, m.row);
            let in_list = app.list_area.contains(Position::new(col, row));
            let in_msgs = app.msg_area.contains(Position::new(col, row));
            match m.kind {
                // 滑到點得下去的東西上就換成手指：可開啟的訊息、清單裡的聊天室。
                MouseEventKind::Moved => {
                    let over_msg = app.msg_rects.iter().any(|(r, i)| {
                        r.contains(Position::new(col, row)) && app.msgs[*i].openable()
                    });
                    let over_chat = in_list
                        && row > app.list_area.y
                        && app.list_offset + usize::from((row - app.list_area.y - 1) / app.list_item_rows())
                            < app.chats.len();
                    let now = over_msg || over_chat;
                    if now != hand {
                        hand = now;
                        set_pointer(if hand { "pointer" } else { "default" });
                    }
                }
                MouseEventKind::Down(MouseButton::Left) if in_list => {
                    // 清單每筆佔兩行（名稱＋預覽），扣掉上框線再換算。
                    let inner_top = app.list_area.y + 1;
                    if row >= inner_top {
                        let idx = app.list_offset + ((row - inner_top) / app.list_item_rows()) as usize;
                        if idx < app.chats.len() {
                            app.selected = idx;
                            app.open_selected();
                        }
                    }
                }
                // 點訊息：選中它；點得開的（↗）就直接開。跟面板「點下去」同一個行為。
                MouseEventKind::Down(MouseButton::Left) if in_msgs => {
                    let hit = app
                        .msg_rects
                        .iter()
                        .find(|(r, _)| r.contains(Position::new(col, row)))
                        .map(|(_, i)| *i);
                    if let Some(i) = hit {
                        app.cursor = Some(i);
                        app.open_msg(i);
                    }
                }
                // scroll 是「從最新往回跳過幾則」，所以往上滾要加。
                // 往上＝回溯歷史，跟捲軸與 PageUp 的直覺一致。
                MouseEventKind::ScrollUp if in_msgs => {
                    app.scroll = app.scroll.saturating_add(3);
                    app.on_scrolled();
                }
                MouseEventKind::ScrollDown if in_msgs => app.scroll = app.scroll.saturating_sub(3),
                MouseEventKind::ScrollUp if in_list => {
                    app.selected = app.selected.saturating_sub(1);
                    app.peek = app.open.is_some();
                }
                MouseEventKind::ScrollDown if in_list => {
                    if app.selected + 1 < app.chats.len() {
                        app.selected += 1;
                    }
                    app.peek = app.open.is_some();
                }
                _ => {}
            }
            continue;
        }
        let Event::Key(key) = ev else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.code != KeyCode::Char('d') {
            app.unsend_armed = None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            break Ok(());
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('b') {
            app.sidebar = match app.sidebar {
                Sidebar::Full => Sidebar::Names,
                Sidebar::Names => Sidebar::Hidden,
                Sidebar::Hidden => Sidebar::Full,
            };
            continue;
        }
        // 說明：可以捲（小螢幕放不下整份），Esc／q／?／F1／Enter 關閉。
        if app.help {
            let max = app.help_max.get();
            let page = 10;
            app.help_scroll = match key.code {
                KeyCode::Char('j') | KeyCode::Down => app.help_scroll.saturating_add(1),
                KeyCode::Char('k') | KeyCode::Up => app.help_scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => app.help_scroll.saturating_add(page),
                KeyCode::PageUp => app.help_scroll.saturating_sub(page),
                KeyCode::Home | KeyCode::Char('g') => 0,
                KeyCode::End | KeyCode::Char('G') => max,
                KeyCode::Esc | KeyCode::Enter | KeyCode::F(1) | KeyCode::Char('q') | KeyCode::Char('?') => {
                    app.help = false;
                    app.help_scroll
                }
                _ => app.help_scroll,
            }
            .min(max);
            continue;
        }
        if key.code == KeyCode::F(1) {
            app.help = true;
            app.help_scroll = 0;
            continue;
        }
        // 表情選單：數字選，其他鍵取消。
        if app.react_picker {
            match key.code {
                KeyCode::Char('1') => app.react_selected("NICE"),
                KeyCode::Char('2') => app.react_selected("LOVE"),
                KeyCode::Char('3') => app.react_selected("FUN"),
                KeyCode::Char('4') => app.react_selected("AMAZING"),
                KeyCode::Char('5') => app.react_selected("SAD"),
                KeyCode::Char('6') => app.react_selected("OMG"),
                KeyCode::Char('0') => app.react_selected("UNDO"),
                _ => app.react_picker = false,
            }
            continue;
        }
        // @ 選單：打字篩、↑↓ 選、Enter 放進輸入框、Esc 就只留一個 @。
        if app.mention.is_some() {
            let hits = app.mention_hits();
            let Some(m) = app.mention.as_mut() else { continue };
            match key.code {
                KeyCode::Esc => app.mention = None,
                KeyCode::Up => m.pick = m.pick.saturating_sub(1),
                KeyCode::Down if m.pick + 1 < hits.len() => m.pick += 1,
                KeyCode::Backspace if m.query.is_empty() => {
                    app.mention = None;
                    app.input.pop(); // 連那個 @ 一起刪
                }
                KeyCode::Backspace => {
                    m.query.pop();
                    m.pick = 0;
                }
                KeyCode::Char(c) => {
                    m.query.push(c);
                    m.pick = 0;
                }
                KeyCode::Enter => {
                    if let Some((mid, name)) = hits.get(m.pick).cloned() {
                        app.input.push_str(&format!("{name} ")); // 輸入框裡已經有那個 @
                        app.pending_mentions.push((mid, name));
                    }
                    app.mention = None;
                }
                _ => {}
            }
            continue;
        }
        // 訊息搜尋開著時，按鍵全部歸它。
        if app.msg_search.is_some() {
            let Some(ms) = app.msg_search.as_mut() else { continue };
            let hits: Vec<(String, String)> =
                archive::find(&ms.all, &ms.query).iter().map(|h| (h.chat.clone(), h.from_name.clone())).collect();
            match key.code {
                KeyCode::Esc => app.msg_search = None,
                KeyCode::Up => ms.pick = ms.pick.saturating_sub(1),
                KeyCode::Down if ms.pick + 1 < hits.len() => ms.pick += 1,
                KeyCode::Backspace => {
                    ms.query.pop();
                    ms.pick = 0;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    ms.query.push(c);
                    ms.pick = 0;
                }
                KeyCode::Enter => {
                    if let Some((mid, from)) = hits.get(ms.pick).cloned() {
                        app.msg_search = None;
                        let name = app.chat_name(&mid).unwrap_or(from);
                        app.open_chat(mid, name);
                    }
                }
                _ => {}
            }
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            app.start_msg_search();
            continue;
        }
        // 搜尋開著時，按鍵全部歸它。
        if app.search.is_some() {
            let hits = app.search_hits().len();
            let picked = app.search.as_ref().map_or(0, |s| s.pick);
            match key.code {
                KeyCode::Esc => app.search = None,
                KeyCode::Enter => {
                    let hit = app.search_hits().get(picked).map(|c| (c.mid.clone(), c.name.clone()));
                    if let Some((mid, name)) = hit {
                        app.search = None;
                        // 清單裡有的話游標也移過去，沒有（排在 60 名外）就只開聊天室。
                        if let Some(i) = app.chats.iter().position(|c| c.mid == mid) {
                            app.selected = i;
                        }
                        app.open_chat(mid, name);
                    }
                }
                code => {
                    let Some(search) = app.search.as_mut() else { continue };
                    match code {
                        KeyCode::Up => search.pick = search.pick.saturating_sub(1),
                        KeyCode::Down if search.pick + 1 < hits => search.pick += 1,
                        KeyCode::Backspace => {
                            search.query.pop();
                            search.pick = 0;
                        }
                        KeyCode::Char(c) => {
                            search.query.push(c);
                            search.pick = 0;
                        }
                        _ => {}
                    }
                }
            }
            continue;
        }
        // 路徑輸入框開著時，按鍵全部歸它。
        if let Some(prompt) = app.path_prompt.as_mut() {
            match key.code {
                KeyCode::Esc => app.path_prompt = None,
                KeyCode::Tab => {
                    let completed = complete_path(&prompt.text).0;
                    prompt.set(completed);
                }
                KeyCode::Backspace => {
                    let mut text = prompt.text.clone();
                    text.pop();
                    prompt.set(text);
                }
                KeyCode::Char(c) => {
                    let text = format!("{}{c}", prompt.text);
                    prompt.set(text);
                }
                KeyCode::Enter => {
                    let text = prompt.text.clone();
                    // 是檔案才關掉；打錯了留著讓人改，錯誤顯示在狀態列。
                    if app.attach_file(&text) {
                        app.path_prompt = None;
                    }
                }
                _ => {}
            }
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
            app.toggle_vip();
            continue;
        }
        // 預覽別的聊天室時：只認移動、進入、回去。打字、貼上、附檔、送出都屬於開著的那個，
        // 這時畫面上卻是別人，一律不收。
        if app.peeking().is_some() {
            match key.code {
                KeyCode::Enter => app.open_selected(),
                KeyCode::Esc => app.end_peek(),
                KeyCode::Up => app.selected = app.selected.saturating_sub(1),
                KeyCode::Down if app.selected + 1 < app.chats.len() => app.selected += 1,
                KeyCode::Char(_) => app.status = "預覽中：Enter 進入，Esc 回到原本的聊天室".into(),
                _ => {}
            }
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && app.open.is_some() {
            match key.code {
                KeyCode::Char('v') => {
                    app.paste();
                    continue;
                }
                KeyCode::Char('f') => {
                    let mut prompt = PathPrompt::default();
                    prompt.set("~/".into());
                    app.path_prompt = Some(prompt);
                    continue;
                }
                _ => {}
            }
        }
        match key.code {
            // 輸入框有字時 Enter 永遠是送出，不管有沒有選中訊息 —— 打到一半的字不能被吃掉。
            KeyCode::Enter => {
                if app.open.is_some() && (!app.input.is_empty() || app.attach.is_some()) {
                    app.send_input();
                } else if let Some(i) = app.cursor {
                    app.open_msg(i);
                } else {
                    app.open_selected();
                }
            }
            KeyCode::Tab if app.open.is_some() => app.move_cursor(true),
            KeyCode::BackTab if app.open.is_some() => app.move_cursor(false),
            KeyCode::Backspace if app.open.is_none() => {}
            // 輸入框已經空了再按一次，才拿掉附件。
            KeyCode::Backspace if app.input.is_empty() => app.attach = None,
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::Up => {
                app.selected = app.selected.saturating_sub(1);
                app.peek = app.open.is_some();
            }
            KeyCode::Down => {
                if app.selected + 1 < app.chats.len() {
                    app.selected += 1;
                }
                app.peek = app.open.is_some();
            }
            KeyCode::PageUp => {
                app.scroll = app.scroll.saturating_add(10);
                app.on_scrolled();
            }
            KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(10),
            // 先取消選取，再按一次才回清單。
            KeyCode::Esc if app.cursor.is_some() => app.cursor = None,
            KeyCode::Esc if app.attach.is_some() => {
                app.attach = None;
                app.status.clear();
            }
            KeyCode::Esc => {
                app.stash_draft();
                app.open = None;
                app.attach = None;
                app.msgs.clear();
                app.preview = None; // 回清單後重新預覽（剛才讀過，內容可能變了）
                app.check_session();
            }
            // 選中一則訊息、輸入框是空的：r 表情、d 收回。
            KeyCode::Char('r') if app.cursor.is_some() && app.input.is_empty() => app.react_picker = true,
            KeyCode::Char('d') if app.cursor.is_some() && app.input.is_empty() => app.unsend_selected(),
            // 群組裡打 @：挑人。一對一沒有要提及的對象。
            KeyCode::Char('@')
                if app.open.as_deref().is_some_and(|c| !c.starts_with('u')) =>
            {
                app.input.push('@');
                app.start_mention();
            }
            KeyCode::Char(c) => {
                // 清單模式：字母一律是快捷鍵。輸入框屬於聊天室，沒開聊天室時它不存在 ——
                // 以前這裡還要求輸入框是空的，於是只要有字殘留在輸入框（離開聊天室沒清），
                // q 之類的鍵就全部被吃進一個看不見的輸入框。
                if app.open.is_none() {
                    match c {
                        'j' => {
                            if app.selected + 1 < app.chats.len() {
                                app.selected += 1;
                            }
                        }
                        'k' => app.selected = app.selected.saturating_sub(1),
                        'r' => app.load_chats(),
                        '/' => app.start_search(),
                        '?' => {
                            app.help = true;
                            app.help_scroll = 0;
                        }
                        'q' => break Ok(()),
                        _ => {}
                    }
                } else if app.input.is_empty() && c == 'g' {
                    app.load_older();
                } else if app.input.is_empty() && c == '/' {
                    app.start_search();
                } else {
                    app.input.push(c);
                }
            }
            _ => {}
        }
    };

    if let Some(mut p) = app.player.take() {
        let _ = p.kill(); // 關掉 TUI 時語音也停
    }
    restore_terminal()?;
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_from_colors_toml() {
        let t = parse_theme("accent = \"#e68e0d\"\nselection = \"#333333\"\n\nbackground = \"#000000\"\ncolor4 = \"#112233\"\n");
        assert_eq!(t.accent, Color::Rgb(0xe6, 0x8e, 0x0d));
        assert_eq!(t.selection, Some(Color::Rgb(0x33, 0x33, 0x33)));
        assert_eq!(t.on_accent, Color::Rgb(0, 0, 0));
        // 沒有 accent 的主題退回 color4；壞掉的值當作沒有。
        assert_eq!(parse_theme("color4 = \"#112233\"\naccent = \"oops\"").accent, Color::Rgb(0x11, 0x22, 0x33));
        // 沒有主題檔（不是 omarchy）：用終端機調色盤，沒有選取底色。
        let none = parse_theme("");
        assert_eq!((none.accent, none.selection), (Color::Green, None));
    }

    #[test]
    fn links_are_marked() {
        let text = rich("看這個https://a.example/x 很棒\n第二行", Color::Green);
        let spans: Vec<&str> = text.lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(spans, ["看這個", "https://a.example/x", " 很棒"]);
        assert!(text.lines[0].spans[1].style.add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(text.lines.len(), 2);
    }

    #[test]
    fn paths_from_clipboard_and_drag() {
        // 檔案管理員給的是編碼過的 URI，中文檔名要解得回來。
        assert_eq!(local_path("file:///tmp/%E5%A0%B1%E5%83%B9%20v2.pdf\n"), "/tmp/報價 v2.pdf");
        // 不是 URI 的路徑原樣保留：檔名裡真的有 % 的不能被解掉。
        assert_eq!(local_path("/tmp/100%25.txt"), "/tmp/100%25.txt");
        assert_eq!(percent_decode("a%zzb%4"), "a%zzb%4"); // 壞掉的編碼不動
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(local_path("~/a.png"), format!("{home}/a.png"));
        assert_eq!(human_size(900), "900 B");
        assert_eq!(human_size(347_566), "340 KB");
        assert_eq!(human_size(3 * 1024 * 1024), "3.0 MB");
    }

    #[test]
    fn path_completion() {
        let dir = std::env::temp_dir().join(format!("enil-tui-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("reports")).unwrap();
        for f in ["report-jan.pdf", "report-feb.pdf", "notes.txt", ".hidden"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        let base = format!("{}/", dir.display());
        // 多個候選：補到共同前綴，目錄帶 /。
        let (text, names) = complete_path(&format!("{base}rep"));
        assert_eq!(text, format!("{base}report"));
        assert_eq!(names, ["report-feb.pdf", "report-jan.pdf", "reports/"]);
        // 唯一候選：補完整個名字。
        assert_eq!(complete_path(&format!("{base}no")).0, format!("{base}notes.txt"));
        // 隱藏檔要自己打出那個點才列。
        assert!(!complete_path(&base).1.contains(&".hidden".to_string()));
        assert_eq!(complete_path(&format!("{base}.")).1, [".hidden"]);
        // 沒有符合的：輸入不動。
        assert_eq!(complete_path(&format!("{base}zzz")).0, format!("{base}zzz"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mention_positions_are_utf16() {
        let picked = vec![("u1".to_string(), "小安".to_string()), ("u2".to_string(), "Bob".to_string())];
        // 😀 在 UTF-16 是兩個單位：位置要照那樣算，不是位元組也不是字元數。
        let spans = mention_spans("😀 @小安 跟 @Bob 好", &picked);
        assert_eq!(spans, serde_json::json!([{"mid":"u1","start":3,"end":6},{"mid":"u2","start":9,"end":13}]));
        // 挑了又刪掉的不算。
        assert_eq!(mention_spans("沒有提及", &picked), serde_json::json!([]));
    }

    #[test]
    fn fit_counts_display_width() {
        assert_eq!(fit("短", 10), "短");
        assert_eq!(fit("工程部公告與值班", 9), "工程部公…"); // 寬字元兩格
        assert_eq!(fit("abcdef", 4), "abc…");
    }
}
