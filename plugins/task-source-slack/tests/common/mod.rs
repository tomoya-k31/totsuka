//! Shared recorded-transport fake for this plugin's test crates: canned Web
//! API responses in, recorded requests out — no network involved.

// Each test crate compiles its own copy of this module and uses a different
// subset of it; unused helpers in one crate are not dead code.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use task_source_slack::error::SlackError;
use task_source_slack::server::TransportFactory;
use task_source_slack::transport::{SlackTransport, TokenKind, TransportSettings};

/// A canned Web API outcome for one `call`.
#[derive(Clone)]
pub enum Canned {
    /// A full response body.
    Data(Value),
    /// Simulate a network failure.
    Network,
}

/// One recorded request: its token kind, method, body, and retry class.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub token: TokenKind,
    pub method: String,
    pub body: Option<Value>,
    pub idempotent: bool,
}

/// One recorded `response_url` POST.
#[derive(Clone, Debug)]
pub struct PostedUrl {
    pub url: String,
    pub body: Value,
}

/// State shared between the factory, the transports it builds, and the test.
#[derive(Clone, Default)]
pub struct Shared {
    responses: Arc<Mutex<VecDeque<Canned>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    posted_urls: Arc<Mutex<Vec<PostedUrl>>>,
}

impl Shared {
    pub fn push(&self, canned: Canned) {
        self.responses.lock().unwrap().push_back(canned);
    }
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
    pub fn posted_urls(&self) -> Vec<PostedUrl> {
        self.posted_urls.lock().unwrap().clone()
    }
}

/// A [`SlackTransport`] answering from the shared canned queue.
pub struct FakeTransport {
    pub shared: Shared,
}

impl SlackTransport for FakeTransport {
    fn call(
        &self,
        token: TokenKind,
        method: &str,
        body: Option<Value>,
        idempotent: bool,
    ) -> impl Future<Output = Result<Value, SlackError>> + Send {
        self.shared.requests.lock().unwrap().push(Recorded {
            token,
            method: method.to_string(),
            body,
            idempotent,
        });
        let next = self.shared.responses.lock().unwrap().pop_front();
        async move {
            match next {
                Some(Canned::Data(v)) => Ok(v),
                Some(Canned::Network) => Err(SlackError::Transport("connection refused".into())),
                None => Err(SlackError::InvalidResponse("no canned response".into())),
            }
        }
    }

    fn post_url(
        &self,
        url: &str,
        body: Value,
    ) -> impl Future<Output = Result<(), SlackError>> + Send {
        self.shared.posted_urls.lock().unwrap().push(PostedUrl {
            url: url.to_string(),
            body,
        });
        async { Ok(()) }
    }
}

/// A [`TransportFactory`] producing [`FakeTransport`]s over the same state.
pub struct FakeFactory {
    pub shared: Shared,
}

impl TransportFactory for FakeFactory {
    type Transport = FakeTransport;
    fn build(&self, _settings: TransportSettings<'_>) -> FakeTransport {
        FakeTransport {
            shared: self.shared.clone(),
        }
    }
}

/// A transport over (a clone of) `shared`.
pub fn transport(shared: &Shared) -> FakeTransport {
    FakeTransport {
        shared: shared.clone(),
    }
}
