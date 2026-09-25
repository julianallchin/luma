pub mod analysis;
pub mod cache;
pub mod decoder;
pub mod fft;
pub mod filters;
pub mod melspec;
pub(crate) mod spectrum;

pub use analysis::calculate_frequency_amplitude;
pub use cache::{
    load_or_decode_audio_shared, read_pcm_file, read_pcm_header, write_pcm_file, PcmData,
    PcmHeader, CACHE_VERSION, PCM_HEADER_LEN,
};
pub use decoder::{stereo_to_mono, SAMPLE_RATE};
pub use fft::FftService;
pub use filters::{filter_3band, highpass_filter, lowpass_filter, FilteredBands};
pub use melspec::{generate_melspec, MEL_SPEC_HEIGHT, MEL_SPEC_WIDTH};
