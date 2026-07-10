//! Contracts shared between the API (which writes object keys) and the worker (which reads them).
//! Kept in one place so the two can never drift out of agreement about the key format.
pub mod key;
