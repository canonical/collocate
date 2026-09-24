pub mod client;
pub mod config;
pub mod http;
pub mod tls;

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
