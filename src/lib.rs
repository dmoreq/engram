//! Library surface for integration tests (binary entry lives in main.rs).

#![forbid(unsafe_code)]

pub mod audit;
pub mod build;
pub mod cli;
pub mod dashboard;
pub mod db;
pub mod doctor;
pub mod error;
pub mod extract;
pub mod hash;
pub mod insights;
pub mod journal;
pub mod mcp;
pub mod memory;
pub mod parse;
pub mod query;
pub mod repo;
pub mod service;
mod time;
pub mod walk;
