//! This is a [Rust](https://rust-lang.org/) library to help you create chat bots for [racetime.gg](https://racetime.gg/).
//!
//! For documentation, see also <https://github.com/racetimeGG/racetime-app/wiki/Category-bots>.

#![deny(rust_2018_idioms, unused, unused_crate_dependencies, unused_import_braces, unused_qualifications, warnings)]
#![forbid(unsafe_code)]

pub use crate::{
    bot::Bot,
    handler::RaceHandler,
};

pub mod bot;
pub mod handler;
pub mod model;
