//! Lossless INI reader/writer for Unreal Engine style configuration files.
//!
//! VALORANT stores its local configuration as Unreal Engine `.ini` files. These
//! files may contain comments, blank lines, repeated keys (array entries such as
//! `+ActionMappings=`), keys with `+`/`-`/`.`/`!` prefixes and either CRLF or LF
//! line endings. VALSYNC must never damage a file it does not fully understand,
//! so this parser keeps every line it does not modify byte-for-byte intact.

use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// `key=value` entry. The key is stored trimmed; the value is stored verbatim
    /// (without the line terminator).
    Entry { key: String, value: String },
    /// Comment, blank line or anything else we keep verbatim.
    Raw(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniDoc {
    /// Lines before the first section header.
    pub preamble: Vec<Line>,
    pub sections: Vec<Section>,
    crlf: bool,
    bom: bool,
    trailing_newline: bool,
}

impl Default for IniDoc {
    fn default() -> Self {
        Self {
            preamble: Vec::new(),
            sections: Vec::new(),
            crlf: true, // VALORANT is a Windows game; new files use CRLF.
            bom: false,
            trailing_newline: true,
        }
    }
}

fn parse_line(raw: &str) -> Line {
    let trimmed = raw.trim_start();
    if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
        return Line::Raw(raw.to_string());
    }
    match raw.find('=') {
        Some(idx) if idx > 0 => {
            let key = raw[..idx].trim();
            if key.is_empty() {
                Line::Raw(raw.to_string())
            } else {
                Line::Entry { key: key.to_string(), value: raw[idx + 1..].to_string() }
            }
        }
        _ => Line::Raw(raw.to_string()),
    }
}

impl IniDoc {
    pub fn parse(input: &str) -> Self {
        let (bom, text) = match input.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, input),
        };
        let crlf = text.contains("\r\n") || text.is_empty();
        let trailing_newline = text.is_empty() || text.ends_with('\n');
        let mut doc = IniDoc { preamble: Vec::new(), sections: Vec::new(), crlf, bom, trailing_newline };
        let body = text.strip_suffix('\n').unwrap_or(text);
        if text.is_empty() {
            return doc;
        }
        for raw in body.split('\n') {
            let raw = raw.strip_suffix('\r').unwrap_or(raw);
            let t = raw.trim();
            if t.starts_with('[') && t.ends_with(']') && t.len() >= 2 {
                doc.sections.push(Section { name: t[1..t.len() - 1].to_string(), lines: Vec::new() });
                continue;
            }
            let line = parse_line(raw);
            match doc.sections.last_mut() {
                Some(s) => s.lines.push(line),
                None => doc.preamble.push(line),
            }
        }
        doc
    }

    /// Parse bytes, tolerating invalid UTF-8 (replaced) and UTF-16 LE files with a BOM.
    pub fn parse_bytes(bytes: &[u8]) -> Self {
        if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
            let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            return Self::parse(&String::from_utf16_lossy(&units));
        }
        Self::parse(&String::from_utf8_lossy(bytes))
    }

    pub fn to_string_lossless(&self) -> String {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        let mut lines: Vec<String> = Vec::new();
        for l in &self.preamble {
            lines.push(render_line(l));
        }
        for s in &self.sections {
            lines.push(format!("[{}]", s.name));
            for l in &s.lines {
                lines.push(render_line(l));
            }
        }
        for (i, l) in lines.iter().enumerate() {
            out.push_str(l);
            if i + 1 < lines.len() || self.trailing_newline {
                out.push_str(nl);
            }
        }
        out
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_string_lossless().into_bytes()
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// All values for `key` in `section` (repeated keys produce several values).
    pub fn get(&self, section: &str, key: &str) -> Vec<String> {
        let mut out = Vec::new();
        for s in self.sections.iter().filter(|s| s.name.eq_ignore_ascii_case(section)) {
            for l in &s.lines {
                if let Line::Entry { key: k, value } = l
                    && k.eq_ignore_ascii_case(key)
                {
                    out.push(value.clone());
                }
            }
        }
        out
    }

    pub fn get_first(&self, section: &str, key: &str) -> Option<String> {
        self.get(section, key).into_iter().next()
    }

    /// Iterate `(section, key, value)` for every entry, in file order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.sections.iter().flat_map(|s| {
            s.lines.iter().filter_map(move |l| match l {
                Line::Entry { key, value } => Some((s.name.as_str(), key.as_str(), value.as_str())),
                Line::Raw(_) => None,
            })
        })
    }

    /// Grouped entries: `(section, key) -> values` preserving first-seen order.
    pub fn grouped(&self) -> Vec<(String, String, Vec<String>)> {
        let mut out: Vec<(String, String, Vec<String>)> = Vec::new();
        for (s, k, v) in self.entries() {
            if let Some(e) = out.iter_mut().find(|e| e.0.eq_ignore_ascii_case(s) && e.1.eq_ignore_ascii_case(k)) {
                e.2.push(v.to_string());
            } else {
                out.push((s.to_string(), k.to_string(), vec![v.to_string()]));
            }
        }
        out
    }

    /// Replace every occurrence of `key` within `section` with `values`.
    /// New values are inserted where the first old occurrence was; if the key
    /// did not exist they are appended to the section (created if necessary).
    pub fn set(&mut self, section: &str, key: &str, values: &[String]) {
        let idx = match self.sections.iter().position(|s| s.name.eq_ignore_ascii_case(section)) {
            Some(i) => i,
            None => {
                self.sections.push(Section { name: section.to_string(), lines: Vec::new() });
                self.sections.len() - 1
            }
        };
        // Remove the key from any duplicate sections with the same name, keep first section.
        for (i, s) in self.sections.iter_mut().enumerate() {
            if i != idx && s.name.eq_ignore_ascii_case(section) {
                s.lines.retain(|l| !matches!(l, Line::Entry { key: k, .. } if k.eq_ignore_ascii_case(key)));
            }
        }
        let sec = &mut self.sections[idx];
        let first = sec.lines.iter().position(|l| matches!(l, Line::Entry { key: k, .. } if k.eq_ignore_ascii_case(key)));
        let existing_key = first.and_then(|i| match &sec.lines[i] {
            Line::Entry { key, .. } => Some(key.clone()),
            _ => None,
        });
        let key_name = existing_key.unwrap_or_else(|| key.to_string());
        sec.lines.retain(|l| !matches!(l, Line::Entry { key: k, .. } if k.eq_ignore_ascii_case(key)));
        let new_lines: Vec<Line> =
            values.iter().map(|v| Line::Entry { key: key_name.clone(), value: v.clone() }).collect();
        let insert_at = match first {
            Some(i) => i.min(sec.lines.len()),
            None => {
                // Append before trailing blank lines so sections stay visually separated.
                let mut end = sec.lines.len();
                while end > 0 && matches!(&sec.lines[end - 1], Line::Raw(r) if r.trim().is_empty()) {
                    end -= 1;
                }
                end
            }
        };
        for (n, l) in new_lines.into_iter().enumerate() {
            sec.lines.insert(insert_at + n, l);
        }
    }

    pub fn remove(&mut self, section: &str, key: &str) {
        for s in self.sections.iter_mut().filter(|s| s.name.eq_ignore_ascii_case(section)) {
            s.lines.retain(|l| !matches!(l, Line::Entry { key: k, .. } if k.eq_ignore_ascii_case(key)));
        }
    }

    pub fn debug_dump(&self) -> String {
        let mut s = String::new();
        for (sec, k, v) in self.entries() {
            let _ = writeln!(s, "[{sec}] {k}={v}");
        }
        s
    }
}

