//! Presentation layer (MVC) for the JSON-RPC 2.0 protocol.
//!
//! * [`model`] — request DTOs: parse & validate raw `params` into typed commands.
//! * [`view`] — renders JSON-RPC success / error envelopes.
//! * [`controller`] — routes a method name to a use case and turns its result into a view.

pub mod controller;
pub mod model;
pub mod view;

pub use controller::RpcDispatcher;
