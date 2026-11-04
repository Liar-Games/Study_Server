use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    #[error("invalid packet")]
    InvalidPacket,
    #[error("authentication failed")]
    AuthFailed,
    #[error("service unavailable")]
    Unavailable,
    #[error("already connected")]
    AlreadyConnected,
    #[error("not a room member")]
    NotMember,
    #[error("room not found")]
    RoomNotFound,
    #[error("record not found")]
    NotFound,
    #[error("room full")]
    RoomFull,
    #[error("admission denied")]
    Denied,
    #[error("capacity exceeded")]
    Limit,
    #[error("mailbox full")]
    Busy,
    #[error("connection closed")]
    Closed,
    #[error("already queued")]
    AlreadyQueued,
}
