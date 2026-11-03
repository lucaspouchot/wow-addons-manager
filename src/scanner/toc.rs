//! `.toc` file parsing.

use std::collections::HashMap;

/// Metadata fields of one TOC file. Keys are lowercase.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TocData {
    fields: HashMap<String, String>,
}

pub const VERSION_PLACEHOLDER: &str = "@project-version@";

impl TocData {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .get(&key.to_ascii_lowercase())
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    /// Display title without WoW color codes.
    pub fn title(&self) -> Option<String> {
        self.get("Title")
            .map(strip_color_codes)
            .filter(|t| !t.is_empty())
    }

    /// Localized titles (`Title-frFR`, …), color codes removed.
    pub fn localized_titles(&self) -> Vec<(String, String)> {
        let mut out: Vec<_> = self
            .fields
            .iter()
            .filter_map(|(k, v)| {
                k.strip_prefix("title-")
                    .map(|loc| (loc.to_string(), strip_color_codes(v)))
            })
            .collect();
        out.sort();
        out
    }

    /// Raw `## Version` value, if any.
    pub fn raw_version(&self) -> Option<&str> {
        self.get("Version")
    }

    /// `## Version`, or `None` when it is the unreplaced packager placeholder.
    pub fn version(&self) -> Option<&str> {
        self.raw_version().filter(|v| !is_placeholder(v))
    }

    pub fn has_version_placeholder(&self) -> bool {
        self.raw_version().is_some_and(is_placeholder)
    }

    /// `## Interface`: one value or a comma-separated list. Invalid parts are skipped.
    pub fn interfaces(&self) -> (Vec<u32>, bool) {
        let Some(raw) = self.get("Interface") else {
            return (Vec::new(), false);
        };
        let mut invalid = false;
        let mut out = Vec::new();
        for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            match part.parse::<u32>() {
                Ok(v) => out.push(v),
                Err(_) => invalid = true,
            }
        }
        (out, invalid)
    }

    /// `## X-Curse-Project-ID`, if present.
    pub fn curse_project_id(&self) -> Option<&str> {
        self.get("X-Curse-Project-ID")
    }

    /// Required dependencies (`Dependencies`, `RequiredDeps`).
    pub fn dependencies(&self) -> Vec<String> {
        let mut out = Vec::new();
        for key in ["Dependencies", "RequiredDeps"] {
            if let Some(v) = self.get(key) {
                out.extend(split_list(v));
            }
        }
        out
    }

    pub fn optional_dependencies(&self) -> Vec<String> {
        self.get("OptionalDeps").map(split_list).unwrap_or_default()
    }
}

fn is_placeholder(v: &str) -> bool {
    v.trim().eq_ignore_ascii_case(VERSION_PLACEHOLDER)
}

fn split_list(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse TOC bytes: UTF-8 with or without BOM, LF or CRLF. Never fails.
pub fn parse_toc(bytes: &[u8]) -> TocData {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let text = String::from_utf8_lossy(bytes);
    let mut fields = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let Some(rest) = line.strip_prefix("##") else {
            continue;
        };
        let Some((key, val)) = rest.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() {
            continue;
        }
        // First occurrence wins, like the game client.
        fields.entry(key).or_insert_with(|| val.trim().to_string());
    }
    TocData { fields }
}

/// Remove `|cAARRGGBB` and `|r` color codes.
pub fn strip_color_codes(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '|' && i + 1 < chars.len() {
            match chars[i + 1] {
                'c' | 'C'
                    if i + 10 <= chars.len()
                        && chars[i + 2..i + 10].iter().all(|c| c.is_ascii_hexdigit()) =>
                {
                    i += 10;
                    continue;
                }
                'r' | 'R' => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_toc() {
        let t = parse_toc(
            b"## Interface: 16001\n## Title: Bartender4\n## Version: 4.15.0\n\
              ## X-Curse-Project-ID: 111\n## Dependencies: Ace3, LibStub\nBartender4.lua\n",
        );
        assert_eq!(t.title().as_deref(), Some("Bartender4"));
        assert_eq!(t.version(), Some("4.15.0"));
        assert_eq!(t.interfaces(), (vec![16001], false));
        assert_eq!(t.curse_project_id(), Some("111"));
        assert_eq!(t.dependencies(), vec!["Ace3", "LibStub"]);
    }

    #[test]
    fn bom_and_crlf() {
        let t = parse_toc(b"\xEF\xBB\xBF## Title: X\r\n## Version: 1.0\r\n");
        assert_eq!(t.title().as_deref(), Some("X"));
        assert_eq!(t.version(), Some("1.0"));
    }

    #[test]
    fn without_version() {
        let t = parse_toc(b"## Title: X\n");
        assert_eq!(t.version(), None);
        assert!(!t.has_version_placeholder());
    }

    #[test]
    fn placeholder_version() {
        let t = parse_toc(b"## Version: @project-version@\n");
        assert_eq!(t.version(), None);
        assert!(t.has_version_placeholder());
    }

    #[test]
    fn several_interfaces() {
        let t = parse_toc(b"## Interface: 16001, 110200 ,11507\n");
        assert_eq!(t.interfaces(), (vec![16001, 110200, 11507], false));
        let t = parse_toc(b"## Interface: 16001, abc\n");
        assert_eq!(t.interfaces(), (vec![16001], true));
    }

    #[test]
    fn case_insensitive_keys() {
        let t = parse_toc(b"## x-curse-project-id: 42\n## TITLE: Y\n");
        assert_eq!(t.curse_project_id(), Some("42"));
        assert_eq!(t.title().as_deref(), Some("Y"));
    }

    #[test]
    fn color_codes() {
        assert_eq!(
            strip_color_codes("|cff00ff00Details!|r Damage"),
            "Details! Damage"
        );
        assert_eq!(strip_color_codes("plain|x"), "plain|x");
        let t = parse_toc(b"## Title-frFR: |cffffffffNom|r\n");
        assert_eq!(
            t.localized_titles(),
            vec![("frfr".to_string(), "Nom".to_string())]
        );
    }
}
