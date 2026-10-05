//! 配色主題。
//!
//! 來源有四種，Ctrl-Y 的選單裡依序列出：
//!   Omarchy   讀 omarchy 目前的主題（colors.toml），換主題時跟著換。沒有那個檔就跟「終端機」一樣
//!   終端機    不設背景，文字與強調色用終端機調色盤的色號 —— 跟終端機的主題走
//!   內建      從 slk 移植的常用主題
//!   自訂      ~/.config/enil/themes/*.toml 與 ~/.config/slk/themes/*.toml，格式跟 slk 一樣
//!
//! 選了哪個存在 ~/.config/enil/theme（一行，主題名稱），沒有就是 Omarchy。

use ratatui::style::Color;
use std::path::PathBuf;
use std::time::SystemTime;

pub const OMARCHY: &str = "Omarchy";
pub const TERMINAL: &str = "終端機";

#[derive(Clone)]
pub struct Theme {
    pub name: String,
    /// 強調色：選取、未讀、焦點所在的面板、連結。
    pub accent: Color,
    /// 印在強調色底上的字。
    pub on_accent: Color,
    /// 選取列的底色。None 時只靠色條與粗體標示。
    pub selection: Option<Color>,
    /// 畫面背景。None = 不畫，用終端機自己的背景。
    pub bg: Option<Color>,
    /// 左邊清單的背景（跟訊息區分得開）。
    pub sidebar_bg: Option<Color>,
    pub text: Option<Color>,
    /// 次要文字（時間、預覽、提示）。None = 用終端機的「暗淡」效果。
    pub muted: Option<Color>,
    /// 沒有焦點的面板外框。None = 跟次要文字一樣。
    pub border: Option<Color>,
    pub warning: Color,
    pub error: Color,
    pub ok: Color,
    /// Omarchy 主題檔的修改時間：變了就重讀（見 refresh）。
    stamp: Option<SystemTime>,
}

impl Theme {
    /// 「終端機」：全部用調色盤的色號，終端機換配色時跟著變。
    pub fn terminal() -> Theme {
        Theme {
            name: TERMINAL.into(),
            accent: Color::Green,
            on_accent: Color::Black,
            selection: None,
            bg: None,
            sidebar_bg: None,
            text: None,
            muted: None,
            border: None,
            warning: Color::Yellow,
            error: Color::Red,
            ok: Color::Green,
            stamp: None,
        }
    }

    /// 依名稱載入；找不到就退回 Omarchy。
    pub fn load(name: &str) -> Theme {
        if name == TERMINAL {
            return Theme::terminal();
        }
        if let Some(t) = builtin(name) {
            return t;
        }
        if let Some(path) = custom_themes().into_iter().find(|(n, _)| n == name).map(|(_, p)| p) {
            if let Some(t) = std::fs::read_to_string(path).ok().and_then(|raw| parse_slk(&raw)) {
                return t;
            }
        }
        omarchy()
    }

