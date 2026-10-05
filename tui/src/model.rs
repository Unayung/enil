//! daemon 回傳的資料形狀。只取 UI 會用到的欄位。

use serde_json::Value;

#[derive(Clone, Debug)]
pub struct Chat {
    pub mid: String,
    pub name: String,
    pub unread: i64,
    pub last_text: String,
    /// 最後一則的發話者與時間（epoch 毫秒，沒有就是 0）。
    pub last_from: String,
    pub last_time: i64,
    /// 大頭貼的本機路徑（一對一是對方的照片，群組是群組圖）。
    pub avatar_path: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub id: String,
    pub from_name: String,
    pub text: String,
    pub time: i64,
    pub mine: bool,
    /// 要畫的圖的本機路徑：圖片縮圖、貼圖、或 FLEX 的第一張圖。
    /// daemon 讀歷史時就抓好了，這裡直接拿來畫。
    pub media_path: Option<String>,
    /// contentType 原樣（IMAGE / STICKER / FLEX …）。
    pub kind: String,
    /// 有原檔可以跟 daemon 要（圖片／影片／語音／檔案）。
    pub has_media: bool,
    /// 這則可以開的網址：FLEX 的第一個連結，或文字裡的第一個網址。
    pub link: Option<String>,
    /// 發話者的 mid。大頭貼依它快取。
    pub from: String,
    /// 發話者大頭貼的本機路徑。推送來的訊息沒有，靠同一個人先前的訊息補。
    pub avatar_path: Option<String>,
    /// 自己發的有幾個人讀了。
    pub read_count: i64,
    /// 表情回應的種類（NICE、LOVE…），一人一筆。
    pub reactions: Vec<String>,
    /// @ 了誰（mid；"all" 是 @所有人）。
    pub mentions: Vec<String>,
    /// 貼圖訊息的參數（id, 貼圖組, 版本）。送一張一樣的貼圖要這三個。
    pub sticker: Option<Sticker>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sticker {
    pub id: String,
    pub pkg: String,
    pub ver: String,
}

/// 表情回應的種類 → 畫面上的符號。
pub fn reaction_glyph(kind: &str) -> &'static str {
    match kind {
        "NICE" => "👍",
        "LOVE" => "❤️",
        "FUN" => "😆",
        "AMAZING" => "😮",
        "SAD" => "😢",
        "OMG" => "😱",
        _ => "✨", // LINE 的自訂／付費回應
    }
}

