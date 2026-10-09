//! GPU and hardware-encoder discovery.
//!
//! Everything Archy decides about quality tiers and which GPU records starts here:
//! which adapters exist, which displays they drive, and which fixed-function
//! encoders (H.264 / HEVC / AV1) each one exposes through Media Foundation.

pub mod probe;
