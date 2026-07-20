//! Anthropic Messages ↔ Grok ConversationRequest translation.

pub mod request;
pub mod stream;

pub use request::{TranslateError, translate_messages_request};
pub use stream::{AnthropicOut, StreamReducer, anthropic_out_debug, event_type_name};
