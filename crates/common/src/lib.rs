//! Contracts shared between the API and the worker. Kept in one place so the two can never drift
//! out of agreement about the key format, or about how text becomes a vector.
pub mod embedding;
pub mod key;
