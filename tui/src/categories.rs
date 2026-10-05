//! 聊天室分類：使用者自己分的，加上自動的預設區。
//!
//! 清單分區順序：★ VIP → 自己建的分類（依建立順序）→ 群組 → 一對一。
//! 已經是 VIP 的聊天室放在 VIP 區，不管有沒有分類。
//!
//! 存在 state 目錄（跟著帳號走，跟 VIP 名單一樣）：
//!   categories  一行一筆「mid<Tab>分類」，分類的順序就是第一次出現的順序
//!   collapsed   一行一個收合起來的分區名稱

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const VIP: &str = "★ VIP";
pub const GROUPS: &str = "群組";
pub const DIRECT: &str = "一對一";

/// 這些名字是自動分區用的，使用者不能拿來當分類名稱 —— 不然清單上會有兩個同名的區。
pub fn reserved(name: &str) -> bool {
    [VIP, GROUPS, DIRECT, "VIP"].contains(&name.trim())
}

#[derive(Default)]
pub struct Categories {
    /// （mid, 分類），依建立順序。一個聊天室只會出現一次。
    assigned: Vec<(String, String)>,
    pub collapsed: HashSet<String>,
    dir: PathBuf,
}

impl Categories {
    pub fn load(dir: &Path) -> Categories {
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
        let assigned = read("categories")
            .lines()
            .filter_map(|l| {
                let (mid, cat) = l.split_once('\t')?;
                let (mid, cat) = (mid.trim(), cat.trim());
                (!mid.is_empty() && !cat.is_empty() && !reserved(cat)).then(|| (mid.to_string(), cat.to_string()))
            })
            .collect();
        let collapsed = read("collapsed").lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
        Categories { assigned, collapsed, dir: dir.to_path_buf() }
    }

    fn save(&self) -> std::io::Result<()> {
        let cats: String = self.assigned.iter().map(|(m, c)| format!("{m}\t{c}\n")).collect();
        std::fs::write(self.dir.join("categories"), cats)?;
        let mut collapsed: Vec<&str> = self.collapsed.iter().map(String::as_str).collect();
        collapsed.sort();
        std::fs::write(self.dir.join("collapsed"), collapsed.iter().map(|c| format!("{c}\n")).collect::<String>())
    }

    pub fn of(&self, mid: &str) -> Option<&str> {
        self.assigned.iter().find(|(m, _)| m == mid).map(|(_, c)| c.as_str())
    }

    /// 自己建的分類，依建立順序。
    pub fn names(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for (_, c) in &self.assigned {
            if !out.contains(&c.as_str()) {
                out.push(c);
            }
        }
        out
    }

    /// 把聊天室放進某個分類；None = 不分類（回到自動區）。
    pub fn set(&mut self, mid: &str, cat: Option<&str>) -> std::io::Result<()> {
        let cat = cat.map(str::trim).filter(|c| !c.is_empty() && !reserved(c));
        match self.assigned.iter_mut().find(|(m, _)| m == mid) {
            // 原地改：分類的順序是「第一次出現」，換分類不該讓別的分類跳位置。
            Some(entry) if cat.is_some() => entry.1 = cat.unwrap().to_string(),
            Some(_) => self.assigned.retain(|(m, _)| m != mid),
            None => {
                if let Some(c) = cat {
                    self.assigned.push((mid.to_string(), c.to_string()));
                }
            }
        }
        self.save()
    }

    pub fn toggle_collapsed(&mut self, section: &str) -> std::io::Result<()> {
        if !self.collapsed.remove(section) {
            self.collapsed.insert(section.to_string());
        }
        self.save()
    }

    pub fn expand(&mut self, section: &str) -> std::io::Result<()> {
        if self.collapsed.remove(section) { self.save() } else { Ok(()) }
    }

    /// 這個聊天室在哪一區，以及那一區的排序（數字小的在上面）。
    pub fn section(&self, mid: &str, vip: bool) -> (String, usize) {
        if vip {
            return (VIP.into(), 0);
        }
        if let Some(cat) = self.of(mid) {
            let rank = self.names().iter().position(|n| *n == cat).unwrap_or(0);
            return (cat.to_string(), 1 + rank);
        }
        // mid 開頭：u 是人（一對一），c 是群組，r 是舊式的多人聊天室。
        if mid.starts_with('u') { (DIRECT.into(), usize::MAX) } else { (GROUPS.into(), usize::MAX - 1) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("enil-cats-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sections_and_order() {
        let dir = temp();
        let mut c = Categories::load(&dir);
        // 沒分類：依 mid 自動分；VIP 永遠最上面。
        assert_eq!(c.section("uA", false).0, DIRECT);
        assert_eq!(c.section("cG", false).0, GROUPS);
        assert_eq!(c.section("uA", true), (VIP.into(), 0));
        assert!(c.section("cG", false).1 < c.section("uA", false).1); // 群組在一對一上面

        c.set("uA", Some("家人")).unwrap();
        c.set("cG", Some("工作")).unwrap();
        c.set("uB", Some("家人")).unwrap();
        assert_eq!(c.names(), ["家人", "工作"]); // 建立順序
        assert_eq!(c.section("uB", false), ("家人".into(), 1));
        assert_eq!(c.section("cG", false), ("工作".into(), 2));
        assert!(c.section("cG", false).1 < c.section("uZ", false).1); // 自己的分類在自動區上面

        // 換分類不會讓別的分類跳位置；不分類就回到自動區。
        c.set("uA", Some("工作")).unwrap();
        assert_eq!(c.names(), ["工作", "家人"]); // uA 原地改成工作，家人只剩 uB
        c.set("uB", None).unwrap();
        assert_eq!(c.section("uB", false).0, DIRECT);

        // 保留名稱不能當分類。
        c.set("uC", Some("群組")).unwrap();
        assert_eq!(c.of("uC"), None);

        // 存檔讀回來一樣，收合狀態也記得。
        c.toggle_collapsed("工作").unwrap();
        let again = Categories::load(&dir);
        assert_eq!(again.of("cG"), Some("工作"));
        assert!(again.collapsed.contains("工作"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