/// 一則訊息的表情統計，例如「👍 2  ❤️ 1」，出現多的在前。
pub fn reaction_summary(kinds: &[String]) -> String {
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for k in kinds.iter().filter(|k| k.as_str() != "UNDO") {
        match counts.iter_mut().find(|(g, _)| *g == reaction_glyph(k)) {
            Some(c) => c.1 += 1,
            None => counts.push((reaction_glyph(k), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    counts.iter().map(|(g, n)| if *n > 1 { format!("{g} {n}") } else { g.to_string() }).collect::<Vec<_>>().join("  ")
}

impl Msg {
    pub fn openable(&self) -> bool {
        self.has_media || self.link.is_some()
    }
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// 物件陣列裡每個物件的某個字串欄位，例如 reactions[].type。
fn strings(v: &Value, k: &str, field: &str) -> Vec<String> {
    v.get(k)
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| s(x, field)).collect())
        .unwrap_or_default()
}

fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

pub fn parse_chats(v: &Value) -> Vec<Chat> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| Chat {
                    mid: s(c, "mid"),
                    name: {
                        let name = s(c, "name");
                        if name.is_empty() { s(c, "mid") } else { name }
                    },
                    unread: n(c, "unread"),
                    last_text: s(c, "lastText").replace('\n', " "),
                    last_from: s(c, "lastFrom"),
                    last_time: n(c, "lastTime"),
                    avatar_path: Some(s(c, "avatar")).filter(|p| !p.is_empty()),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_msgs(v: &Value, me: &str) -> Vec<Msg> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    // mediaPath 是圖片縮圖，thumbPath 是貼圖或 FLEX 的圖 —— 畫法一樣。
                    let media_path = [s(m, "mediaPath"), s(m, "thumbPath")]
                        .into_iter()
                        .find(|p| !p.is_empty());
                    let text = body(m, media_path.is_some());
                    let link = m
                        .get("flexLinks")
                        .and_then(|l| l.get(0))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or_else(|| first_url(&s(m, "text")))
                        .filter(|u| is_web_url(u));
                    Msg {
                        id: s(m, "id"),
                        from_name: s(m, "fromName"),
                        text,
                        time: n(m, "time"),
                        mine: s(m, "from") == me,
                        media_path,
                        kind: s(m, "contentType"),
                        has_media: m.get("hasMedia").and_then(Value::as_bool) == Some(true),
                        link,
                        from: s(m, "from"),
                        avatar_path: Some(s(m, "fromAvatar")).filter(|p| !p.is_empty()),
                        read_count: n(m, "readCount"),
                        reactions: strings(m, "reactions", "type"),
                        mentions: m
                            .get("mentions")
                            .and_then(Value::as_array)
                            .map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
                            .unwrap_or_default(),
                        sticker: m.get("sticker").map(|v| Sticker { id: s(v, "id"), pkg: s(v, "pkg"), ver: s(v, "ver") }).filter(|k| !k.id.is_empty()),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 訊息本文。有文字就是文字；沒有的依型別給一個讀得懂的標示。
/// has_image 為真時圖片與貼圖不需要文字 —— 圖本身就是內容。
fn body(m: &Value, has_image: bool) -> String {
    let text = s(m, "text");
    let alt = s(m, "altText");
    let kind = s(m, "contentType");
    match kind.as_str() {
        // 通話紀錄的 text 是 "Call History : 109000 millisecs, Result: …"，不是給人看的。
        "CALL" => call_label(&text),
        _ if !text.is_empty() => text,
        // 解密失敗只對「本來該有文字」的訊息有意義；E2EE 的圖片與檔案本來就沒有 text。
        "NONE" if m.get("decryptFailed").and_then(Value::as_bool) == Some(true) => {
            "[E2EE 解密失敗]".into()
        }
        // altText 常常只是「您有新訊息」，卡片內文在 flexText。
        "FLEX" => [s(m, "flexText"), alt].into_iter().find(|t| !t.is_empty()).unwrap_or("[FLEX]".into()),
        "IMAGE" if has_image => String::new(),
        "STICKER" if has_image => String::new(),
        "IMAGE" => "[圖片]".into(),
        "STICKER" => "[貼圖]".into(),
        "VIDEO" => format!("[影片]{}{}", length(m), size_suffix(m)),
        // 語音的大小沒什麼意義，長度才是。
        "AUDIO" => format!("[語音]{}", length(m)),
        "FILE" => {
            let name = s(m, "fileName");
            format!("[檔案] {}{}", if name.is_empty() { "未命名" } else { &name }, size_suffix(m))
        }
        "CHATEVENT" => "[聊天室事件]".into(),
        _ if !alt.is_empty() => alt,
        _ => format!("[{kind}]"),
    }
}

/// 影片／語音長度，例如 " 0:12"。沒有就是空字串。
fn length(m: &Value) -> String {
    match n(m, "duration") / 1000 {
        s if s <= 0 => String::new(),
        s => format!(" {}:{:02}", s / 60, s % 60),
    }
}

fn size_suffix(m: &Value) -> String {
    match n(m, "fileSize") {
        b if b <= 0 => String::new(),
        b if b < 1024 * 1024 => format!("（{} KB）", (b + 1023) / 1024),
        b => format!("（{:.1} MB）", b as f64 / 1048576.0),
    }
}

fn call_label(text: &str) -> String {
    let ms: i64 = text
        .split(" millisecs")
        .next()
        .and_then(|head| head.rsplit(' ').next())
        .and_then(|d| d.parse().ok())
        .unwrap_or(0);
    if ms <= 0 {
        "[通話] 未接通".into()
    } else {
        format!("[通話] {}:{:02}", ms / 60_000, ms / 1000 % 60)
    }
}

fn first_url(text: &str) -> Option<String> {
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    Some(text[start..].split_whitespace().next()?.to_string())
}

/// 只開 http(s)。連結是對方給的，file:// 或自訂 scheme 交給 xdg-open 等於讓對方
/// 挑一個本機程式來執行。
fn is_web_url(u: &str) -> bool {
    u.starts_with("https://") || u.starts_with("http://")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(v: Value) -> Msg {
        parse_msgs(&json!([v]), "uME").pop().unwrap()
    }

    #[test]
    fn body_per_type() {
        assert_eq!(one(json!({"contentType":"NONE","text":"嗨"})).text, "嗨");
        assert_eq!(one(json!({"contentType":"NONE","text":"","decryptFailed":true})).text, "[E2EE 解密失敗]");
        // 有圖就不需要文字；沒抓到圖才退回標示。
        assert_eq!(one(json!({"contentType":"IMAGE","hasMedia":true,"mediaPath":"/a.jpg"})).text, "");
        assert_eq!(one(json!({"contentType":"IMAGE","hasMedia":true})).text, "[圖片]");
        let stk = one(json!({"contentType":"STICKER","thumbPath":"/s.thumb"}));
        assert_eq!((stk.text.as_str(), stk.media_path.as_deref()), ("", Some("/s.thumb")));
        assert!(!stk.openable());
        // E2EE 的檔案 decryptFailed 可能是 true，但那不該蓋掉檔名。
        let f = one(json!({"contentType":"FILE","hasMedia":true,"decryptFailed":true,"fileName":"報價.pdf","fileSize":347566}));
        assert_eq!(f.text, "[檔案] 報價.pdf（340 KB）");
        assert!(f.openable());
        assert_eq!(one(json!({"contentType":"VIDEO","hasMedia":true,"fileSize":3145728})).text, "[影片]（3.0 MB）");
        assert_eq!(one(json!({"contentType":"VIDEO","hasMedia":true,"fileSize":3145728,"duration":25000})).text, "[影片] 0:25（3.0 MB）");
        assert_eq!(one(json!({"contentType":"AUDIO","hasMedia":true,"fileSize":9000,"duration":72500})).text, "[語音] 1:12");
        assert_eq!(one(json!({"contentType":"CALL","text":"Call History : 109000 millisecs, Result: "})).text, "[通話] 1:49");
        assert_eq!(one(json!({"contentType":"CALL","text":"Call History : 0 millisecs, Result: 77"})).text, "[通話] 未接通");
        assert_eq!(one(json!({"contentType":"RICH","altText":"優惠"})).text, "優惠");
        assert_eq!(one(json!({"contentType":"LOCATION"})).text, "[LOCATION]");
    }

    #[test]
    fn reactions_and_reads() {
        let m = one(json!({"contentType":"NONE","text":"x","from":"uME","readCount":3,
            "reactions":[{"from":"u1","type":"NICE"},{"from":"u2","type":"LOVE"},{"from":"u3","type":"NICE"},{"from":"u4"}],
            "mentions":["uME","all"]}));
        assert_eq!(m.read_count, 3);
        assert_eq!(reaction_summary(&m.reactions), "👍 2  ❤️  ✨");
        assert_eq!(m.mentions, ["uME", "all"]);
        assert_eq!(reaction_summary(&[]), "");
        assert_eq!(reaction_summary(&["UNDO".into()]), ""); // 收回的回應不算
    }

    #[test]
    fn flex_and_links() {
        let m = one(json!({"contentType":"FLEX","altText":"您有新訊息","flexText":"商品\nNT$990",
            "flexLinks":["https://example.com/a"]}));
        assert_eq!(m.text, "商品\nNT$990");
        assert_eq!(m.link.as_deref(), Some("https://example.com/a"));
        // 文字裡的網址，前面黏著中文也要找得到。
        let t = one(json!({"contentType":"NONE","text":"看這個https://example.com/x 很棒"}));
        assert_eq!(t.link.as_deref(), Some("https://example.com/x"));
        // 非 http(s) 的連結不開。
        let bad = one(json!({"contentType":"FLEX","altText":"x","flexLinks":["file:///etc/passwd"]}));
        assert_eq!(bad.link, None);
        assert!(!bad.openable());
    }
}

const TZ_OFFSET_SECS: i64 = 8 * 3600; // 台北

/// epoch 毫秒 → 本地 HH:MM。避免為了這個拉進 chrono。
pub fn clock(ms: i64) -> String {
    let secs = ms / 1000 + TZ_OFFSET_SECS;
    let mins = (secs / 60) % 60;
    let hours = (secs / 3600) % 24;
    format!("{hours:02}:{mins:02}")
}

/// 本地日的序號。只用來判斷「跨日了沒」。
pub fn day_index(ms: i64) -> i64 {
    (ms / 1000 + TZ_OFFSET_SECS) / 86_400
}

/// 日期分隔線用的：MM/DD 加星期。1970-01-01 是星期四。
pub fn day_label_long(ms: i64) -> String {
    const WEEK: [&str; 7] = ["四", "五", "六", "日", "一", "二", "三"];
    format!("{} 週{}", day_label(ms), WEEK[day_index(ms).rem_euclid(7) as usize])
}

/// 日序號 → MM/DD。只顯示 HH:MM 的話，昨天 19:30 排在今天 15:01 前面會看起來像亂序。
pub fn day_label(ms: i64) -> String {
    let days = day_index(ms);
    // 1970-01-01 起算的民用曆換算，避免拉進日期函式庫。
    let (mut y, mut d) = (1970_i64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if d < len { break; }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 0;
    while mo < 12 && d >= months[mo] {
        d -= months[mo];
        mo += 1;
    }
    format!("{:02}/{:02}", mo + 1, d + 1)
}
