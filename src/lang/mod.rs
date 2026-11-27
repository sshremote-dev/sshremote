//! The program's words in the reader's language. The source says everything in English; each
//! other language is a table from those English words to its own (`assets/lang/<code>.json`).
//! `t!("...")` translates a literal where it is shown, `t!("... {} ...", x)` fills one in, and
//! `n!("...")` only marks a literal kept for later (`tr`, `tr_msg` when it is shown).

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicPtr, Ordering};


/// Translates a literal (`t!("Open")`), or fills a translated template with values in order
/// (`t!("Sent {} files", n)`).
macro_rules! t {
    ($s:literal) => {
        $crate::lang::tr($s)
    };
    ($s:literal, $($a:expr),+ $(,)?) => {
        $crate::lang::fill($crate::lang::tr($s), &[$(&$a as &dyn ::std::fmt::Display),+])
    };
}

/// Marks a literal for translation where it is shown later, through `tr` or `tr_msg`.
macro_rules! n {
    ($s:literal) => {
        $s
    };
}

pub struct Lang {
    /// A BCP 47 tag, also the table's file name.
    pub code: &'static str,
    /// The language's name in itself.
    pub name: &'static str,
    /// The English name, for sorting and search.
    pub english: &'static str,
    /// Its table, deflated by the build (empty for English).
    table: &'static [u8],
}

macro_rules! langs {
    ($(($code:literal, $name:literal, $english:expr)),+ $(,)?) => {
        pub const LANGS: &[Lang] = &[
            Lang { code: "en", name: "English", english: n!("English"), table: b"" },
            $(Lang { code: $code, name: $name, english: $english, table: include_bytes!(concat!(env!("OUT_DIR"), "/lang/", $code, ".json.z")) }),+
        ];
    };
}

langs![
    ("ar", "العربية", n!("Arabic")),
    ("az", "Azərbaycan dili", n!("Azerbaijani")),
    ("be", "Беларуская", n!("Belarusian")),
    ("bg", "Български", n!("Bulgarian")),
    ("ca", "Català", n!("Catalan")),
    ("cs", "Čeština", n!("Czech")),
    ("da", "Dansk", n!("Danish")),
    ("de", "Deutsch", n!("German")),
    ("el", "Ελληνικά", n!("Greek")),
    ("eo", "Esperanto", n!("Esperanto")),
    ("es", "Español", n!("Spanish")),
    ("et", "Eesti", n!("Estonian")),
    ("eu", "Euskara", n!("Basque")),
    ("fa", "فارسی", n!("Persian")),
    ("fi", "Suomi", n!("Finnish")),
    ("fr", "Français", n!("French")),
    ("gl", "Galego", n!("Galician")),
    ("gu", "ગુજરાતી", n!("Gujarati")),
    ("he", "עברית", n!("Hebrew")),
    ("hi", "हिन्दी", n!("Hindi")),
    ("hr", "Hrvatski", n!("Croatian")),
    ("hu", "Magyar", n!("Hungarian")),
    ("id", "Bahasa Indonesia", n!("Indonesian")),
    ("it", "Italiano", n!("Italian")),
    ("ja", "日本語", n!("Japanese")),
    ("ka", "ქართული", n!("Georgian")),
    ("kk", "Қазақ тілі", n!("Kazakh")),
    ("ko", "한국어", n!("Korean")),
    ("lt", "Lietuvių", n!("Lithuanian")),
    ("lv", "Latviešu", n!("Latvian")),
    ("ml", "മലയാളം", n!("Malayalam")),
    ("nb", "Norsk bokmål", n!("Norwegian Bokmål")),
    ("nl", "Nederlands", n!("Dutch")),
    ("pl", "Polski", n!("Polish")),
    ("pt-BR", "Português (Brasil)", n!("Portuguese (Brazil)")),
    ("pt-PT", "Português (Portugal)", n!("Portuguese (Portugal)")),
    ("ro", "Română", n!("Romanian")),
    ("ru", "Русский", n!("Russian")),
    ("sc", "Sardu", n!("Sardinian")),
    ("sk", "Slovenčina", n!("Slovak")),
    ("sl", "Slovenščina", n!("Slovenian")),
    ("sq", "Shqip", n!("Albanian")),
    ("sr", "Srpski", n!("Serbian")),
    ("sv", "Svenska", n!("Swedish")),
    ("ta", "தமிழ்", n!("Tamil")),
    ("th", "ไทย", n!("Thai")),
    ("tr", "Türkçe", n!("Turkish")),
    ("uk", "Українська", n!("Ukrainian")),
    ("ur", "اردو", n!("Urdu")),
    ("vi", "Tiếng Việt", n!("Vietnamese")),
    ("zh-CN", "简体中文", n!("Chinese (Simplified)")),
    ("zh-TW", "繁體中文", n!("Chinese (Traditional)")),
];

