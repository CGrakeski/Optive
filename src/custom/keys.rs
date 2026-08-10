//! 强类型诊断 / CLI / REPL 消息 ID。

use std::borrow::Cow;

/// 统一人读消息入口。
#[derive(Debug, Clone)]
pub enum Diag {
    Parse(ParseMsg),
    Runtime(ErrorKindMsg),
    Cli(CliMsg),
    Repl(ReplMsg),
    /// 尚未迁移的英文原文。
    Legacy(Cow<'static, str>),
}

#[derive(Debug, Clone, Copy)]
pub enum ParseMsg {
    ExpectedExpression,
}

#[derive(Debug, Clone, Copy)]
pub enum ErrorKindMsg {
    ZeroDivision,
}

#[derive(Debug, Clone, Copy)]
pub enum CliMsg {
    HelpTitle,
    HelpUsageHeader,
    HelpRepl,
    HelpRunScript,
    HelpRunCode,
    HelpNew,
    HelpRun,
    HelpUp,
    HelpAdd,
    HelpRemove,
    HelpUpdate,
    HelpCache,
    HelpDeps,
    HelpDepsDoctor,
    HelpEnv,
    HelpChange,
    HelpFmt,
    HelpDebug,
    HelpCustom,
    HelpCapsHeader,
    HelpSandbox,
    HelpNoNetwork,
    HelpNoFfi,
    HelpAllowFfi,
    HelpAllowPath,
    HelpH,
    HelpV,
    HelpEnvHeader,
    HelpOptiveHome,
    HelpLocalDeps,
    HelpFiles,
    HelpOptiveCustomEnv,
    CustomChanging,
    CustomDone,
    CustomAdded,
    CustomNow,
}

#[derive(Debug, Clone, Copy)]
pub enum ReplMsg {
    HelpTitle,
    HelpHelp,
    HelpQuit,
    HelpCtrlC,
    HelpCtrlD,
}

impl Diag {
    #[must_use]
    pub const fn key(&self) -> &'static str {
        match self {
            Self::Parse(ParseMsg::ExpectedExpression) => "parse.expected_expression",
            Self::Runtime(ErrorKindMsg::ZeroDivision) => "runtime.zero_division",
            Self::Cli(m) => m.key(),
            Self::Repl(m) => m.key(),
            Self::Legacy(_) => "legacy",
        }
    }

    #[must_use]
    pub fn default_en(&self) -> Cow<'static, str> {
        match self {
            Self::Parse(ParseMsg::ExpectedExpression) => "expected expression".into(),
            Self::Runtime(ErrorKindMsg::ZeroDivision) => "division by zero".into(),
            Self::Cli(m) => m.default_en().into(),
            Self::Repl(m) => m.default_en().into(),
            Self::Legacy(s) => s.clone(),
        }
    }
}

