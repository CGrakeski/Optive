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
    HelpSearch,
    HelpRemove,
    HelpUpdate,
    HelpPublish,
    HelpCache,
    HelpDeps,
    HelpDepsDoctor,
    HelpEnv,
    HelpChange,
    HelpFmt,
    HelpDebug,
    HelpTest,
    HelpCheck,
    HelpLsp,
    HelpDap,
    HelpIndex,
    HelpIndexChange,
    HelpCustom,
    HelpCapsHeader,
    HelpQuiet,
    HelpSandbox,
    HelpNoNetwork,
    HelpNoFfi,
    HelpAllowFfi,
    HelpAllowPath,
    HelpTrustDeps,
    HelpAllowDepNetwork,
    HelpAllowDepEnv,
    HelpAllowDepProcess,
    HelpAllowDepFfi,
    HelpH,
    HelpV,
    HelpEnvHeader,
    HelpOptiveHome,
    HelpLocalDeps,
    HelpFiles,
    HelpOptiveCustomEnv,
    HelpOptiveIndexUrl,
    HelpOptiveIndexPin,
    HelpOptiveIndexPolicy,
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
            Self::HelpSearch => "cli.help.search",
            Self::HelpRemove => "cli.help.remove",
            Self::HelpUpdate => "cli.help.update",
            Self::HelpPublish => "cli.help.publish",
            Self::HelpCache => "cli.help.cache",
            Self::HelpDeps => "cli.help.deps",
            Self::HelpDepsDoctor => "cli.help.deps_doctor",
            Self::HelpEnv => "cli.help.env",
            Self::HelpChange => "cli.help.change",
            Self::HelpFmt => "cli.help.fmt",
            Self::HelpDebug => "cli.help.debug",
            Self::HelpTest => "cli.help.test",
            Self::HelpCheck => "cli.help.check",
            Self::HelpLsp => "cli.help.lsp",
            Self::HelpDap => "cli.help.dap",
            Self::HelpIndex => "cli.help.index",
            Self::HelpIndexChange => "cli.help.index_change",
            Self::HelpCustom => "cli.help.custom",
            Self::HelpCapsHeader => "cli.help.caps_header",
            Self::HelpQuiet => "cli.help.quiet",
            Self::HelpSandbox => "cli.help.sandbox",
            Self::HelpNoNetwork => "cli.help.no_network",
            Self::HelpNoFfi => "cli.help.no_ffi",
            Self::HelpAllowFfi => "cli.help.allow_ffi",
            Self::HelpAllowPath => "cli.help.allow_path",
            Self::HelpTrustDeps => "cli.help.trust_deps",
            Self::HelpAllowDepNetwork => "cli.help.allow_dep_network",
            Self::HelpAllowDepEnv => "cli.help.allow_dep_env",
            Self::HelpAllowDepProcess => "cli.help.allow_dep_process",
            Self::HelpAllowDepFfi => "cli.help.allow_dep_ffi",
            Self::HelpH => "cli.help.h",
            Self::HelpV => "cli.help.v",
            Self::HelpEnvHeader => "cli.help.env_header",
            Self::HelpOptiveHome => "cli.help.optive_home",
            Self::HelpLocalDeps => "cli.help.local_deps",
            Self::HelpFiles => "cli.help.files",
            Self::HelpOptiveCustomEnv => "cli.help.optive_custom_env",
            Self::HelpOptiveIndexUrl => "cli.help.optive_index_url",
            Self::HelpOptiveIndexPin => "cli.help.optive_index_pin",
            Self::HelpOptiveIndexPolicy => "cli.help.optive_index_policy",
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
            Self::HelpRun => "  Optive run [path] [-- args...]   Ensure deps (strict lock) + run entry",
            Self::HelpUp => "  Optive up [path] [-- args...]    update + run",
            Self::HelpAdd => "  Optive add <url|pack[@ver]>    Add git or index pack dependency",
            Self::HelpSearch => "  Optive search [query]          Search pack names in the index",
            Self::HelpRemove => "  Optive remove <name>           Remove dependency",
            Self::HelpUpdate => "  Optive update [name] [--dry-run] [-v]",
            Self::HelpPublish => "  Optive publish <version>       Tag HEAD (vX.Y.Z) and optional push",
            Self::HelpCache => "  Optive cache gc [--dry-run]    Remove orphan packs",
            Self::HelpDeps => "  Optive deps [-v]               List project dependencies",
            Self::HelpDepsDoctor => "  Optive deps doctor [-v]        Diagnose deps / lock / orphans",
            Self::HelpEnv => "  Optive env                     Print OPTIVE_HOME and paths",
            Self::HelpChange => "  Optive change track_latest=...   Toggle tip-following (warns)",
            Self::HelpFmt => "  Optive fmt [path] [--check] [-o|--out]  Format a file or project",
            Self::HelpDebug => "  Optive debug [file|path]       Debug a script or project entry",
            Self::HelpTest => {
                "  Optive test [path] [--cover] [--filter P] [--jobs N] [--junit F] [--lcov F] [--cobertura F] [--cover-min N] [-- args...]"
            }
            Self::HelpCheck => {
                "  Optive check [path]             Parse + name/std/arity check (no VM)"
            }
            Self::HelpLsp => "  Optive lsp                     Language server (diagnostics, rename, tokens)",
            Self::HelpDap => "  Optive dap                     Debug adapter (stdio DAP; breakpoints / fibers)",
            Self::HelpIndex => "  Optive index sync              Fetch the package index (default: Gitee optindex)",
            Self::HelpIndexChange => "  Optive index change <url>      Set index git remote + sync",
            Self::HelpCustom => "  Optive custom ...                Manage customization packs",
            Self::HelpCapsHeader => "Runtime capability flags (apply to run / up / debug / test / <script> / -c):",
            Self::HelpQuiet => "  --quiet                  Silence Project / Running status lines (stderr)",
            Self::HelpSandbox => "  --sandbox[=DIR]          No network, no env, no FFI; fs limited to DIR (default: cwd)",
            Self::HelpNoNetwork => "  --no-network            Disable std.http / std.net",
            Self::HelpNoFfi => {
                "  --no-ffi                Disable C.frompath / extern (not with --allow-ffi)"
            }
            Self::HelpAllowFfi => {
                "  --allow-ffi             Permit FFI (not with --no-ffi; does not change filesystem sandboxing)"
            }
            Self::HelpAllowPath => "  --allow-path DIR         Allow fs access under DIR (repeatable; combines with --sandbox)",
            Self::HelpTrustDeps => "  --trust-deps             Let dependencies inherit host capabilities",
            Self::HelpAllowDepNetwork => "  --allow-dep-network      Allow network from dependency code",
            Self::HelpAllowDepEnv => "  --allow-dep-env          Allow env from dependency code",
            Self::HelpAllowDepProcess => {
                "  --allow-dep-process      Allow std.os.run/capture from dependency code"
            }
            Self::HelpAllowDepFfi => "  --allow-dep-ffi          Allow FFI from dependency code",
            Self::HelpH => "  Optive -h, --help              Show this help",
            Self::HelpV => "  Optive -V, --version           Show version",
            Self::HelpEnvHeader => "Env:",
            Self::HelpOptiveHome => "  OPTIVE_HOME              Global pack/ + index.db + index.url root",
            Self::HelpLocalDeps => "  OPTIVE_USE_LOCAL_DEPS=1  Debug: install into project deps/",
            Self::HelpOptiveCustomEnv => "  OPTIVE_CUSTOM=a,b        Override active customization packs",
            Self::HelpOptiveIndexUrl => "  OPTIVE_INDEX_URL         Override package index git remote (default: gitee.com/CGrakeski/optindex)",
            Self::HelpOptiveIndexPin => "  OPTIVE_INDEX_PIN         Require index HEAD to equal this full commit id",
            Self::HelpOptiveIndexPolicy => "  OPTIVE_INDEX_POLICY      Index trust: off (default), signed, or strict",
            Self::HelpFiles => "Files: Optive.toml, Optive.lock, Optive.cache, .optive/bc (bytecode), Custom.toml",
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