struct Table {
    code: &'static str,
    words: HashMap<&'static str, &'static str>,
    /// Templates with `{}` in them, for messages made elsewhere (`tr_msg`).
    templates: Vec<(&'static str, &'static str)>,
}

/// The language now; null for English. Tables are kept once made (a change of language is rare).
static NOW: AtomicPtr<Table> = AtomicPtr::new(std::ptr::null_mut());

fn now() -> Option<&'static Table> {
    // SAFETY: only ever a leaked, never freed Table, or null.
    unsafe { NOW.load(Ordering::Acquire).as_ref() }
}

/// A language's table as its file has it (English to its words), for the web viewer: inflated
/// once and kept.
pub fn table_json(code: &str) -> Option<&'static str> {
    static KEPT: std::sync::Mutex<Vec<(&'static str, &'static str)>> = std::sync::Mutex::new(Vec::new());
    let lang = LANGS.iter().find(|l| l.code == code)?;
    if lang.table.is_empty() {
        return Some("{}");
    }
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, t)) = kept.iter().find(|(c, _)| *c == lang.code) {
        return Some(t);
    }
    let text: &'static str = Box::leak(inflate(lang)?.into_boxed_str());
    kept.push((lang.code, text));
    Some(text)
}

/// A table's JSON, as the build deflated it.
fn inflate(lang: &Lang) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::DeflateDecoder::new(lang.table).read_to_string(&mut text).ok()?;
    Some(text)
}

/// The language now, "en" when none is chosen.
pub fn current() -> &'static str {
    now().map_or("en", |t| t.code)
}

/// A literal in the language now.
pub fn tr(s: &'static str) -> &'static str {
    match now() {
        Some(t) => t.words.get(s).copied().unwrap_or(s),
        None => s,
    }
}

/// Words made at run time (a message from the other side, a kept label) in the language now:
/// the same words or a template that matches them, else as they are.
pub fn tr_msg(s: &str) -> Cow<'_, str> {
    translated(s, 2)
}

/// `s` translated whole, as a list of words translated one by one, or by a template whose values
/// are translated in turn (a message inside a message), `depth` levels down.
fn translated(s: &str, depth: u8) -> Cow<'_, str> {
    let Some(t) = now() else { return Cow::Borrowed(s) };
    if let Some(w) = t.words.get(s) {
        return Cow::Borrowed(w);
    }
    // "keyboard and mouse, files": each known on its own.
    if s.contains(", ") {
        let items: Vec<&str> = s.split(", ").collect();
        if items.iter().all(|i| t.words.contains_key(i)) {
            return Cow::Owned(items.iter().map(|i| t.words[i]).collect::<Vec<_>>().join(tr(", ")));
        }
    }
    if depth == 0 {
        return Cow::Borrowed(s);
    }
    for (en, to) in &t.templates {
        if let Some(parts) = matches(en, s) {
            let parts: Vec<Cow<str>> = parts.into_iter().map(|p| translated(p, depth - 1)).collect();
            let args: Vec<&dyn Display> = parts.iter().map(|p| p as &dyn Display).collect();
            return Cow::Owned(fill(to, &args));
        }
    }
    Cow::Borrowed(s)
}

