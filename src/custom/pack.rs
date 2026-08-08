//! 定制包数据结构、加载与字段级合并。

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::PACK_MANIFEST_FILE;

#[derive(Debug, Clone, Default)]
pub struct CustomPack {
    pub id: String,
    pub description: String,
    pub format_version: u32,
    pub messages: BTreeMap<String, MessageSpec>,
    pub layout: Layout,
    /// 哪些 layout 字段在本包中被显式设置（合并时只覆盖这些）。
    pub layout_set: LayoutSet,
    pub gloss: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MessageSpec {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LayoutSet {
    pub repl_prompt: bool,
    pub repl_continuation: bool,
    pub parse_label: bool,
    pub parse_arrow: bool,
    pub tb_header: bool,
    pub tb_frame: bool,
    pub tb_direction: bool,
    pub exc_line: bool,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub repl: ReplLayout,
    pub parse: ParseLayout,
    pub traceback: TracebackLayout,
    pub exception: ExceptionLayout,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            repl: ReplLayout::default(),
            parse: ParseLayout::default(),
            traceback: TracebackLayout::default(),
            exception: ExceptionLayout::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplLayout {
    pub prompt: String,
    pub continuation: String,
}

impl Default for ReplLayout {
    fn default() -> Self {
        Self {
            prompt: ">>> ".into(),
            continuation: "... ".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParseLayout {
    pub label_error: String,
    pub arrow: String,
}

impl Default for ParseLayout {
    fn default() -> Self {
        Self {
            label_error: "error: ".into(),
            arrow: " --> ".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TracebackLayout {
    pub header: String,
    pub frame: String,
    pub direction: String,
}

impl Default for TracebackLayout {
    fn default() -> Self {
        Self {
            header: "Traceback (most recent call last):".into(),
            frame: "  File \"{file}\", line {line}, in {func}".into(),
            direction: "top_down".into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExceptionLayout {
    pub line: String,
}

impl Default for ExceptionLayout {
    fn default() -> Self {
        Self {
            line: "{name}: {msg}".into(),
        }
    }
}

#[derive(Debug)]
pub enum PackLoadError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for PackLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackLoadError::Io(s) | PackLoadError::Parse(s) | PackLoadError::Invalid(s) => {
                write!(f, "{s}")
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct PackFile {
    id: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_format_version")]
    format_version: u32,
    #[serde(default)]
    messages: BTreeMap<String, MessageSpec>,
    #[serde(default)]
    layout: LayoutFile,
    #[serde(default)]
    gloss: BTreeMap<String, String>,
}

fn default_format_version() -> u32 {
    1
}

#[derive(Debug, Default, Deserialize)]
struct LayoutFile {
    #[serde(default)]
    repl: ReplLayoutFile,
    #[serde(default)]
    parse: ParseLayoutFile,
    #[serde(default)]
    traceback: TracebackLayoutFile,
    #[serde(default)]
    exception: ExceptionLayoutFile,
}

#[derive(Debug, Default, Deserialize)]
struct ReplLayoutFile {
    prompt: Option<String>,
    continuation: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ParseLayoutFile {
    label_error: Option<String>,
    arrow: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct TracebackLayoutFile {
    header: Option<String>,
    frame: Option<String>,
    direction: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ExceptionLayoutFile {
    line: Option<String>,
}

const ALLOWED_PLACEHOLDERS: &[&str] = &[
    "file", "line", "column", "func", "name", "gloss", "msg", "text",
];

fn validate_template(tmpl: &str, ctx: &str) -> Result<(), PackLoadError> {
    let mut rest = tmpl;
    while let Some(i) = rest.find('{') {
        let after = &rest[i + 1..];
        let Some(j) = after.find('}') else {
            return Err(PackLoadError::Invalid(format!(
                "{ctx}: unclosed placeholder in template"
            )));
        };
        let name = &after[..j];
        if !ALLOWED_PLACEHOLDERS.contains(&name) {
            return Err(PackLoadError::Invalid(format!(
                "{ctx}: unknown placeholder {{{name}}}"
            )));
        }
        rest = &after[j + 1..];
    }
    Ok(())
}

impl CustomPack {
    pub fn builtin_en_us() -> Self {
        let mut messages = BTreeMap::new();
        for (k, t) in [
            ("parse.expected_expression", "expected expression"),
            ("runtime.zero_division", "division by zero"),
        ] {
            messages.insert(
                k.into(),
                MessageSpec {
                    text: Some(t.into()),
                    suffix: None,
                    style: None,
                },
            );
        }
        Self {
            id: "en-US".into(),
            description: "Official English (built-in)".into(),
            format_version: 1,
            messages,
            layout: Layout::default(),
            layout_set: LayoutSet::default(),
            gloss: BTreeMap::new(),
        }
    }

    /// `overlay` 覆盖本包：messages/gloss 按键字段合并；layout 仅覆盖 overlay 显式字段。
    pub fn merged_with(&self, overlay: &CustomPack) -> CustomPack {
        let mut out = self.clone();
        for (k, v) in &overlay.messages {
            let entry = out.messages.entry(k.clone()).or_default();
            if v.text.is_some() {
                entry.text = v.text.clone();
            }
            if v.suffix.is_some() {
                entry.suffix = v.suffix.clone();
            }
            if v.style.is_some() {
                entry.style = v.style.clone();
            }
        }
        let s = &overlay.layout_set;
        if s.repl_prompt {
            out.layout.repl.prompt = overlay.layout.repl.prompt.clone();
            out.layout_set.repl_prompt = true;
        }
        if s.repl_continuation {
            out.layout.repl.continuation = overlay.layout.repl.continuation.clone();
            out.layout_set.repl_continuation = true;
        }
        if s.parse_label {
            out.layout.parse.label_error = overlay.layout.parse.label_error.clone();
            out.layout_set.parse_label = true;
        }
        if s.parse_arrow {
            out.layout.parse.arrow = overlay.layout.parse.arrow.clone();
            out.layout_set.parse_arrow = true;
        }
        if s.tb_header {
            out.layout.traceback.header = overlay.layout.traceback.header.clone();
            out.layout_set.tb_header = true;
        }
        if s.tb_frame {
            out.layout.traceback.frame = overlay.layout.traceback.frame.clone();
            out.layout_set.tb_frame = true;
        }
        if s.tb_direction {
            out.layout.traceback.direction = overlay.layout.traceback.direction.clone();
            out.layout_set.tb_direction = true;
        }
        if s.exc_line {
            out.layout.exception.line = overlay.layout.exception.line.clone();
            out.layout_set.exc_line = true;
        }
        for (k, v) in &overlay.gloss {
            out.gloss.insert(k.clone(), v.clone());
        }
        out
    }

    pub fn render_message(&self, key: &str, fallback: &str) -> String {
        if let Some(spec) = self.messages.get(key) {
            let mut s = spec.text.as_deref().unwrap_or(fallback).to_string();
            if let Some(suf) = &spec.suffix {
                s.push_str(suf);
            }
            return s;
        }
        fallback.to_string()
    }
}

pub fn load_pack_dir(dir: &Path) -> Result<CustomPack, PackLoadError> {
    let path = dir.join(PACK_MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|e| {
        PackLoadError::Io(format!("cannot read {}: {e}", path.display()))
    })?;
    let file: PackFile = toml::from_str(&text)
        .map_err(|e| PackLoadError::Parse(format!("invalid {}: {e}", path.display())))?;
    if file.format_version != 1 {
        return Err(PackLoadError::Invalid(format!(
            "unsupported format_version {}",
            file.format_version
        )));
    }
    if file.id.is_empty() {
        return Err(PackLoadError::Invalid("id must be non-empty".into()));
    }
    let dir_name = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    if dir_name != file.id {
        return Err(PackLoadError::Invalid(format!(
            "directory name `{dir_name}` must match id `{}`",
            file.id
        )));
    }

    let mut layout_set = LayoutSet::default();
    let mut layout = Layout::default();
    if let Some(v) = file.layout.repl.prompt {
        layout.repl.prompt = v;
        layout_set.repl_prompt = true;
    }
    if let Some(v) = file.layout.repl.continuation {
        layout.repl.continuation = v;
        layout_set.repl_continuation = true;
    }
    if let Some(v) = file.layout.parse.label_error {
        layout.parse.label_error = v;
        layout_set.parse_label = true;
    }
    if let Some(v) = file.layout.parse.arrow {
        layout.parse.arrow = v;
        layout_set.parse_arrow = true;
    }
    if let Some(v) = file.layout.traceback.header {
        layout.traceback.header = v;
        layout_set.tb_header = true;
    }
    if let Some(v) = file.layout.traceback.frame {
        layout.traceback.frame = v;
        layout_set.tb_frame = true;
    }
    if let Some(v) = file.layout.traceback.direction {
        if v != "top_down" && v != "bottom_up" {
            return Err(PackLoadError::Invalid(format!(
                "layout.traceback.direction must be top_down or bottom_up, got {v}"
            )));
        }
        layout.traceback.direction = v;
        layout_set.tb_direction = true;
    }
    if let Some(v) = file.layout.exception.line {
        layout.exception.line = v;
        layout_set.exc_line = true;
    }

    validate_template(&layout.traceback.frame, "layout.traceback.frame")?;
    validate_template(&layout.exception.line, "layout.exception.line")?;

    Ok(CustomPack {
        id: file.id,
        description: file.description,
        format_version: file.format_version,
        messages: file.messages,
        layout,
        layout_set,
        gloss: file.gloss,
    })
}

pub fn list_installed_ids(custom_root: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    let Ok(rd) = std::fs::read_dir(custom_root) else {
        return ids;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.is_dir() && path.join(PACK_MANIFEST_FILE).is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids
}
