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
    pub fn key(&self) -> &'static str {
        match self {
            Diag::Parse(ParseMsg::ExpectedExpression) => "parse.expected_expression",
            Diag::Runtime(ErrorKindMsg::ZeroDivision) => "runtime.zero_division",
            Diag::Cli(m) => m.key(),
            Diag::Repl(m) => m.key(),
            Diag::Legacy(_) => "legacy",
        }
    }

    pub fn default_en(&self) -> Cow<'static, str> {
        match self {
            Diag::Parse(ParseMsg::ExpectedExpression) => "expected expression".into(),
            Diag::Runtime(ErrorKindMsg::ZeroDivision) => "division by zero".into(),
            Diag::Cli(m) => m.default_en().into(),
            Diag::Repl(m) => m.default_en().into(),
            Diag::Legacy(s) => s.clone(),
        }
    }
}

impl CliMsg {
    pub fn key(self) -> &'static str {
        match self {
            CliMsg::HelpTitle => "cli.help.title",
            CliMsg::HelpUsageHeader => "cli.help.usage_header",
            CliMsg::HelpRepl => "cli.help.repl",
            CliMsg::HelpRunScript => "cli.help.run_script",
            CliMsg::HelpRunCode => "cli.help.run_code",
            CliMsg::HelpNew => "cli.help.new",
            CliMsg::HelpRun => "cli.help.run",
            CliMsg::HelpUp => "cli.help.up",
            CliMsg::HelpAdd => "cli.help.add",
            CliMsg::HelpRemove => "cli.help.remove",
            CliMsg::HelpUpdate => "cli.help.update",
            CliMsg::HelpCache => "cli.help.cache",
            CliMsg::HelpDeps => "cli.help.deps",
            CliMsg::HelpDepsDoctor => "cli.help.deps_doctor",
            CliMsg::HelpEnv => "cli.help.env",
            CliMsg::HelpChange => "cli.help.change",
            CliMsg::HelpFmt => "cli.help.fmt",
            CliMsg::HelpDebug => "cli.help.debug",
            CliMsg::HelpCustom => "cli.help.custom",
            CliMsg::HelpCapsHeader => "cli.help.caps_header",
            CliMsg::HelpSandbox => "cli.help.sandbox",
            CliMsg::HelpNoNetwork => "cli.help.no_network",
            CliMsg::HelpNoFfi => "cli.help.no_ffi",
            CliMsg::HelpAllowFfi => "cli.help.allow_ffi",
            CliMsg::HelpAllowPath => "cli.help.allow_path",
            CliMsg::HelpH => "cli.help.h",
            CliMsg::HelpV => "cli.help.v",
            CliMsg::HelpEnvHeader => "cli.help.env_header",
            CliMsg::HelpOptiveHome => "cli.help.optive_home",
            CliMsg::HelpLocalDeps => "cli.help.local_deps",
            CliMsg::HelpFiles => "cli.help.files",
            CliMsg::HelpOptiveCustomEnv => "cli.help.optive_custom_env",
            CliMsg::CustomChanging => "cli.custom.changing",
            CliMsg::CustomDone => "cli.custom.done",
            CliMsg::CustomAdded => "cli.custom.added",
            CliMsg::CustomNow => "cli.custom.now",
        }
    }

    pub fn default_en(self) -> &'static str {
        match self {
            CliMsg::HelpTitle => "Optive",
            CliMsg::HelpUsageHeader => "Usage:",
            CliMsg::HelpRepl => "  Optive                         Start interactive REPL",
            CliMsg::HelpRunScript => "  Optive <script.tive>           Run a script",
            CliMsg::HelpRunCode => "  Optive -c <code>               Run code from argument (multi-line OK)",
            CliMsg::HelpNew => "  Optive new <ProjectName>       Create a new project",
            CliMsg::HelpRun => "  Optive run [path] [-- args…]   Ensure deps (strict lock) + run entry",
            CliMsg::HelpUp => "  Optive up [path] [-- args…]    update + run",
            CliMsg::HelpAdd => "  Optive add <git-url> […]       Add dependency (default: pin tip commit)",
            CliMsg::HelpRemove => "  Optive remove <name>           Remove dependency",
            CliMsg::HelpUpdate => "  Optive update [name] [--dry-run] [-v]",
            CliMsg::HelpCache => "  Optive cache gc [--dry-run]    Remove orphan packs",
            CliMsg::HelpDeps => "  Optive deps [-v]               List project dependencies",
            CliMsg::HelpDepsDoctor => "  Optive deps doctor [-v]        Diagnose deps / lock / orphans",
            CliMsg::HelpEnv => "  Optive env                     Print OPTIVE_HOME and paths",
            CliMsg::HelpChange => "  Optive change track_latest=…   Toggle tip-following (warns)",
            CliMsg::HelpFmt => "  Optive fmt <file> [-o|--out]   Format a .tive file (default: write back)",
            CliMsg::HelpDebug => "  Optive debug [file|path]       Debug a script or project entry",
            CliMsg::HelpCustom => "  Optive custom …                Manage customization packs",
            CliMsg::HelpCapsHeader => "Runtime capability flags (apply to run / up / debug / <script> / -c):",
            CliMsg::HelpSandbox => "  --sandbox[=DIR]          No network, no env, no FFI; fs limited to DIR (default: cwd)",
            CliMsg::HelpNoNetwork => "  --no-network            Disable std.http",
            CliMsg::HelpNoFfi => "  --no-ffi                Disable C.frompath / extern",
            CliMsg::HelpAllowFfi => "  --allow-ffi             Allow native FFI (overrides sandbox default)",
            CliMsg::HelpAllowPath => "  --allow-path DIR         Allow fs access under DIR (repeatable; combines with --sandbox)",
            CliMsg::HelpH => "  Optive -h, --help              Show this help",
            CliMsg::HelpV => "  Optive -V, --version           Show version",
            CliMsg::HelpEnvHeader => "Env:",
            CliMsg::HelpOptiveHome => "  OPTIVE_HOME              Global pack/ + index.db root",
            CliMsg::HelpLocalDeps => "  OPTIVE_USE_LOCAL_DEPS=1  Debug: install into project deps/",
            CliMsg::HelpOptiveCustomEnv => "  OPTIVE_CUSTOM=a,b        Override active customization packs",
            CliMsg::HelpFiles => "Files: Optive.toml (intent), Optive.lock (repro), Optive.cache (local), Custom.toml (packs)",
            CliMsg::CustomChanging => "Changing...",
            CliMsg::CustomDone => "Done.",
            CliMsg::CustomAdded => "Added Custom Pack:",
            CliMsg::CustomNow => "Now Custom:",
        }
    }
}

impl ReplMsg {
    pub fn key(self) -> &'static str {
        match self {
            ReplMsg::HelpTitle => "repl.help.title",
            ReplMsg::HelpHelp => "repl.help.help",
            ReplMsg::HelpQuit => "repl.help.quit",
            ReplMsg::HelpCtrlC => "repl.help.ctrl_c",
            ReplMsg::HelpCtrlD => "repl.help.ctrl_d",
        }
    }

    pub fn default_en(self) -> &'static str {
        match self {
            ReplMsg::HelpTitle => "Optive REPL",
            ReplMsg::HelpHelp => "  :help              Show this help",
            ReplMsg::HelpQuit => "  :quit / :exit      Exit (also quit / exit)",
            ReplMsg::HelpCtrlC => "  Ctrl-C             Cancel unfinished multi-line input",
            ReplMsg::HelpCtrlD => "  Ctrl-D             Exit",
        }
    }
}