/// What fills each `{}` of `template` to make `s`, if it does.
fn matches<'a>(template: &str, s: &'a str) -> Option<Vec<&'a str>> {
    let pieces: Vec<&str> = template.split("{}").collect();
    if pieces.len() < 2 {
        return None;
    }
    let mut rest = s.strip_prefix(pieces[0])?;
    let mut parts = Vec::new();
    for (i, piece) in pieces.iter().enumerate().skip(1) {
        let last = i + 1 == pieces.len();
        let at = if last {
            if !rest.ends_with(piece) {
                return None;
            }
            rest.len() - piece.len()
        } else if piece.is_empty() {
            return None;
        } else {
            rest.find(piece)?
        };
        if at == 0 {
            return None;
        }
        parts.push(&rest[..at]);
        rest = &rest[at + piece.len()..];
    }
    rest.is_empty().then_some(parts)
}

/// A template with its values: `{}` takes them in order, `{0}`, `{1}` by place (a language that
/// says them in another order), `{{` and `}}` are braces.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(template.len() + 16);
    let mut next = 0;
    let mut chars = template.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '{' if chars.peek().map(|p| p.1) == Some('{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek().map(|p| p.1) == Some('}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let Some(end) = template[i..].find('}') else {
                    out.push_str(&template[i..]);
                    break;
                };
                let inside = &template[i + 1..i + end];
                let at = if inside.is_empty() {
                    next += 1;
                    Some(next - 1)
                } else {
                    inside.parse::<usize>().ok()
                };
                match at.and_then(|a| args.get(a)) {
                    Some(a) => {
                        let _ = write!(out, "{a}");
                    }
                    None => out.push_str(&template[i..=i + end]),
                }
                while chars.peek().is_some_and(|p| p.0 <= i + end) {
                    chars.next();
                }
            }
            c => out.push(c),
        }
    }
    out
}

fn table(lang: &'static Lang) -> Option<Table> {
    let raw: HashMap<String, String> = match inflate(lang).ok_or("not inflated".to_string()).and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("sshremote: the {} words do not read: {e}", lang.english);
            return None;
        }
    };
    let mut words = HashMap::with_capacity(raw.len());
    let mut templates = Vec::new();
    for (en, to) in raw {
        if to.is_empty() {
            continue;
        }
        let en: &'static str = Box::leak(en.into_boxed_str());
        let has_slot = en.contains("{}");
        let to: &'static str = Box::leak(to.into_boxed_str());
        if has_slot {
            templates.push((en, to));
        }
        words.insert(en, to);
    }
    // Longer templates first, so the most specific one matches a message.
    templates.sort_by_key(|t| std::cmp::Reverse(t.0.len()));
    Some(Table { code: lang.code, words, templates })
}

/// Speaks `code` from now on ("" for the system's language, "en" for none).
pub fn set(code: &str) {
    let code = if code.is_empty() { system() } else { code };
    let Some(lang) = find(code).filter(|l| l.code != "en") else {
        NOW.store(std::ptr::null_mut(), Ordering::Release);
        return;
    };
    if now().is_some_and(|t| t.code == lang.code) {
        return;
    }
    match table(lang) {
        Some(t) => NOW.store(Box::leak(Box::new(t)), Ordering::Release),
        None => NOW.store(std::ptr::null_mut(), Ordering::Release),
    }
}

/// The language chosen in Settings ("" for the system's).
pub fn chosen() -> String {
    crate::hosts::setting("language").unwrap_or_default()
}

pub fn choose(code: &str) {
    crate::hosts::set_setting("language", code);
    set(code);
}

/// The language this program speaks at start: the chosen one, else the system's.
pub fn start() {
    set(&chosen());
}

