//! Experiment data recording infrastructure.
//!
//! See [`ExperimentSession`] for the main entry point and
//! [`DataWriter`] for implementing custom backends.

mod csv_writer;
pub mod messages;
mod parquet_writer;
mod session;
mod writer;

pub use csv_writer::CsvDataWriter;
pub use parquet_writer::ParquetDataWriter;
pub use session::{ExperimentSession, ExperimentSessionBuilder, OverflowBehavior};
pub use writer::{DataError, DataWriter};
