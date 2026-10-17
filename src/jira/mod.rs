pub mod attachment;
pub mod client;
pub mod issue;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "read by the issue search command")
)]
pub mod search;