    /// 只有 Omarchy 要跟著外面變。omarchy 換主題是把整個目錄刪掉再搬新的進來，中間有一瞬間
    /// 檔案不存在 —— 那時先維持原樣，不要閃回預設色。
    pub fn refresh(&mut self) {
        if self.name != OMARCHY {
            return;
        }
        let stamp = std::fs::metadata(omarchy_path()).and_then(|m| m.modified()).ok();
        if stamp.is_some() && stamp != self.stamp {
            *self = omarchy();
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn omarchy_path() -> PathBuf {
    let state = std::env::var("XDG_STATE_HOME").map(PathBuf::from).unwrap_or_else(|_| home().join(".local/state"));
    state.join("omarchy/current/theme/colors.toml")
}

fn hex(s: &str) -> Option<Color> {
    let h = s.trim().trim_matches('"').strip_prefix('#')?;
    let n = u32::from_str_radix(h, 16).ok().filter(|_| h.len() == 6)?;
    Some(Color::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

/// 一行一個 `key = "#rrggbb"` 的檔（omarchy 的 colors.toml、slk 主題的 [colors] 都是）。
/// 只取用得到的鍵，不為此拉進 toml。
fn lookup<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    raw.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then_some(v.trim())
    })
}

/// 往黑或白推一點（amount 0..1）。側欄背景要跟訊息區分得出來，omarchy 只給一個背景色。
fn shade(c: Color, amount: f32) -> Color {
    let Color::Rgb(r, g, b) = c else { return c };
    let dark = (r as u32 + g as u32 + b as u32) < 3 * 128;
    let mix = |v: u8| {
        let target = if dark { 255.0 } else { 0.0 };
        (v as f32 + (target - v as f32) * amount).round() as u8
    };
    Color::Rgb(mix(r), mix(g), mix(b))
}

/// Omarchy 目前的主題。沒有主題檔（不是 omarchy）就跟「終端機」一樣，只是名字留著，
/// 之後裝了或換了主題 refresh 會接上。
pub fn omarchy() -> Theme {
    let path = omarchy_path();
    let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut t = parse_omarchy(&std::fs::read_to_string(&path).unwrap_or_default());
    t.stamp = stamp;
    t
}

pub fn parse_omarchy(raw: &str) -> Theme {
    let get = |k: &str| lookup(raw, k).and_then(hex);
    let mut t = Theme::terminal();
    t.name = OMARCHY.into();
    if raw.trim().is_empty() {
        return t;
    }
    t.accent = get("accent").or_else(|| get("color4")).unwrap_or(Color::Green);
    t.selection = get("selection");
    t.bg = get("background");
    t.on_accent = t.bg.unwrap_or(Color::Black);
    t.sidebar_bg = t.bg.map(|b| shade(b, 0.04));
    t.text = get("foreground");
    t.muted = get("color8");
    t.border = get("color8");
    t.warning = get("color3").unwrap_or(Color::Yellow);
    t.error = get("color1").unwrap_or(Color::Red);
    t.ok = get("color2").unwrap_or(Color::Green);
    t
}

/// slk 的主題檔：`name = "…"`，加一段 [colors]。
pub fn parse_slk(raw: &str) -> Option<Theme> {
    let name = lookup(raw, "name")?.trim_matches('"').to_string();
    let get = |k: &str| lookup(raw, k).and_then(hex);
    let bg = get("background")?;
    Some(Theme {
        name,
        accent: get("primary")?,
        on_accent: bg,
        selection: get("selection_background").or_else(|| get("surface")),
        bg: Some(bg),
        sidebar_bg: get("sidebar_background").or_else(|| get("surface_dark")),
        text: get("text"),
        muted: get("text_muted"),
        border: get("border"),
        warning: get("warning").unwrap_or(Color::Yellow),
        error: get("error").unwrap_or(Color::Red),
        ok: get("accent").unwrap_or(Color::Green),
        stamp: None,
    })
}

/// （名稱, primary, accent, warning, error, background, surface, sidebar, text, muted, border）
/// 色碼照抄 slk 的內建主題。
const BUILTIN: &[(&str, [&str; 10])] = &[
    ("Catppuccin Mocha", ["#89B4FA", "#A6E3A1", "#F9E2AF", "#F38BA8", "#1E1E2E", "#313244", "#0E0E16", "#CDD6F4", "#6C7086", "#45475A"]),
    ("Catppuccin Latte", ["#1E66F5", "#40A02B", "#DF8E1D", "#D20F39", "#EFF1F5", "#E6E9EF", "#DCE0E8", "#4C4F69", "#6C6F85", "#BCC0CC"]),
    ("Tokyo Night", ["#7AA2F7", "#9ECE6A", "#E0AF68", "#F7768E", "#1A1B26", "#24283B", "#262838", "#C0CAF5", "#565F89", "#3B4261"]),
    ("Dracula", ["#BD93F9", "#50FA7B", "#FFB86C", "#FF5555", "#282A36", "#343746", "#14151C", "#F8F8F2", "#6272A4", "#44475A"]),
    ("Gruvbox Dark", ["#83A598", "#B8BB26", "#FABD2F", "#FB4934", "#282828", "#3C3836", "#181818", "#EBDBB2", "#928374", "#504945"]),
    ("Nord", ["#88C0D0", "#A3BE8C", "#EBCB8B", "#BF616A", "#2E3440", "#3B4252", "#1B2028", "#ECEFF4", "#7B88A1", "#4C566A"]),
    ("One Dark", ["#61AFEF", "#98C379", "#E5C07B", "#E06C75", "#282C34", "#2C313C", "#1A1D23", "#ABB2BF", "#636D83", "#3E4452"]),
    ("Rosé Pine", ["#C4A7E7", "#9CCFD8", "#F6C177", "#EB6F92", "#191724", "#1F1D2E", "#262433", "#E0DEF4", "#6E6A86", "#26233A"]),
    ("Solarized Dark", ["#268BD2", "#859900", "#B58900", "#DC322F", "#002B36", "#073642", "#001820", "#839496", "#586E75", "#073642"]),
    // Solarized Light 在 slk 裡側欄是深色的；這裡讓側欄跟主體同色系，淺色主題就整片淺色。
    ("Solarized Light", ["#268BD2", "#859900", "#B58900", "#DC322F", "#FDF6E3", "#EEE8D5", "#EEE8D5", "#657B83", "#93A1A1", "#EEE8D5"]),
    ("Everforest Dark", ["#7FBBB3", "#A7C080", "#DBBC7F", "#E67E80", "#2D353B", "#343F44", "#1E2429", "#D3C6AA", "#859289", "#3D484D"]),
    ("Kanagawa", ["#7FB4CA", "#98BB6C", "#E6C384", "#E46876", "#1F1F28", "#2A2A37", "#0C0C10", "#DCD7BA", "#727169", "#363646"]),
    ("GitHub Dark", ["#58A6FF", "#3FB950", "#D29922", "#F85149", "#0D1117", "#161B22", "#1C2128", "#C9D1D9", "#8B949E", "#30363D"]),
    ("Monokai", ["#66D9EF", "#A6E22E", "#E6DB74", "#F92672", "#272822", "#3E3D32", "#181811", "#F8F8F2", "#75715E", "#49483E"]),
    ("Cyberpunk Neon", ["#0ABDC6", "#00FF9C", "#FCEE0C", "#EA00D9", "#000B1E", "#0D1B2A", "#0D1B2A", "#D7D7D7", "#7E7E8E", "#133E7C"]),
];

fn builtin(name: &str) -> Option<Theme> {
    let (n, c) = BUILTIN.iter().find(|(n, _)| n.eq_ignore_ascii_case(name))?;
    let h = |i: usize| hex(c[i]).unwrap_or(Color::Reset);
    Some(Theme {
        name: n.to_string(),
        accent: h(0),
        on_accent: h(4),
        selection: Some(h(5)),
        bg: Some(h(4)),
        sidebar_bg: Some(h(6)),
        text: Some(h(7)),
        muted: Some(h(8)),
        border: Some(h(9)),
        warning: h(2),
        error: h(3),
        ok: h(1),
        stamp: None,
    })
}

/// 自訂主題（名稱, 檔案）：enil 自己的目錄優先，同名的 slk 主題略過。
pub fn custom_themes() -> Vec<(String, PathBuf)> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for dir in [config_dir().join("enil/themes"), config_dir().join("slk/themes")] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        files.sort();
        for path in files {
            let Some(name) = std::fs::read_to_string(&path).ok().and_then(|raw| parse_slk(&raw)).map(|t| t.name) else { continue };
            if !out.iter().any(|(n, _)| *n == name) {
                out.push((name, path));
            }
        }
    }
    out
}

