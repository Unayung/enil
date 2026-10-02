//! 搜尋 daemon 的訊息存檔（state 目錄的 archive.jsonl）。
//!
//! LINE 伺服器對次要裝置只留約兩週的歷史，這份存檔是唯一的長期紀錄（從 daemon 開始
//! 跑的那天算起）。TUI 跟 daemon 在同一台機器上，直接讀檔，不必多一個 socket 指令。

use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Hit {
    pub chat: String,
    pub from_name: String,
    pub text: String,
    pub time: i64,
}

/// 讀進整份存檔，新到舊。存檔只增不刪、重連時同一則可能寫兩次，所以依 id 去重。
/// ponytail: 一次全讀進記憶體（目前約 0.5 MB）。大到開搜尋會卡再改成串流或建索引。
pub fn load(path: &Path) -> Vec<Hit> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut hits: Vec<Hit> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| seen.insert(s(m, "id")))
        .filter_map(|m| {
            // 純文字訊息是 text；FLEX 的內容在 flexText，其次 altText。
            let text = ["text", "flexText", "altText"].iter().map(|k| s(&m, k)).find(|t| !t.is_empty())?;
            Some(Hit {
                chat: s(&m, "chat"),
                from_name: s(&m, "fromName"),
                text: text.replace('\n', " "),
                time: m.get("time").and_then(Value::as_i64).unwrap_or(0),
            })
        })
        .collect();
    hits.sort_by(|a, b| b.time.cmp(&a.time));
    hits
}

/// 空白分開的每個詞都要出現（不分大小寫），發話者名字也算。
pub fn find<'a>(all: &'a [Hit], query: &str) -> Vec<&'a Hit> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if terms.is_empty() {
        return vec![];
    }
    all.iter()
        .filter(|h| {
            let hay = format!("{} {}", h.text, h.from_name).to_lowercase();
            terms.iter().all(|t| hay.contains(t.as_str()))
        })
        .collect()
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_and_find() {
        let path = std::env::temp_dir().join(format!("enil-archive-test-{}.jsonl", std::process::id()));
        std::fs::write(
            &path,
            [
                r#"{"id":"1","chat":"cA","fromName":"Alice","text":"明天開會\n記得帶筆電","time":100}"#,
                r#"{"id":"2","chat":"cA","fromName":"Bob","text":"","flexText":"訂單已出貨","altText":"您有新訊息","time":200}"#,
                r#"{"id":"1","chat":"cA","fromName":"Alice","text":"明天開會\n記得帶筆電","time":100}"#,
                r#"{"id":"3","chat":"cB","fromName":"Alice","text":"","contentType":"IMAGE","time":300}"#,
                "壞掉的一行",
                r#"{"id":"4","chat":"cB","fromName":"Carol","text":"MEETING moved","time":400}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let all = load(&path);
        std::fs::remove_file(&path).unwrap();

        // 去重、沒有文字的（圖片）不收、壞掉的行跳過、新到舊、換行壓成空白。
        let ids: Vec<i64> = all.iter().map(|h| h.time).collect();
        assert_eq!(ids, [400, 200, 100]);
        assert_eq!(all[2].text, "明天開會 記得帶筆電");
        // FLEX 用 flexText，不是「您有新訊息」。
        assert_eq!(all[1].text, "訂單已出貨");

        assert_eq!(find(&all, "meeting").len(), 1); // 不分大小寫
        assert_eq!(find(&all, "開會 筆電").len(), 1); // 每個詞都要有
        assert_eq!(find(&all, "開會 出貨").len(), 0);
        assert_eq!(find(&all, "alice").len(), 1); // 發話者名字也算
        assert!(find(&all, "   ").is_empty());
    }
}
