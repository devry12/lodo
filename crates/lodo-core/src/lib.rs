//! lodo-core: logika bersama untuk app (UI) dan helper (service).

pub mod domain;
pub mod hosts;
pub mod ipc;
pub mod model;
pub mod peers;
pub mod probe;
pub mod update;
pub mod webscan;

pub use model::{Config, Route, Server, ServerStatus, VpnTarget};
pub use serde_json;