impl CliMsg {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::HelpTitle => "cli.help.title",
            Self::HelpUsageHeader => "cli.help.usage_header",
            Self::HelpRepl => "cli.help.repl",
            Self::HelpRunScript => "cli.help.run_script",
            Self::HelpRunCode => "cli.help.run_code",
            Self::HelpNew => "cli.help.new",
            Self::HelpRun => "cli.help.run",
            Self::HelpUp => "cli.help.up",
            Self::HelpAdd => "cli.help.add",
            Self::HelpRemove => "cli.help.remove",
            Self::HelpUpdate => "cli.help.update",
            Self::HelpCache => "cli.help.cache",
            Self::HelpDeps => "cli.help.deps",
            Self::HelpDepsDoctor => "cli.help.deps_doctor",
            Self::HelpEnv => "cli.help.env",
            Self::HelpChange => "cli.help.change",
            Self::HelpFmt => "cli.help.fmt",
            Self::HelpDebug => "cli.help.debug",
            Self::HelpCustom => "cli.help.custom",
            Self::HelpCapsHeader => "cli.help.caps_header",
            Self::HelpSandbox => "cli.help.sandbox",
            Self::HelpNoNetwork => "cli.help.no_network",
            Self::HelpNoFfi => "cli.help.no_ffi",
            Self::HelpAllowFfi => "cli.help.allow_ffi",
            Self::HelpAllowPath => "cli.help.allow_path",
            Self::HelpH => "cli.help.h",
            Self::HelpV => "cli.help.v",
            Self::HelpEnvHeader => "cli.help.env_header",
            Self::HelpOptiveHome => "cli.help.optive_home",
            Self::HelpLocalDeps => "cli.help.local_deps",
            Self::HelpFiles => "cli.help.files",
            Self::HelpOptiveCustomEnv => "cli.help.optive_custom_env",
            Self::CustomChanging => "cli.custom.changing",
            Self::CustomDone => "cli.custom.done",
            Self::CustomAdded => "cli.custom.added",
            Self::CustomNow => "cli.custom.now",
        }
    }

    #[must_use]
    pub const fn default_en(self) -> &'static str {
        match self {
            Self::HelpTitle => "Optive",
            Self::HelpUsageHeader => "Usage:",
            Self::HelpRepl => "  Optive                         Start interactive REPL",
            Self::HelpRunScript => "  Optive <script.tive>           Run a script",
            Self::HelpRunCode => "  Optive -c <code>               Run code from argument (multi-line OK)",
            Self::HelpNew => "  Optive new <ProjectName>       Create a new project",
            Self::HelpRun => "  Optive run [path] [-- args…]   Ensure deps (strict lock) + run entry",
            Self::HelpUp => "  Optive up [path] [-- args…]    update + run",
            Self::HelpAdd => "  Optive add <git-url> […]       Add dependency (default: pin tip commit)",
            Self::HelpRemove => "  Optive remove <name>           Remove dependency",
            Self::HelpUpdate => "  Optive update [name] [--dry-run] [-v]",
            Self::HelpCache => "  Optive cache gc [--dry-run]    Remove orphan packs",
            Self::HelpDeps => "  Optive deps [-v]               List project dependencies",
            Self::HelpDepsDoctor => "  Optive deps doctor [-v]        Diagnose deps / lock / orphans",
            Self::HelpEnv => "  Optive env                     Print OPTIVE_HOME and paths",
            Self::HelpChange => "  Optive change track_latest=…   Toggle tip-following (warns)",
            Self::HelpFmt => "  Optive fmt <file> [-o|--out]   Format a .tive file (default: write back)",
            Self::HelpDebug => "  Optive debug [file|path]       Debug a script or project entry",
            Self::HelpCustom => "  Optive custom …                Manage customization packs",
            Self::HelpCapsHeader => "Runtime capability flags (apply to run / up / debug / <script> / -c):",
            Self::HelpSandbox => "  --sandbox[=DIR]          No network, no env, no FFI; fs limited to DIR (default: cwd)",
            Self::HelpNoNetwork => "  --no-network            Disable std.http",
            Self::HelpNoFfi => "  --no-ffi                Disable C.frompath / extern",
            Self::HelpAllowFfi => "  --allow-ffi             Allow native FFI (overrides sandbox default)",
            Self::HelpAllowPath => "  --allow-path DIR         Allow fs access under DIR (repeatable; combines with --sandbox)",
            Self::HelpH => "  Optive -h, --help              Show this help",
            Self::HelpV => "  Optive -V, --version           Show version",
            Self::HelpEnvHeader => "Env:",
            Self::HelpOptiveHome => "  OPTIVE_HOME              Global pack/ + index.db root",
            Self::HelpLocalDeps => "  OPTIVE_USE_LOCAL_DEPS=1  Debug: install into project deps/",
            Self::HelpOptiveCustomEnv => "  OPTIVE_CUSTOM=a,b        Override active customization packs",
            Self::HelpFiles => "Files: Optive.toml (intent), Optive.lock (repro), Optive.cache (local), Custom.toml (packs)",
            Self::CustomChanging => "Changing...",
            Self::CustomDone => "Done.",
            Self::CustomAdded => "Added Custom Pack:",
            Self::CustomNow => "Now Custom:",
        }
    }
}

impl ReplMsg {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::HelpTitle => "repl.help.title",
            Self::HelpHelp => "repl.help.help",
            Self::HelpQuit => "repl.help.quit",
            Self::HelpCtrlC => "repl.help.ctrl_c",
            Self::HelpCtrlD => "repl.help.ctrl_d",
        }
    }

    #[must_use]
    pub const fn default_en(self) -> &'static str {
        match self {
            Self::HelpTitle => "Optive REPL",
            Self::HelpHelp => "  :help              Show this help",
            Self::HelpQuit => "  :quit / :exit      Exit (also quit / exit)",
            Self::HelpCtrlC => "  Ctrl-C             Cancel unfinished multi-line input",
            Self::HelpCtrlD => "  Ctrl-D             Exit",
        }
    }
}