fn render_line(l: &Line) -> String {
    match l {
        Line::Entry { key, value } => format!("{key}={value}"),
        Line::Raw(r) => r.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "; comment\r\n[/Script/ShooterGame.ShooterGameUserSettings]\r\nResolutionSizeX=1920\r\n+Arr=1\r\n+Arr=2\r\n\r\n[ScalabilityGroups]\r\nsg.ShadowQuality=0\r\n";

    #[test]
    fn roundtrip_is_lossless() {
        let doc = IniDoc::parse(SAMPLE);
        assert_eq!(doc.to_string_lossless(), SAMPLE);
        let lf = SAMPLE.replace("\r\n", "\n");
        assert_eq!(IniDoc::parse(&lf).to_string_lossless(), lf);
        let no_trailing = "[A]\nx=1";
        assert_eq!(IniDoc::parse(no_trailing).to_string_lossless(), no_trailing);
    }

    #[test]
    fn set_replaces_and_appends() {
        let mut doc = IniDoc::parse(SAMPLE);
        doc.set("/Script/ShooterGame.ShooterGameUserSettings", "+Arr", &["9".into()]);
        assert_eq!(doc.get("/Script/ShooterGame.ShooterGameUserSettings", "+Arr"), vec!["9"]);
        doc.set("ScalabilityGroups", "sg.TextureQuality", &["3".into()]);
        assert_eq!(doc.get_first("scalabilitygroups", "SG.TEXTUREQUALITY").as_deref(), Some("3"));
        doc.set("NewSection", "K", &["V".into()]);
        assert!(doc.to_string_lossless().contains("[NewSection]\r\nK=V\r\n"));
    }

    #[test]
    fn values_keep_equals_signs() {
        let doc = IniDoc::parse("[S]\nCode=0;P;c;5;h;0=1\n");
        assert_eq!(doc.get_first("S", "Code").as_deref(), Some("0;P;c;5;h;0=1"));
    }
}
