use thiserror::Error;

/// 语言异常种类。宿主错误必须携带此类型，禁止靠解析文案猜测。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExceptionKind {
    BaseException,
    Exception,
    StopIteration,
    ArithmeticError,
    LookupError,
    Runtime,
    ValueError,
    TypeError,
    /// 二元/一元运算不支持；对外类型名为 `TypeError`，但可与其它 TypeError 区分。
    UnsupportedOp,
    SyntaxError,
    AttributeError,
    ZeroDivision,
    KeyError,
    IndexError,
    NameError,
    IOError,
    AssertionError,
    NotImplemented,
    RecursionError,
    /// 协作/并行调度下无可运行任务仍阻塞（channel / mutex / await 等）。
    DeadlockError,
}

impl ExceptionKind {
    pub const ALL: &'static [Self] = &[
        Self::BaseException,
        Self::Exception,
        Self::StopIteration,
        Self::ArithmeticError,
        Self::LookupError,
        Self::Runtime,
        Self::ValueError,
        Self::TypeError,
        Self::SyntaxError,
        Self::AttributeError,
        Self::ZeroDivision,
        Self::KeyError,
        Self::IndexError,
        Self::NameError,
        Self::IOError,
        Self::AssertionError,
        Self::NotImplemented,
        Self::RecursionError,
        Self::DeadlockError,
    ];

    pub const fn type_name(self) -> &'static str {
        match self {
            Self::BaseException => "BaseException",
            Self::Exception => "Exception",
            Self::StopIteration => "StopIteration",
            Self::ArithmeticError => "ArithmeticError",
            Self::LookupError => "LookupError",
            Self::Runtime => "RuntimeError",
            Self::ValueError => "ValueError",
            Self::TypeError | Self::UnsupportedOp => "TypeError",
            Self::SyntaxError => "SyntaxError",
            Self::AttributeError => "AttributeError",
            Self::ZeroDivision => "ZeroDivisionError",
            Self::KeyError => "KeyError",
            Self::IndexError => "IndexError",
            Self::NameError => "NameError",
            Self::IOError => "IOError",
            Self::AssertionError => "AssertionError",
            Self::NotImplemented => "NotImplementedError",
            Self::RecursionError => "RecursionError",
            Self::DeadlockError => "DeadlockError",
        }
    }

    pub const fn parent(self) -> Option<Self> {
        match self {
            Self::BaseException => None,
            Self::Exception => Some(Self::BaseException),
            Self::StopIteration
            | Self::ArithmeticError
            | Self::LookupError
            | Self::Runtime
            | Self::ValueError
            | Self::TypeError
            | Self::SyntaxError
            | Self::AttributeError
            | Self::NameError
            | Self::IOError
            | Self::AssertionError
            | Self::NotImplemented => Some(Self::Exception),
            Self::UnsupportedOp => Some(Self::TypeError),
            Self::ZeroDivision => Some(Self::ArithmeticError),
            Self::KeyError | Self::IndexError => Some(Self::LookupError),
            Self::RecursionError | Self::DeadlockError => Some(Self::Runtime),
        }
    }

    pub fn from_type_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|k| k.type_name() == name)
    }
}

#[derive(Debug, Error, Clone)]
pub enum LexError {
    #[error("lex error at {line}:{column}: {message}")]
    Message { line: usize, column: usize, message: String },
}

#[derive(Debug, Error, Clone)]
pub enum ParseError {
    #[error("parse error at {line}:{column}: {message}")]
    Message { line: usize, column: usize, message: String },
}

#[derive(Debug, Error, Clone)]
pub enum RuntimeError {
    #[error("{message}")]
    Host {
        kind: ExceptionKind,
        message: String,
    },
    #[error("{message} at line {line}")]
    AtLine {
        kind: ExceptionKind,
        message: String,
        line: usize,
    },
}

pub type Result<T> = std::result::Result<T, RuntimeError>;

impl ParseError {
    pub fn here(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self::Message {
            line,
            column,
            message: message.into(),
        }
    }
}

impl RuntimeError {
    /// 默认宿主运行时错误（种类为 `RuntimeError`）。
    pub fn msg(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::Runtime, message)
    }

    pub fn typed(kind: ExceptionKind, message: impl Into<String>) -> Self {
        Self::Host {
            kind,
            message: message.into(),
        }
    }

    pub fn type_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::TypeError, message)
    }

    pub fn value_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::ValueError, message)
    }

    pub fn index_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::IndexError, message)
    }

    pub fn key_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::KeyError, message)
    }

    pub fn name_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::NameError, message)
    }

    pub fn attr_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::AttributeError, message)
    }

    pub fn zero_div(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::ZeroDivision, message)
    }

    pub fn io_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::IOError, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::UnsupportedOp, message)
    }

    pub fn stop_iteration(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::StopIteration, message)
    }

    pub fn deadlock(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::DeadlockError, message)
    }

    pub fn kind(&self) -> ExceptionKind {
        match self {
            Self::Host { kind, .. } | Self::AtLine { kind, .. } => *kind,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Host { message, .. } | Self::AtLine { message, .. } => message.as_str(),
        }
    }

    pub fn with_line(self, line: usize) -> Self {
        Self::AtLine {
            kind: self.kind(),
            message: self.message().to_string(),
            line,
        }
    }
}
