//! VALSYNC — VALORANT terminal settings manager (library crate).
//!
//! The binary (`valsync.exe`) is a thin layer over this library so the engine
//! can be tested end-to-end without a terminal.

pub mod accounts;
pub mod appconfig;
pub mod catalog;
pub mod cloud;
pub mod db;
pub mod engine;
pub mod game;
pub mod ini;
pub mod paths;
pub mod profile;
pub mod matches;
pub mod rank;
pub mod riot;
pub mod service;
pub mod txn;
pub mod bench;
pub mod cli;
pub mod commands;
pub mod sample;
pub mod term;
pub mod tui;
