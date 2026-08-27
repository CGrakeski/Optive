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
    /// 二元/一元运算不支持；对外类型名为 `TypeError`，但可与其它 `TypeError` 区分。
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
    /// 任务被协作式取消（`Task.cancel` / `race` 失败者 / `taskgroup` 退出）。
    Cancelled,
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
        Self::Cancelled,
    ];

    #[must_use]
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
            Self::Cancelled => "Cancelled",
        }
    }

    #[must_use]
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
            | Self::NotImplemented
            | Self::Cancelled => Some(Self::Exception),
            Self::UnsupportedOp => Some(Self::TypeError),
            Self::ZeroDivision => Some(Self::ArithmeticError),
            Self::KeyError | Self::IndexError => Some(Self::LookupError),
            Self::RecursionError | Self::DeadlockError => Some(Self::Runtime),
        }
    }

    #[must_use]
    pub fn from_type_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.type_name() == name)
    }
}

#[derive(Debug, Error, Clone)]
pub enum LexError {
    #[error("lex error at {line}:{column}: {message}")]
    Message {
        line: usize,
        column: usize,
        message: String,
    },
}

#[derive(Debug, Error, Clone)]
pub enum ParseError {
    #[error("parse error at {line}:{column}: {message}")]
    Message {
        line: usize,
        column: usize,
        message: String,
    },
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

    /// `message` 是异常**正文**，不要带 `TypeError:` 这类类型名前缀。
    /// 人读末行由 [`Self::uncaught_line`] / traceback 用 `kind` 拼一次。
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

    pub fn recursion_err(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::RecursionError, message)
    }

    /// 按当前定制包渲染的除零错误（人读文案；类型名仍为 `ZeroDivisionError`）。
    #[must_use]
    pub fn zero_div_diag() -> Self {
        Self::zero_div(crate::custom::render(&crate::custom::Diag::Runtime(
            crate::custom::ErrorKindMsg::ZeroDivision,
        )))
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

    pub fn cancelled(message: impl Into<String>) -> Self {
        Self::typed(ExceptionKind::Cancelled, message)
    }

    #[must_use]
    pub const fn kind(&self) -> ExceptionKind {
        match self {
            Self::Host { kind, .. } | Self::AtLine { kind, .. } => *kind,
        }
    }

    #[must_use]
    pub const fn message(&self) -> &str {
        match self {
            Self::Host { message, .. } | Self::AtLine { message, .. } => message.as_str(),
        }
    }

    /// 未捕获时的人读末行：`TypeName: 正文`。`message` 本身不含类型名。
    #[must_use]
    pub fn uncaught_line(&self) -> String {
        crate::custom::active_pack().format_exception_line(self.kind().type_name(), self.message())
    }

    #[must_use]
    pub fn with_line(self, line: usize) -> Self {
        Self::AtLine {
            kind: self.kind(),
            message: self.message().to_string(),
            line,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_message_is_body_uncaught_line_adds_kind() {
        let err = RuntimeError::type_err("invalid num literal");
        assert_eq!(err.message(), "invalid num literal");
        assert_eq!(err.uncaught_line(), "TypeError: invalid num literal");
        let rec = RuntimeError::recursion_err("maximum recursion depth exceeded");
        assert_eq!(rec.kind(), ExceptionKind::RecursionError);
        assert_eq!(rec.message(), "maximum recursion depth exceeded");
        assert_eq!(
            rec.uncaught_line(),
            "RecursionError: maximum recursion depth exceeded"
        );
    }
}
