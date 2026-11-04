use crate::{
    Error, Result,
    protocol::AuthRequest,
    types::{Identity, Principal},
};
use futures::future::BoxFuture;

pub trait Authenticator: Send + Sync + 'static {
    fn authenticate(&self, request: AuthRequest) -> BoxFuture<'_, Result<Identity>>;
}
pub struct GuestAuth;
impl Authenticator for GuestAuth {
    fn authenticate(&self, request: AuthRequest) -> BoxFuture<'_, Result<Identity>> {
        Box::pin(async move {
            match request {
                AuthRequest::Guest { version: 1 } => Ok(Identity {
                    principal: Principal::guest(),
                    name: "Guest".into(),
                    avatar_url: None,
                }),
                _ => Err(Error::AuthFailed),
            }
        })
    }
}
