#![warn(rust_2018_idioms)]
#![allow(clippy::try_err)]
#![forbid(unsafe_code)]

//! Context Tile service
//!
//! Tiles are the links displayed on a "Firefox Home" page (displayed as
//! a new tab or default open page.) Context tiles are sponsored tiles
//! that can be offered. These tiles are provided by an advertising
//! partner (ADM). Contile provides a level of additional privacy by
//! disclosing only the minimal user info required, and providing a
//! caching system.

#[macro_use]
extern crate slog_scope;

pub mod adm;
#[macro_use]
pub mod logging;
pub mod error;
pub mod metrics;
pub mod server;
pub mod settings;
pub mod tags;
pub mod web;