/// The table for a tag such as "pt-BR", "pt_BR.UTF-8", "zh-Hant-TW" or "nn", or its nearest.
pub fn find(tag: &str) -> Option<&'static Lang> {
    let tag = tag.split(['.', '@']).next().unwrap_or("").replace('_', "-");
    let lower = tag.to_ascii_lowercase();
    let mut parts = lower.split('-');
    let base = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let code = match base {
        "zh" => {
            if rest.iter().any(|p| matches!(*p, "hant" | "tw" | "hk" | "mo")) {
                "zh-TW"
            } else {
                "zh-CN"
            }
        }
        "pt" => {
            if rest.contains(&"br") {
                "pt-BR"
            } else {
                "pt-PT"
            }
        }
        "no" | "nn" | "nb" => "nb",
        "iw" => "he",
        "in" => "id",
        "ge" => "ka",
        "kz" => "kk",
        b => b,
    };
    LANGS.iter().find(|l| l.code.eq_ignore_ascii_case(code))
}

/// The system's language as one of ours, "en" when it is none of them. Tests speak English, or
/// SSHREMOTE_TEST_LANG (to look the windows over in another language).
pub fn system() -> &'static str {
    if cfg!(test) {
        return std::env::var("SSHREMOTE_TEST_LANG").ok().and_then(|l| find(&l)).map_or("en", |l| l.code);
    }
    system_tags().iter().find_map(|t| find(t)).map_or("en", |l| l.code)
}

/// The languages the person prefers, most wanted first.
fn system_tags() -> Vec<String> {
    let mut tags = Vec::new();
    // LANGUAGE lists several (GNU); LC_ALL, LC_MESSAGES and LANG name one.
    if let Ok(list) = std::env::var("LANGUAGE") {
        tags.extend(list.split(':').filter(|t| !t.is_empty()).map(str::to_string));
    }
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(key)
            && !v.is_empty()
        {
            tags.push(v);
            break;
        }
    }
    tags.retain(|t| t != "C" && t != "POSIX" && !t.starts_with("C."));
    tags.splice(0..0, os_tags());
    tags
}

#[cfg(windows)]
fn os_tags() -> Vec<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};
    let (mut count, mut len) = (0u32, 0u32);
    // SAFETY: sized first, then filled into a buffer of that size.
    unsafe {
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, std::ptr::null_mut(), &mut len) == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, buf.as_mut_ptr(), &mut len) == 0 {
            return Vec::new();
        }
        buf.split(|&c| c == 0).filter(|s| !s.is_empty()).map(String::from_utf16_lossy).collect()
    }
}

