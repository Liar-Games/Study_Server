use axum::extract::ws::Message;
use std::{borrow::Cow, fmt};

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(thiserror::Error, Debug)]
pub enum AppError {
    /* Error type for System, Infra */
    #[error("configuration error: {reason}")]
    Config { reason: Cow<'static, str> },

    #[error("failed to bind server socket at {addr}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    #[error("server runtime error")]
    Serve {
        #[source]
        source: std::io::Error,
    },

    #[error("redis error: {reason}")]
    Redis {
        reason: Cow<'static, str>,
        #[source]
        source: redis::RedisError,
    },

    #[error("SessionManager error: {reason}")]
    SessionManager { reason: Cow<'static, str> },

    /* Error type for Network, Protocol */
    #[error("websocket protocol error: {reason}")]
    WebSocket { reason: Cow<'static, str> },

    #[error("unknown socket type: {value}")]
    UnknownSocketType { value: u8 },

    #[error("utf-8 decode failed for field: {field}")]
    Utf8 {
        field: Cow<'static, str>,
        #[source]
        source: std::string::FromUtf8Error,
    },

    /* Error type for Logic, Auth */
    #[error("ticket is invalid or expired")]
    InvalidTicket,

    #[error("ticket lookup failed (key: {key})")]
    TicketLookupFailed {
        key: String,
        #[source]
        source: redis::RedisError,
    },

    #[error("session channel full/closed for user_id={user_id}")]
    SessionSendFailed {
        user_id: String,
        #[source]
        source: SessionSendError,
    },

    /* Error type for invalid ticket lengths */
    #[error("invalid frame: {reason}")]
    InvalidFrame { reason: Cow<'static, str> },

    /* Error type for business, session policy */
    #[error("duplicate login for user_id={user_id}")]
    DuplicateLogin { user_id: String },

    /* Error type for room manager error */
    #[error("room manager error: {reason}")]
    RoomManager { reason: Cow<'static, str> },
}

impl AppError {
    /* Return error message to client */
    // TODO : maybe make enum for client error codes?
    pub fn to_client_message(&self) -> (&'static str, String) {
        match self {
            /* Error that can be notified to client */
            AppError::InvalidTicket => ("AUTH_FAILED", "Invalid or expired ticket".into()),
            AppError::UnknownSocketType { value } => {
                ("BAD_REQUEST", format!("Unknown opcode: {}", value))
            }
            AppError::Utf8 { field, .. } => ("BAD_FORMAT", format!("Invalid UTF-8 in {}", field)),
            AppError::InvalidFrame { reason } => ("BAD_REQUEST", reason.to_string()),

            /* Business/session policy errors that are safe to expose */
            AppError::DuplicateLogin { .. } => {
                ("ALREADY_CONNECTED", "User is already connected".into())
            }

            /* Internal server error */
            _ => ("SERVER_ERROR", "Internal server error".into()),
        }
    }
}

/* Helper for mpsc error (Remove generics) */
#[derive(Debug)]
pub struct SessionSendError;

impl fmt::Display for SessionSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiver dropped or channel closed")
    }
}
impl std::error::Error for SessionSendError {}

// --- Converters ---
impl From<redis::RedisError> for AppError {
    fn from(source: redis::RedisError) -> Self {
        AppError::Redis {
            reason: "operation failed".into(),
            source,
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(source: std::io::Error) -> Self {
        AppError::Serve { source }
    }
}

impl From<tokio::sync::mpsc::error::SendError<Message>> for SessionSendError {
    fn from(_: tokio::sync::mpsc::error::SendError<Message>) -> Self {
        SessionSendError
    }
}
