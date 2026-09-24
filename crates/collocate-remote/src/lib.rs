pub mod client;
pub mod config;
pub mod control;
pub mod http;
pub mod tls;

pub use collocate_core::client::Api;
pub use collocate_core::limits::Limits;
pub use collocate_core::net::{Proto, Publish};
pub use collocate_core::request::{ContainerInfo, ContainerStats, HealthState, LogSource, RegistryCredential, Request, Response};
pub use collocate_core::spec::{ImageKind, Labels, Mount, RestartPolicy, RootSource, Series, Spec};
pub use collocate_core::{ContainerId, Error, Result};
pub use collocate_image::config::ImageMeta;
pub use control::{Collocate, ExecOptions, LogWindow, Logs, RunBuilder};

pub const API_PREFIX: &str = "/1.0";
pub const CHANNEL_STDIN: u8 = 0;
pub const CHANNEL_STDOUT: u8 = 1;
pub const CHANNEL_STDERR: u8 = 2;
pub const CHANNEL_CONTROL: u8 = 3;
pub const CHANNEL_EXIT: u8 = 4;

pub fn http_status(code: i32) -> u16 {
    match code {
        2 => 400,
        3 => 404,
        4 => 502,
        5 => 409,
        6 => 504,
        7 => 503,
        8 | 9 => 403,
        _ => 500,
    }
}
