//! Shared recorded-transport fake for this plugin's test crates: canned Web
//! API responses in, recorded requests out — no network involved.

// Each test crate compiles its own copy of this module and uses a different
// subset of it; unused helpers in one crate are not dead code.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use task_source_slack::config::LlmConfig;
use task_source_slack::error::SlackError;
use task_source_slack::llm::ChatTransport;
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
    /// Method-keyed responses, consulted before the global queue. For tests
    /// where concurrent background tasks make a single ordered queue racy.
    keyed: Arc<Mutex<std::collections::HashMap<String, VecDeque<Canned>>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    posted_urls: Arc<Mutex<Vec<PostedUrl>>>,
    chat_responses: Arc<Mutex<VecDeque<Result<Value, String>>>>,
    chat_requests: Arc<Mutex<Vec<Value>>>,
}

impl Shared {
    pub fn push(&self, canned: Canned) {
        self.responses.lock().unwrap().push_back(canned);
    }
    /// Queue a response for one specific Web API `method`. The last entry is
    /// sticky: it keeps answering repeats of the method.
    pub fn push_for(&self, method: &str, canned: Canned) {
        self.keyed
            .lock()
            .unwrap()
            .entry(method.to_string())
            .or_default()
            .push_back(canned);
    }
    fn next_response(&self, method: &str) -> Option<Canned> {
        if let Some(queue) = self.keyed.lock().unwrap().get_mut(method) {
            return match queue.len() {
                0 => None,
                1 => queue.front().cloned(), // sticky last answer
                _ => queue.pop_front(),
            };
        }
        self.responses.lock().unwrap().pop_front()
    }
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
    pub fn posted_urls(&self) -> Vec<PostedUrl> {
        self.posted_urls.lock().unwrap().clone()
    }
    /// Queue one chat-completion outcome for the repo classifier.
    pub fn push_chat(&self, outcome: Result<Value, String>) {
        self.chat_responses.lock().unwrap().push_back(outcome);
    }
    pub fn chat_requests(&self) -> Vec<Value> {
        self.chat_requests.lock().unwrap().clone()
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
        let next = self.shared.next_response(method);
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
    type Chat = FakeChat;
    fn build(&self, _settings: TransportSettings<'_>) -> FakeTransport {
        FakeTransport {
            shared: self.shared.clone(),
        }
    }
    fn build_chat(&self) -> FakeChat {
        FakeChat {
            shared: self.shared.clone(),
        }
    }
}

/// A [`ChatTransport`] answering from the shared canned queue.
pub struct FakeChat {
    pub shared: Shared,
}

impl ChatTransport for FakeChat {
    fn complete(
        &self,
        _config: &LlmConfig,
        body: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        self.shared.chat_requests.lock().unwrap().push(body);
        let next = self.shared.chat_responses.lock().unwrap().pop_front();
        async move { next.unwrap_or_else(|| Err("no canned chat response".into())) }
    }
}

/// A transport over (a clone of) `shared`.
pub fn transport(shared: &Shared) -> FakeTransport {
    FakeTransport {
        shared: shared.clone(),
    }
}