#[cfg(target_os = "macos")]
fn os_tags() -> Vec<String> {
    // The order of Language & Region; GUI programs started from the Finder have no LANG.
    std::process::Command::new("/usr/bin/defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .filter(|s| s.len() >= 2)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn os_tags() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fill_in_order_or_by_place() {
        assert_eq!(fill("Sent {} files to {}", &[&3, &"pc"]), "Sent 3 files to pc");
        assert_eq!(fill("{1}에 파일 {0}개를 보냈습니다", &[&3, &"pc"]), "pc에 파일 3개를 보냈습니다");
        assert_eq!(fill("{{}} {}", &[&1]), "{} 1");
        assert_eq!(fill("no {2} here", &[&1]), "no {2} here");
    }

    #[test]
    fn messages_match_their_templates() {
        assert_eq!(matches("Wrong password for {}", "Wrong password for pc"), Some(vec!["pc"]));
        assert_eq!(matches("{} of {} done", "3 of 9 done"), Some(vec!["3", "9"]));
        assert_eq!(matches("{} of {} done", "3 of 9 done!"), None);
        assert_eq!(matches("Open", "Open"), None);
    }

    #[test]
    fn system_tags_find_their_language() {
        let code = |t: &str| find(t).map(|l| l.code);
        assert_eq!(code("ko_KR.UTF-8"), Some("ko"));
        assert_eq!(code("zh-Hant-TW"), Some("zh-TW"));
        assert_eq!(code("zh_CN"), Some("zh-CN"));
        assert_eq!(code("pt_BR"), Some("pt-BR"));
        assert_eq!(code("pt"), Some("pt-PT"));
        assert_eq!(code("nn_NO"), Some("nb"));
        assert_eq!(code("en-US"), Some("en"));
        assert_eq!(code("xx"), None);
    }

    /// Hebrew beside Latin words and digits, and over two rows, in reading order (the vendored
    /// epaint's bidi layout), with the system's Hebrew font where it has one.
    #[cfg(all(feature = "gui", target_os = "linux"))]
    #[test]
    fn right_to_left_text_reads_right_to_left() {
        use eframe::egui::epaint::text::{FontData, FontDefinitions, FontFamily, FontId, Fonts, LayoutJob, TextOptions};
        let files = ["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf"];
        if files.iter().any(|f| !std::path::Path::new(f).exists()) {
            return;
        }
        let mut defs = FontDefinitions::empty();
        for (i, f) in files.iter().enumerate() {
            defs.font_data.insert(format!("f{i}"), std::sync::Arc::new(FontData::from_owned(std::fs::read(f).unwrap())));
        }
        defs.families.insert(FontFamily::Proportional, vec!["f0".into(), "f1".into()]);
        let mut fonts = Fonts::new(TextOptions::default(), defs);
        let mut view = fonts.with_pixels_per_point(1.0);
        // Hebrew letters stand for Latin ones: alef A, bet B, gimel G, dalet D...
        let latin = |c: char| "\u{5d0}\u{5d1}\u{5d2}\u{5d3}\u{5d4}\u{5d5}\u{5d6}\u{5d7}".find(c).map_or(c, |i| (b'A' + (i / 2) as u8) as char);
        let mut rows = |text: &str, width: f32| -> Vec<String> {
            let mut job = LayoutJob::simple(text.to_string(), FontId::proportional(16.0), Default::default(), width);
            job.wrap.max_width = width;
            let g = view.layout_job(job);
            g.rows
                .iter()
                .map(|r| {
                    let mut v: Vec<(f32, char)> = r.row.glyphs.iter().map(|g| (g.pos.x, latin(g.chr))).collect();
                    v.sort_by(|a, b| a.0.total_cmp(&b.0));
                    v.into_iter().map(|p| p.1).collect::<String>().trim().to_string()
                })
                .collect()
        };
        // Each word shaped right to left shows its letters reversed; the words go right to left,
        // the Latin and the digits keep their own order.
        assert_eq!(rows("\u{5d0}\u{5d1} SSH 12 \u{5d2}\u{5d3}", f32::INFINITY), ["DC SSH 12 BA"]);
        assert_eq!(rows("Open \u{5d0}\u{5d1} now", f32::INFINITY), ["Open BA now"]);
        let two = rows("\u{5d0}\u{5d1} \u{5d2}\u{5d3} \u{5d4}\u{5d5} \u{5d6}\u{5d7} \u{5d0}\u{5d1} \u{5d2}\u{5d3}", 70.0);
        assert!(two.len() >= 2, "{two:?}");
        // The first row holds the first words, the rightmost first.
        assert!(two[0].ends_with("BA"), "{two:?}");
    }

    #[test]
    fn every_table_reads_and_keeps_its_slots() {
        for lang in LANGS.iter().filter(|l| l.code != "en") {
            let raw: HashMap<String, String> = serde_json::from_str(&inflate(lang).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", lang.code));
            for (en, to) in &raw {
                if to.is_empty() {
                    continue;
                }
                let slots = |s: &str| {
                    let mut v: Vec<usize> = Vec::new();
                    let mut next = 0;
                    let mut rest = s.replace("{{", "").replace("}}", "");
                    while let Some(a) = rest.find('{') {
                        let Some(b) = rest[a..].find('}') else { break };
                        let inside = &rest[a + 1..a + b];
                        v.push(if inside.is_empty() {
                            next += 1;
                            next - 1
                        } else {
                            inside.parse().unwrap_or(usize::MAX)
                        });
                        rest = rest[a + b + 1..].to_string();
                    }
                    v.sort();
                    v
                };
                assert_eq!(slots(en), slots(to), "{}: {en:?} -> {to:?}", lang.code);
            }
        }
    }
}
