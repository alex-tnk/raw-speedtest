//! The platform-neutral view layer: everything the native shells show, as
//! plain values. Shells lay these out with their own controls and draw the
//! chart scenes with their own graphics API, so every platform shows the
//! same text, numbers and charts.

pub mod chart;
pub mod core;
pub mod format;
pub mod frame;
pub mod log;
pub mod palette;
pub mod scene;
pub mod tip;
