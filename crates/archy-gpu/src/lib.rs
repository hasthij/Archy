//! GPU side of Archy: adapter/encoder discovery, the D3D11 device the recorder
//! owns, and the video-engine converter that turns captured frames into NV12
//! for the hardware encoder without touching the game's shader cores.

pub mod convert;
pub mod convert_shader;
pub mod device;
pub mod probe;

/// Lets COM pointers cross threads. Every D3D11/MF object Archy shares is used
/// on a multithread-protected device or by free-threaded MF components.
pub struct SendPtr<T>(pub T);
unsafe impl<T> Send for SendPtr<T> {}
