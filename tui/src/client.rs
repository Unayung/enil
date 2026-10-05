//! 跟 enil daemon 講話。協定是 unix socket 上一行一個 JSON。
//!
//! 這裡完全不碰 LINE —— 登入、E2EE、輪詢都在 daemon 那側。
//! 每個請求開一條新連線：daemon 支援並行連線，這樣就不必自己管
//! 單一連線上的請求／回應配對。

use serde_json::{json, Value};
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    let base = env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
        format!("{}/.local/state", env::var("HOME").unwrap_or_default())
    });
    PathBuf::from(base).join("enil")
}

pub fn request(req: Value) -> Result<Value, String> {
    let mut stream = UnixStream::connect(state_dir().join("sock"))
        .map_err(|_| "daemon 沒在跑（systemctl --user start enil）".to_string())?;
    let mut line = req.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).map_err(|e| e.to_string())?;

    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| e.to_string())?;
    let res: Value = serde_json::from_str(&buf).map_err(|e| e.to_string())?;
    if res.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(res.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(res
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("未知錯誤")
            .to_string())
    }
}

/// 訂閱 daemon 的推送：一條常駐連線，每則新訊息一行。會一直擋到斷線才回來，
/// 重連由呼叫端決定。舊版 daemon 不認得 subscribe，只會回一行錯誤然後沒下文 ——
/// 那行沒有 event 欄位，被濾掉，行為退回純輪詢。
/// on_event 收到（事件名稱, 資料）：message、read、chat-read、unsend、reaction、edit。
pub fn subscribe(on_open: impl FnOnce(), mut on_event: impl FnMut(&str, Value)) -> Result<(), String> {
    let mut stream = UnixStream::connect(state_dir().join("sock")).map_err(|e| e.to_string())?;
    stream
        .write_all(b"{\"cmd\":\"subscribe\"}\n")
        .map_err(|e| e.to_string())?;
    on_open();
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        if let Some(event) = v.get("event").and_then(Value::as_str) {
            on_event(event, v.get("data").cloned().unwrap_or(Value::Null));
        }
    }
    Ok(())
}

pub fn chats(limit: u32) -> Result<Value, String> {
    request(json!({ "cmd": "chats", "limit": limit }))
}

pub fn history(chat: &str, count: u32, before: Option<&str>) -> Result<Value, String> {
    let mut req = json!({ "cmd": "history", "chat": chat, "count": count });
    match before {
        // 往前翻頁時不動已讀；第一次進入才標記。
        Some(id) => req["before"] = json!(id),
        None => req["markRead"] = json!(true),
    }
    request(req)
}

/// 預覽：最新一頁，但不標已讀。清單上移動游標時用 —— 只是看一眼，不算讀過。
pub fn peek(chat: &str, count: u32) -> Result<Value, String> {
    request(json!({ "cmd": "history", "chat": chat, "count": count }))
}

/// 下載原檔（不是縮圖），回傳 { path }。
pub fn download(message_id: &str) -> Result<Value, String> {
    request(json!({ "cmd": "download", "messageId": message_id, "preview": false }))
}

/// 送檔案或圖片。daemon 依副檔名決定當圖片、影片還是一般檔案送。
pub fn send_file(chat: &str, path: &str) -> Result<Value, String> {
    request(json!({ "cmd": "sendFile", "chat": chat, "path": path }))
}

/// mentions：[{ mid, start, end }]，start／end 是 text 的 UTF-16 索引（LINE 的算法）。
pub fn send(chat: &str, text: &str, mentions: Value) -> Result<Value, String> {
    request(json!({ "cmd": "send", "chat": chat, "text": text, "mentions": mentions }))
}

pub fn unsend(message_id: &str) -> Result<Value, String> {
    request(json!({ "cmd": "unsend", "messageId": message_id }))
}

/// type：NICE／LOVE／FUN／AMAZING／SAD／OMG，UNDO 是收回自己的回應。
pub fn react(message_id: &str, kind: &str) -> Result<Value, String> {
    request(json!({ "cmd": "react", "messageId": message_id, "type": kind }))
}

/// 送貼圖。
pub fn send_sticker(chat: &str, s: &crate::model::Sticker) -> Result<Value, String> {
    request(json!({ "cmd": "sendSticker", "chat": chat, "sticker": { "id": s.id, "pkg": s.pkg, "ver": s.ver } }))
}

/// 某張貼圖的圖檔（daemon 從 CDN 抓好快取），回傳 { path }。
pub fn sticker_image(id: &str) -> Result<Value, String> {
    request(json!({ "cmd": "stickerImage", "id": id }))
}

/// 擁有的貼圖組 [{pkg, ver, name, stickers:[id]}]，最近下載的在前。
pub fn sticker_packs() -> Result<Value, String> {
    request(json!({ "cmd": "stickerPacks" }))
}

/// 好友清單 [{mid, name}]（含還沒聊過的）。
pub fn contacts() -> Result<Value, String> {
    request(json!({ "cmd": "contacts" }))
}

pub fn members(chat: &str) -> Result<Value, String> {
    request(json!({ "cmd": "members", "chat": chat }))
}
