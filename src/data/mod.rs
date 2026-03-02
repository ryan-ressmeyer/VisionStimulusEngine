//! Experiment data recording infrastructure.
//!
//! See [`ExperimentSession`] for the main entry point and
//! [`DataWriter`] for implementing custom backends.

pub mod messages;
mod session;
mod csv_writer;
mod parquet_writer;
mod writer;

pub use writer::{DataError, DataWriter};
pub use session::{ExperimentSession, ExperimentSessionBuilder, OverflowBehavior};
pub use csv_writer::CsvDataWriter;
pub use parquet_writer::ParquetDataWriter;