/// Ctrl-Y 選單的順序：Omarchy、終端機、內建、自訂。
pub fn all_names() -> Vec<String> {
    let mut names = vec![OMARCHY.to_string(), TERMINAL.to_string()];
    names.extend(BUILTIN.iter().map(|(n, _)| n.to_string()));
    for (n, _) in custom_themes() {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    names
}

fn config_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| home().join(".config"))
}

fn choice_path() -> PathBuf {
    config_dir().join("enil/theme")
}

pub fn saved_choice() -> String {
    std::fs::read_to_string(choice_path()).map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| OMARCHY.into())
}

pub fn save_choice(name: &str) -> std::io::Result<()> {
    let path = choice_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{name}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omarchy_colors() {
        let t = parse_omarchy("accent = \"#e68e0d\"\nselection = \"#333333\"\nbackground = \"#000000\"\nforeground = \"#efefef\"\ncolor8 = \"#8a8a8d\"\ncolor1 = \"#d35f5f\"\n");
        assert_eq!(t.name, OMARCHY);
        assert_eq!(t.accent, Color::Rgb(0xe6, 0x8e, 0x0d));
        assert_eq!(t.bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(t.text, Some(Color::Rgb(0xef, 0xef, 0xef)));
        assert_eq!(t.muted, Some(Color::Rgb(0x8a, 0x8a, 0x8d)));
        assert_eq!(t.error, Color::Rgb(0xd3, 0x5f, 0x5f));
        // 側欄比背景亮一點（深色主題），跟訊息區分得出來。
        assert_eq!(t.sidebar_bg, Some(Color::Rgb(10, 10, 10)));
        // 沒有 accent 退回 color4；沒有主題檔就跟終端機一樣、不畫背景。
        assert_eq!(parse_omarchy("color4 = \"#112233\"").accent, Color::Rgb(0x11, 0x22, 0x33));
        let none = parse_omarchy("");
        assert_eq!((none.name.as_str(), none.bg, none.accent), (OMARCHY, None, Color::Green));
    }

    #[test]
    fn slk_theme_file() {
        let raw = "name = \"My Theme\"\n\n[colors]\nprimary = \"#BD93F9\"\naccent = \"#50FA7B\"\nbackground = \"#282A36\"\nsurface = \"#343746\"\ntext = \"#F8F8F2\"\ntext_muted = \"#6272A4\"\n";
        let t = parse_slk(raw).unwrap();
        assert_eq!(t.name, "My Theme");
        assert_eq!(t.accent, Color::Rgb(0xBD, 0x93, 0xF9));
        assert_eq!(t.ok, Color::Rgb(0x50, 0xFA, 0x7B));
        assert_eq!(t.selection, Some(Color::Rgb(0x34, 0x37, 0x46))); // 沒寫 selection 用 surface
        // 缺了必要的欄位（背景、primary、名字）就不算主題。
        assert!(parse_slk("name = \"x\"\nprimary = \"#000000\"").is_none());
    }

    #[test]
    fn names_and_lookup() {
        let names = all_names();
        assert_eq!(&names[..2], [OMARCHY, TERMINAL]);
        assert!(names.contains(&"Catppuccin Mocha".to_string()));
        assert_eq!(Theme::load("catppuccin mocha").name, "Catppuccin Mocha"); // 大小寫不拘
        assert_eq!(Theme::load(TERMINAL).bg, None);
        assert_eq!(Theme::load("不存在的主題").name, OMARCHY);
    }
}
