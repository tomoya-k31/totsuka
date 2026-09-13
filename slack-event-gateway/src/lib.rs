//! The Slack Event Gateway (#659, ADR-0072).
//!
//! Slack posts here; this process verifies the delivery, projects it to
//! coordinates, and puts those on the operator's Pub/Sub topic. totsuka pulls
//! them when it is running, which is what makes it safe for totsuka to not be
//! running — the failure this whole design exists to remove.
//!
//! # What this process must never do
//!
//! **Store, forward, or log the message body.** The only thing it may do with
//! the text is compare it against constant strings. It calls no external API
//! to interpret a message — no LLM, nothing. The one outbound call is the
//! Pub/Sub publish, which is the point.
//!
//! The record schema has no field for a body, but that is not the guarantee:
//! a process can keep a string in memory, print it, or post it elsewhere. The
//! guarantee is this code and the tests that pin it.
//!
//! # Configuration
//!
//! | Variable | Meaning |
//! |---|---|
//! | `REGISTRATIONS_PATH` | File holding the registration table (a mounted Secret Manager secret). Preferred |
//! | `REGISTRATIONS` | The table inline, for environments without a mount |
//! | `PORT` | Listen port. Cloud Run sets this; defaults to 8080 |
//! | `PUBSUB_URL` | Pub/Sub base URL. For tests |

#![forbid(unsafe_code)]

pub mod http;
pub mod project;
pub mod publish;
pub mod registry;
pub mod signature;
