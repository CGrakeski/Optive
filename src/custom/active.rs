//! 进程内激活的定制包。

use std::sync::{Arc, RwLock};

use super::keys::Diag;
use super::pack::CustomPack;
use super::{build_active_from_ids, resolve_use_chain};

static ACTIVE: RwLock<Option<Arc<ActivePack>>> = RwLock::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    TopDown,
    BottomUp,
}

#[derive(Debug, Clone)]
pub struct ActivePack {
    pub pack: CustomPack,
    /// 展示用：`en-US → catgirl → …`
    pub chain: Vec<String>,
}

impl ActivePack {
    pub fn render_diag(&self, diag: &Diag) -> String {
        let key = diag.key();
        let fallback = diag.default_en();
        if matches!(diag, Diag::Legacy(_)) {
            return fallback.into_owned();
        }
        self.pack.render_message(key, fallback.as_ref())
    }

    pub fn repl_prompt(&self) -> &str {
        &self.pack.layout.repl.prompt
    }

    pub fn repl_continuation(&self) -> &str {
        &self.pack.layout.repl.continuation
    }

    pub fn parse_label_error(&self) -> &str {
        &self.pack.layout.parse.label_error
    }

    pub fn parse_arrow(&self) -> &str {
        &self.pack.layout.parse.arrow
    }

    pub fn traceback_header(&self) -> &str {
        &self.pack.layout.traceback.header
    }

    pub fn traceback_direction(&self) -> TraceDirection {
        if self.pack.layout.traceback.direction == "bottom_up" {
            TraceDirection::BottomUp
        } else {
            TraceDirection::TopDown
        }
    }

    pub fn format_traceback_frame(&self, file: &str, line: usize, func: &str) -> String {
        self.pack
            .layout
            .traceback
            .frame
            .replace("{file}", file)
            .replace("{line}", &line.to_string())
            .replace("{func}", func)
    }

    pub fn format_exception_line(&self, name: &str, msg: &str) -> String {
        let gloss = self.pack.gloss.get(name).cloned().unwrap_or_default();
        if msg.is_empty()
            && gloss.is_empty()
            && self.pack.layout.exception.line == "{name}: {msg}"
        {
            return name.to_string();
        }
        self.pack
            .layout
            .exception
            .line
            .replace("{name}", name)
            .replace("{gloss}", &gloss)
            .replace("{msg}", msg)
    }

    pub fn chain_display(&self) -> String {
        self.chain.join(" → ")
    }
}

fn default_active() -> Arc<ActivePack> {
    Arc::new(ActivePack {
        pack: CustomPack::builtin_en_us(),
        chain: vec!["en-US".into()],
    })
}

/// 返回当前激活包（未 init 时用内嵌 en-US）。
pub fn active_pack() -> Arc<ActivePack> {
    ACTIVE
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(default_active)
}

pub fn set_active_pack(pack: ActivePack) {
    if let Ok(mut g) = ACTIVE.write() {
        *g = Some(Arc::new(pack));
    }
}

/// 启动时调用：按 CLI / 环境 / 项目 / 全局解析并装载。
pub fn init_from_env_and_cwd(cli_override: Option<&str>) -> Result<(), String> {
    let ids = resolve_use_chain(cli_override);
    let active = build_active_from_ids(&ids)?;
    set_active_pack(active);
    Ok(())
}
