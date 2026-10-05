//! A complete, pure-Rust implementation of the [Opus audio codec](https://opus-codec.org/)
//! (RFC 6716 / RFC 8251): encoder and decoder, SILK, CELT and Hybrid modes, with no C code
//! and no dependencies.
//!
//! # Quick start
//!
//! ```
//! use rusty_opus::{Application, Error, OpusDecoder, OpusEncoder};
//!
//! # fn main() -> Result<(), Error> {
//! const RATE: i32 = 48_000;
//! const FRAME: usize = 960; // 20 ms at 48 kHz
//!
//! let mut encoder = OpusEncoder::new(RATE, 2, Application::Audio)?;
//! encoder.bitrate_bps = 96_000;
//! let pcm = vec![0.0f32; FRAME * 2]; // interleaved stereo, nominal range [-1, 1]
//! let mut packet = [0u8; 1500];
//! let len = encoder.encode(&pcm, FRAME, &mut packet)?;
//!
//! let mut decoder = OpusDecoder::new(RATE, 2)?;
//! let mut out = vec![0.0f32; FRAME * 2];
//! assert_eq!(decoder.decode(&packet[..len], FRAME, &mut out)?, FRAME);
//!
//! // An empty packet signals a lost frame: the decoder conceals it.
//! assert_eq!(decoder.decode(&[], FRAME, &mut out)?, FRAME);
//!
//! // Malformed input is an error, never a panic.
//! assert!(decoder.decode(&[0xFF, 0x03], FRAME, &mut out).is_err());
//! # Ok(())
//! # }
//! ```
//!
//! # Public API
//!
//! - [`OpusEncoder`] and [`OpusDecoder`] — mono/stereo coding, packet-loss concealment
//!   ([`OpusDecoder::decode`] with an empty packet) and in-band FEC
//!   ([`OpusDecoder::decode_fec`]).
//! - [`multistream`] — surround encoding and decoding (mapping families 0 and 1).
//! - [`repacketizer`] — merge, split, pad and unpad packets without re-encoding.
//! - [`parallel`] — frame-parallel and batch encoding across threads.
//! - [`Error`] — the error type of every fallible operation; variants mirror `libopus`
//!   error codes.
//!
//! Other modules are internal codec stages: they are public so integration tests can
//! reach them, but hidden from this documentation and not covered by semantic versioning.
//!
//! # Performance and platforms
//!
//! SIMD kernels (AVX2/FMA, AVX and SSE2 on x86; NEON on aarch64) are selected at runtime,
//! each with a scalar fallback; `RUSTY_OPUS_ISA=scalar|sse2|avx|avx2` caps the instruction
//! set. After warm-up, [`OpusEncoder::encode`] and [`OpusDecoder::decode`] perform no heap
//! allocation per frame. The crate builds
//! for every Rust target, including `wasm32`.

// The documented (non-hidden) public API is fully documented; keep it that way.
#![warn(missing_docs)]
#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]

// Internal codec stages. They stay `pub` so the integration tests can exercise
// them directly, but they are hidden from the documentation and are NOT part of
// the semver-covered API: depend only on the items re-exported at the crate root
// and the `parallel`, `repacketizer` and `multistream` modules.
#[doc(hidden)]
pub mod analysis;
mod error;
pub use error::Error;
#[doc(hidden)]
pub mod analysis_data;
#[doc(hidden)]
pub mod bands;
#[doc(hidden)]
pub mod celt;
#[doc(hidden)]
pub mod celt_lpc;
#[doc(hidden)]
pub mod hp_cutoff;
#[doc(hidden)]
pub mod isa;
#[doc(hidden)]
pub mod kiss_fft;
#[doc(hidden)]
pub mod mdct;
#[doc(hidden)]
pub mod modes;
pub mod multistream;
pub mod parallel;
#[doc(hidden)]
pub mod pitch;
#[doc(hidden)]
pub mod prof;
#[doc(hidden)]
pub mod pvq;
#[doc(hidden)]
pub mod quant_bands;
#[doc(hidden)]
pub mod range_coder;
#[doc(hidden)]
pub mod rate;
pub mod repacketizer;
#[doc(hidden)]
pub mod silk;

#[doc(hidden)]
pub use silk::{SilkResampler, SilkResamplerDown1_3, SilkResamplerDown1_6};

// Low-level stage types: hidden (not semver-covered), see the module note above.
#[doc(hidden)]
pub use celt::{CeltDecoder, CeltEncoder};
use hp_cutoff::hp_cutoff;
use range_coder::RangeCoder;
use silk::control_codec::silk_control_encoder;
use silk::enc_api::silk_encode;
use silk::init_encoder::silk_init_encoder;
use silk::lin2log::silk_lin2log;
use silk::log2lin::silk_log2lin;
use silk::macros::*;
use silk::structs::SilkEncoderState;

/// The intended use of an encoder, which steers its mode and tuning decisions
/// (libopus `OPUS_APPLICATION_*`; the discriminants are the libopus values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    /// Interactive speech: favours intelligibility and SILK/Hybrid coding.
    Voip = 2048,
    /// General audio and music: favours fidelity to the input.
    Audio = 2049,
    /// Lowest possible latency: CELT only, no speech-optimised modes.
    RestrictedLowDelay = 2051,
}

/// OPUS_SET_SIGNAL hint: bias mode selection toward speech or music. `None` =
/// OPUS_AUTO (let the analysis decide).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    /// The input is predominantly speech.
    Voice,
    /// The input is predominantly music.
    Music,
}

/// Audio bandwidth of a coded stream (libopus `OPUS_BANDWIDTH_*`; the
/// discriminants are the libopus values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    /// Let the encoder choose from the bitrate and content.
    Auto = -1000,
    /// 4 kHz audio bandwidth (8 kHz sampling).
    Narrowband = 1101,
    /// 6 kHz audio bandwidth (12 kHz sampling).
    Mediumband = 1102,
    /// 8 kHz audio bandwidth (16 kHz sampling).
    Wideband = 1103,
    /// 12 kHz audio bandwidth (24 kHz sampling).
    Superwideband = 1104,
    /// 20 kHz audio bandwidth (48 kHz sampling).
    Fullband = 1105,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpusMode {
    SilkOnly,
    Hybrid,
    CeltOnly,
}

/// An Opus encoder for one mono or stereo stream.
///
/// Create it with [`OpusEncoder::new`], adjust the public fields (bitrate,
/// complexity, CBR, FEC, DTX, ...) as needed, then call [`OpusEncoder::encode`]
/// once per frame. After the first few frames, encoding performs no heap
/// allocation.
pub struct OpusEncoder {
    celt_enc: CeltEncoder,
    silk_enc: Box<SilkEncoderState>,
    application: Application,
    sampling_rate: i32,
    channels: usize,
    bandwidth: Bandwidth,
    /// Target bitrate in bits per second (default 64 000).
    pub bitrate_bps: i32,
    /// Computational complexity, 0 (fastest) to 10 (best quality).
    pub complexity: i32,
    /// Constant bitrate when `true`; variable bitrate (the default) when `false`.
    pub use_cbr: bool,

    /// Embed in-band forward error correction (SILK LBRR) so the next packet can
    /// recover a lost one; most useful with `packet_loss_perc > 0`.
    pub use_inband_fec: bool,

    /// Discontinuous transmission: after enough consecutive inactive frames,
    /// emit a 1-byte (TOC-only) packet so the decoder runs comfort-noise/PLC.
    pub use_dtx: bool,
    /// Consecutive inactive milliseconds, in Q1 (opus_encoder.c nb_no_activity).
    nb_no_activity_ms_q1: i32,
    /// Final range-coder state of the last packet (0 for DTX/PLC packets, which
    /// carry no coded range — opus_encoder.c st->rangeFinal).
    range_final: u32,

    /// Expected packet-loss percentage (0-100); raises robustness at some
    /// cost in quality.
    pub packet_loss_perc: i32,
    silk_initialized: bool,
    mode: OpusMode,
    prev_enc_mode: Option<OpusMode>,

    variable_hp_smth2_q15: i32,
    /// Rate-dependent automatic bandwidth (libopus auto_bandwidth), stored as the
    /// Bandwidth discriminant (1101 NB .. 1105 FB). Hysteresis state.
    auto_bandwidth: i32,
    first_frame: bool,
    /// Overrides automatic bandwidth selection when set (OPUS_SET_BANDWIDTH).
    pub force_bandwidth: Option<Bandwidth>,
    /// OPUS_SET_SIGNAL: force the voice/music bias (None = auto from analysis).
    pub signal_type: Option<SignalType>,
    /// OPUS_SET_MAX_BANDWIDTH: cap the automatically-selected bandwidth.
    pub max_bandwidth: Bandwidth,
    /// Tonality/music/bandwidth analysis (libopus src/analysis.c); runs when
    /// complexity >= 7 and the API rate is >= 16 kHz.
    tonality: analysis::TonalityAnalysisState,
    analysis_kfft: Option<kiss_fft::KissFftState>,
    /// Input bit depth assumed by the analysis noise floors. The float API
    /// default is 24; set 16 for s16-sourced content (opus_demo parity).
    pub lsb_depth: i32,
    /// 0..100 voice probability from the analysis (-1 = unknown), C voice_ratio.
    voice_ratio: i32,
    detected_bandwidth: i32,
    hp_mem: Vec<i32>,

    buf_filtered: Vec<i16>,
    buf_silk_input: Vec<i16>,
    buf_stereo_mid: Vec<i16>,
    buf_stereo_side: Vec<i16>,
    buf_celt_input: Vec<f32>,
    down2_state_first: [i32; 2],
    down2_state_second: [i32; 2],
    down2_3_state: [i32; 6],
    down_1_3_state: silk::resampler::SilkResamplerDown1_3,
    down2_3_state_r: [i32; 6],
    down_1_3_state_r: silk::resampler::SilkResamplerDown1_3,
    down_fir_l: Option<silk::resampler::SilkDownFirResampler>,
    down_fir_r: Option<silk::resampler::SilkDownFirResampler>,
    /// Last 10 ms of API-rate mono input, for the SILK prefill after a
    /// CELT-only -> SILK/hybrid transition (opus_encoder.c:1449 prefill=1).
    silk_prefill_tail: Vec<i16>,
    silk_prefill_pending: bool,
    buf_left: Vec<i16>,
    buf_right: Vec<i16>,
    /// Last 2.5 ms of the previous frame's input (planar), for the CELT
    /// prefill after a mode-transition reset (opus_encoder.c:2060).
    celt_prefill_tail: Vec<f32>,

    rc: RangeCoder,

    // ---- Great Gate P1 instrumentation (docs/great-gate.md) ----
    /// Observe-only harvest tap: when `RUSTY_OPUS_GATE_HARVEST=<path>` is set at
    /// construction, every encoded frame appends one CSV row with the signals
    /// the mode/bandwidth decision consumed plus the outcome (mode, bw, bytes).
    /// The bitstream is byte-identical on or off — the tap only reads. Env is
    /// read ONCE here, never per frame. Serial encoders only (the parallel path
    /// would interleave rows).
    gate_tap: Option<std::io::BufWriter<std::fs::File>>,
    /// Clip label stamped into harvest rows (`RUSTY_OPUS_GATE_CLIP`).
    gate_clip: String,
    /// Frame counter for harvest rows.
    gate_frame: u64,
    /// Truth-table lever: `RUSTY_OPUS_FORCE_MODE=silk|celt|hybrid` pins the
    /// coding mode after the auto decision (bandwidth reconciled to a valid TOC
    /// config). Unset = None = byte-identical to shipped behavior.
    force_mode: Option<OpusMode>,
    /// Mode-dwell hysteresis: a proposed mode change must persist this many
    /// consecutive frames before it is committed. **1 = OFF and
    /// byte-identical**; set via `RUSTY_OPUS_MODE_DWELL`.
    ///
    /// Measured ineffective for the startup-mode defect it was built for and
    /// left default-off — see the refutation at its use site in `encode`.
    pub mode_dwell: u32,
    /// Consecutive frames the current proposal has differed from the coded mode.
    mode_dwell_run: u32,
    /// Analysis warm-up guard: ignore the tonality classifier's verdict for
    /// this many analysis frames and fall back to the application default.
    /// **Default 10** (`RUSTY_OPUS_ANALYSIS_WARMUP`; 0 = OFF and restores the
    /// pre-2026-08-07 byte-identical behaviour).
    ///
    /// libopus feeds its analysis a lookahead buffer, so the classifier is
    /// already converged when the first frame is coded. We call `run_analysis`
    /// with `analysis_frame_size == frame_size` — zero lookahead — so on our
    /// encoder the classifier spends its first ~20 frames climbing from
    /// "voice" to its steady-state verdict. On music-ish content that made the
    /// first 480 ms code as hybrid before flipping to CELT for good.
    analysis_warmup: u32,
    /// Analysis frames seen (saturating), compared against `analysis_warmup`.
    analysis_frames: u32,
    /// Multi-frame packet in progress (opus_encode_native's >20 ms CELT/hybrid
    /// and >60 ms paths): mode + bandwidth decided ONCE for the whole packet,
    /// then each sub-frame is coded with them locked so every sub-frame
    /// carries the same TOC config (a code-3 packet requires it).
    mf_lock: Option<(OpusMode, Bandwidth)>,
    /// Multi-frame packet assembly: reused across calls so 40-120 ms frames
    /// encode without allocating once warmed up.
    mf_rp: repacketizer::Repacketizer,
    mf_buf: Vec<u8>,
    /// Set by encode_multiframe for the last sub-frame of a to_celt packet.
    mf_to_celt: bool,
}

// libopus opus_encoder.c bandwidth thresholds: (threshold, hysteresis) pairs for
// NB<->MB, MB<->WB, WB<->SWB, SWB<->FB, interpolated voice<->music by voice_est^2.
const MONO_VOICE_BANDWIDTH_THRESHOLDS: [i32; 8] = [9000, 700, 9000, 700, 13500, 1000, 14000, 2000];
const MONO_MUSIC_BANDWIDTH_THRESHOLDS: [i32; 8] = [9000, 700, 9000, 700, 11000, 1000, 12000, 2000];
const STEREO_VOICE_BANDWIDTH_THRESHOLDS: [i32; 8] =
    [9000, 700, 9000, 700, 13500, 1000, 14000, 2000];
const STEREO_MUSIC_BANDWIDTH_THRESHOLDS: [i32; 8] =
    [9000, 700, 9000, 700, 11000, 1000, 12000, 2000];

/// Coerce a bandwidth to one the given mode can actually signal in the TOC:
/// CELT has no mediumband config, SILK-only tops out at wideband, and hybrid
/// exists only at SWB/FB. Used wherever a mode is overridden after the
/// bandwidth has already been chosen (dwell hysteresis, forced mode).
fn reconcile_bandwidth(mode: OpusMode, bw: Bandwidth) -> Bandwidth {
    match mode {
        // libopus: "CELT mode doesn't support mediumband, use wideband instead".
        OpusMode::CeltOnly if bw == Bandwidth::Mediumband => Bandwidth::Wideband,
        OpusMode::SilkOnly if matches!(bw, Bandwidth::Superwideband | Bandwidth::Fullband) => {
            Bandwidth::Wideband
        }
        OpusMode::Hybrid if !matches!(bw, Bandwidth::Superwideband | Bandwidth::Fullband) => {
            Bandwidth::Superwideband
        }
        _ => bw,
    }
}

/// Development toggles read from the environment: mode forcing, A/B switches
/// and the tuning-harvest log used by the `tools/gate_*` research harnesses.
///
/// Always `None` unless the crate is built with the `research` feature, so the
/// behaviour of a production build can never be changed by its environment.
/// (The one documented variable, `RUSTY_OPUS_ISA`, only caps the SIMD level and
/// is read in `crate::isa`.)
#[inline]
pub(crate) fn research_env(name: &str) -> Option<std::ffi::OsString> {
    #[cfg(feature = "research")]
    {
        std::env::var_os(name)
    }
    #[cfg(not(feature = "research"))]
    {
        let _ = name;
        None
    }
}

fn research_str(name: &str) -> Option<String> {
    research_env(name).and_then(|v| v.into_string().ok())
}

fn research_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    research_str(name).and_then(|s| s.parse().ok())
}

/// The tuning-harvest CSV (`RUSTY_OPUS_GATE_HARVEST=<path>`), research builds only.
#[cfg(feature = "research")]
fn open_gate_tap() -> Option<std::io::BufWriter<std::fs::File>> {
    let p = research_str("RUSTY_OPUS_GATE_HARVEST")?;
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&p)
        .ok()?;
    if f.metadata().map_or(0, |m| m.len()) == 0 {
        let _ = writeln!(
            f,
            "clip,frame,mode,bw,ch,bitrate,complexity,equiv,voice_est,\
                 is_silence,active,valid,tonality,tonality_slope,noisiness,\
                 activity_prob,music_prob,music_prob_min,music_prob_max,\
                 det_bw,max_pitch_ratio,bytes"
        );
    }
    Some(std::io::BufWriter::new(f))
}

#[cfg(not(feature = "research"))]
fn open_gate_tap() -> Option<std::io::BufWriter<std::fs::File>> {
    None
}

/// opus_encoder.c compute_redundancy_bytes.
fn compute_redundancy_bytes(
    max_data_bytes: usize,
    bitrate_bps: i32,
    frame_rate: i32,
    channels: usize,
) -> usize {
    let base_bits = 40 * channels as i32 + 20;
    // Equivalent rate for 5 ms frames; 1.5x for VBR (short, avoids artefacts).
    let redundancy_rate = bitrate_bps + base_bits * (200 - frame_rate);
    let redundancy_rate = 3 * redundancy_rate / 2;
    let mut redundancy_bytes = redundancy_rate / 1600;
    // The max rate we can use given CBR or VBR with cap.
    let available_bits = max_data_bytes as i32 * 8 - 2 * base_bits;
    let cap = (available_bits * 240 / (240 + 48000 / frame_rate) + base_bits) / 8;
    redundancy_bytes = redundancy_bytes.min(cap);
    if redundancy_bytes > 4 + 8 * channels as i32 {
        redundancy_bytes.min(257) as usize
    } else {
        0
    }
}

fn compute_equiv_rate(
    bitrate: i32,
    channels: usize,
    frame_rate: i32,
    vbr: bool,
    complexity: i32,
    loss: i32,
) -> i32 {
    let mut equiv = bitrate;
    if frame_rate > 50 {
        equiv -= (40 * channels as i32 + 20) * (frame_rate - 50);
    }
    if !vbr {
        equiv -= equiv / 12;
    }
    equiv = equiv * (90 + complexity) / 100;
    if loss > 0 {
        equiv -= equiv * loss / (12 * loss + 20);
    }
    equiv
}

fn compute_mode_threshold(
    application: Application,
    channels: usize,
    prev_was_celt: bool,
    has_prev_mode: bool,
    voice_est: i32,
) -> i32 {
    let mode_voice = if channels == 1 { 64000 } else { 44000 };
    let mode_music = 10000;

    let diff = mode_voice - mode_music;
    let offset = (voice_est * voice_est * diff) >> 14;
    let mut threshold = mode_music + offset;

    if application == Application::Voip {
        threshold += 8000;
    }

    if has_prev_mode {
        if prev_was_celt {
            threshold -= 4000;
        } else {
            threshold += 4000;
        }
    }

    if application == Application::RestrictedLowDelay {
        threshold = 0;
    }

    threshold
}

fn compute_silk_rate_for_hybrid(
    rate_bps: i32,
    bandwidth: Bandwidth,
    frame20ms: bool,
    vbr: bool,
) -> i32 {
    const RATE_TABLE: &[(i32, i32, i32)] = &[
        (0, 0, 0),
        (12000, 10000, 10000),
        (16000, 13500, 13500),
        (20000, 16000, 16000),
        (24000, 18000, 18000),
        (32000, 22000, 22000),
        (64000, 38000, 38000),
    ];
    let n = RATE_TABLE.len();
    let mut i = 1;
    while i < n && RATE_TABLE[i].0 <= rate_bps {
        i += 1;
    }
    let mut silk_rate = if i == n {
        let (x_last, r10_last, r20_last) = RATE_TABLE[n - 1];
        let base = if frame20ms { r20_last } else { r10_last };
        base + (rate_bps - x_last) / 2
    } else {
        let (x0, lo10, lo20) = RATE_TABLE[i - 1];
        let (x1, hi10, hi20) = RATE_TABLE[i];
        let (lo, hi) = if frame20ms {
            (lo20, hi20)
        } else {
            (lo10, hi10)
        };
        (lo * (x1 - rate_bps) + hi * (rate_bps - x0)) / (x1 - x0)
    };
    // C tail adjustments (opus_encoder.c:789): tiny SILK boost for CBR, and
    // +300 for SWB hybrid (the CELT part starts at band 17 either way but
    // covers less spectrum, so SILK earns a bigger share).
    if !vbr {
        silk_rate += 100;
    }
    if bandwidth == Bandwidth::Superwideband {
        silk_rate += 300;
    }
    silk_rate
}

#[cfg(test)]
mod reconcile_bandwidth_tests {
    use super::{Bandwidth, OpusMode, reconcile_bandwidth};

    #[test]
    fn celt_maps_mediumband_up_to_wideband() {
        // The CELT TOC has no mediumband config; libopus uses WIDEBAND
        // (opus_encoder.c:1680). Mapping it DOWN to NB was a divergence.
        assert_eq!(
            reconcile_bandwidth(OpusMode::CeltOnly, Bandwidth::Mediumband),
            Bandwidth::Wideband
        );
    }

    #[test]
    fn celt_leaves_every_other_bandwidth_alone() {
        for bw in [
            Bandwidth::Narrowband,
            Bandwidth::Wideband,
            Bandwidth::Superwideband,
            Bandwidth::Fullband,
        ] {
            assert_eq!(reconcile_bandwidth(OpusMode::CeltOnly, bw), bw);
        }
    }

    #[test]
    fn silk_only_caps_at_wideband() {
        assert_eq!(
            reconcile_bandwidth(OpusMode::SilkOnly, Bandwidth::Superwideband),
            Bandwidth::Wideband
        );
        assert_eq!(
            reconcile_bandwidth(OpusMode::SilkOnly, Bandwidth::Fullband),
            Bandwidth::Wideband
        );
        // At or below wideband it is already codeable.
        for bw in [
            Bandwidth::Narrowband,
            Bandwidth::Mediumband,
            Bandwidth::Wideband,
        ] {
            assert_eq!(reconcile_bandwidth(OpusMode::SilkOnly, bw), bw);
        }
    }

    #[test]
    fn hybrid_floors_at_superwideband() {
        for bw in [
            Bandwidth::Narrowband,
            Bandwidth::Mediumband,
            Bandwidth::Wideband,
        ] {
            assert_eq!(
                reconcile_bandwidth(OpusMode::Hybrid, bw),
                Bandwidth::Superwideband
            );
        }
        // Hybrid exists only at SWB/FB, so those pass through.
        assert_eq!(
            reconcile_bandwidth(OpusMode::Hybrid, Bandwidth::Superwideband),
            Bandwidth::Superwideband
        );
        assert_eq!(
            reconcile_bandwidth(OpusMode::Hybrid, Bandwidth::Fullband),
            Bandwidth::Fullband
        );
    }
}

#[cfg(test)]
mod silk_rate_tests {
    use super::compute_silk_rate_for_hybrid;
    use crate::Bandwidth;

    #[test]
    fn test_reference_table_exact_entries() {
        assert_eq!(
            compute_silk_rate_for_hybrid(12000, Bandwidth::Fullband, true, true),
            10000
        );
        assert_eq!(
            compute_silk_rate_for_hybrid(16000, Bandwidth::Fullband, true, true),
            13500
        );
        assert_eq!(
            compute_silk_rate_for_hybrid(20000, Bandwidth::Fullband, true, true),
            16000
        );
        assert_eq!(
            compute_silk_rate_for_hybrid(24000, Bandwidth::Fullband, true, true),
            18000
        );
        assert_eq!(
            compute_silk_rate_for_hybrid(32000, Bandwidth::Fullband, true, true),
            22000
        );
        assert_eq!(
            compute_silk_rate_for_hybrid(64000, Bandwidth::Fullband, true, true),
            38000
        );
    }

    #[test]
    fn test_32kbps_gives_22kbps_silk() {
        assert_eq!(
            compute_silk_rate_for_hybrid(32000, Bandwidth::Fullband, true, true),
            22000
        );
    }

    #[test]
    fn test_interpolation_between_table_entries() {
        let r = compute_silk_rate_for_hybrid(18000, Bandwidth::Fullband, true, true);
        assert_eq!(r, 14750);
    }

    #[test]
    fn test_above_table_max_gives_half_extra() {
        let r = compute_silk_rate_for_hybrid(72000, Bandwidth::Fullband, true, true);
        assert_eq!(r, 38000 + (72000 - 64000) / 2);
    }
}

impl OpusEncoder {
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `sampling_rate` is not 8000, 12000, 16000, 24000 or
    /// 48000 Hz or `channels` is not 1 or 2; [`Error::Internal`] if the SILK
    /// encoder fails to initialise.
    pub fn new(
        sampling_rate: i32,
        channels: usize,
        application: Application,
    ) -> Result<Self, Error> {
        if ![8000, 12000, 16000, 24000, 48000].contains(&sampling_rate) {
            return Err(Error::BadArg("Invalid sampling rate"));
        }
        if ![1, 2].contains(&channels) {
            return Err(Error::BadArg("Invalid number of channels"));
        }

        let mode = modes::default_mode();
        let mut celt_enc = CeltEncoder::new(mode, channels);
        // CELT always codes a 48 kHz frame (libopus resampling_factor).
        celt_enc.upsample = (48000 / sampling_rate) as usize;

        let mut silk_enc = Box::new(SilkEncoderState::default());
        if silk_init_encoder(&mut silk_enc, 0) != 0 {
            return Err(Error::Internal("SILK encoder initialization failed"));
        }

        let (opus_mode, bw) = match application {
            Application::Voip => {
                let bw = match sampling_rate {
                    8000 => Bandwidth::Narrowband,
                    12000 => Bandwidth::Mediumband,
                    16000 => Bandwidth::Wideband,
                    24000 => Bandwidth::Superwideband,
                    48000 => Bandwidth::Fullband,
                    _ => Bandwidth::Narrowband,
                };

                let mode = if sampling_rate > 16000 {
                    OpusMode::Hybrid
                } else {
                    OpusMode::SilkOnly
                };
                (mode, bw)
            }
            Application::RestrictedLowDelay => {
                let bw = match sampling_rate {
                    8000 => Bandwidth::Narrowband,
                    12000 => Bandwidth::Mediumband,
                    16000 => Bandwidth::Wideband,
                    24000 => Bandwidth::Superwideband,
                    _ => Bandwidth::Fullband,
                };
                (OpusMode::CeltOnly, bw)
            }
            Application::Audio => {
                if sampling_rate <= 16000 {
                    let bw = match sampling_rate {
                        8000 => Bandwidth::Narrowband,
                        12000 => Bandwidth::Mediumband,
                        _ => Bandwidth::Wideband,
                    };
                    (OpusMode::SilkOnly, bw)
                } else {
                    let bw = match sampling_rate {
                        24000 => Bandwidth::Superwideband,
                        _ => Bandwidth::Fullband,
                    };
                    (OpusMode::Hybrid, bw)
                }
            }
        };

        use silk::lin2log::silk_lin2log;
        let variable_hp_smth2_q15 = silk_lin2log(60) << 8;

        Ok(Self {
            celt_enc,
            silk_enc,
            application,
            sampling_rate,
            channels,
            bandwidth: bw,
            bitrate_bps: 64000,
            complexity: 9,
            use_cbr: false,
            use_inband_fec: false,
            use_dtx: false,
            nb_no_activity_ms_q1: 0,
            range_final: 0,
            packet_loss_perc: 0,
            silk_initialized: false,
            prev_enc_mode: None,
            mode: opus_mode,
            variable_hp_smth2_q15,
            auto_bandwidth: 0,
            first_frame: true,
            force_bandwidth: None,
            signal_type: None,
            max_bandwidth: Bandwidth::Fullband,
            tonality: analysis::TonalityAnalysisState::new(sampling_rate),
            analysis_kfft: kiss_fft::KissFftState::new(480),
            // Float-API default, faithful to opus_encoder.c. `RUSTY_OPUS_LSB_DEPTH`
            // overrides it for the D1 bandwidth-detector investigation: the
            // analysis noise floor is (5.7e-4 / 2^(lsb_depth-8))^2, so feeding
            // s16-sourced material at depth 24 puts the floor 2^16 too low.
            lsb_depth: research_parse("RUSTY_OPUS_LSB_DEPTH").unwrap_or(24),
            voice_ratio: -1,
            detected_bandwidth: 0,
            hp_mem: vec![0; channels * 2],

            buf_filtered: Vec::new(),
            buf_silk_input: Vec::new(),
            buf_stereo_mid: Vec::new(),
            buf_stereo_side: Vec::new(),
            buf_celt_input: Vec::new(),
            down2_state_first: [0; 2],
            down2_state_second: [0; 2],
            down2_3_state: [0; 6],
            down_1_3_state: silk::resampler::SilkResamplerDown1_3::default(),
            down2_3_state_r: [0; 6],
            down_1_3_state_r: silk::resampler::SilkResamplerDown1_3::default(),
            down_fir_l: None,
            down_fir_r: None,
            silk_prefill_tail: Vec::new(),
            silk_prefill_pending: false,
            buf_left: Vec::new(),
            buf_right: Vec::new(),
            celt_prefill_tail: Vec::new(),
            rc: RangeCoder::new_encoder(1),
            gate_tap: open_gate_tap(),
            gate_clip: research_str("RUSTY_OPUS_GATE_CLIP").unwrap_or_default(),
            gate_frame: 0,
            force_mode: match research_str("RUSTY_OPUS_FORCE_MODE").as_deref() {
                Some("silk") => Some(OpusMode::SilkOnly),
                Some("celt") => Some(OpusMode::CeltOnly),
                Some("hybrid") => Some(OpusMode::Hybrid),
                _ => None,
            },
            mode_dwell: research_parse("RUSTY_OPUS_MODE_DWELL").unwrap_or(1),
            mode_dwell_run: 0,
            // DEFAULT-ON at 10 since 2026-08-07: 14 wins / 0 losses / 1 neutral
            // (-0.005) over a 65-rung, 13-class PEAQ ladder, with all VoIP
            // classes bit-for-bit unchanged. `RUSTY_OPUS_ANALYSIS_WARMUP=0`
            // restores the previous byte-identical behaviour.
            analysis_warmup: research_parse("RUSTY_OPUS_ANALYSIS_WARMUP").unwrap_or(10),
            analysis_frames: 0,
            mf_lock: None,
            mf_rp: repacketizer::Repacketizer::new(),
            mf_buf: Vec::new(),
            mf_to_celt: false,
        })
    }

    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] unless the encoder runs at 24 or 48 kHz, the only rates
    /// where hybrid (SILK + CELT) coding exists.
    pub fn enable_hybrid_mode(&mut self) -> Result<(), Error> {
        if self.sampling_rate != 24000 && self.sampling_rate != 48000 {
            return Err(Error::BadArg(
                "Hybrid mode requires 24kHz or 48kHz sampling rate",
            ));
        }
        let bw = if self.sampling_rate == 48000 {
            Bandwidth::Fullband
        } else {
            Bandwidth::Superwideband
        };
        self.mode = OpusMode::Hybrid;
        self.bandwidth = bw;
        self.silk_initialized = false;
        Ok(())
    }

    /// Final range-coder state of the last encoded packet (libopus
    /// OPUS_GET_FINAL_RANGE). Stored in opus_demo `.bit` framing so the reference
    /// decoder can verify encoder/decoder range-coder agreement per packet.
    pub fn final_range(&self) -> u32 {
        self.range_final
    }

    /// opus_encoder.c:1296 voice_est ladder: forced by `signal_type` when set,
    /// else analysis-driven when voice_ratio is known, else application defaults.
    fn compute_voice_est(&self) -> i32 {
        match self.signal_type {
            Some(SignalType::Voice) => return 127,
            Some(SignalType::Music) => return 0,
            None => {}
        }
        if self.voice_ratio >= 0 {
            let mut v = (self.voice_ratio * 327) >> 8;
            // For AUDIO, never be more than 90% confident of having speech.
            if self.application == Application::Audio {
                v = v.min(115);
            }
            v
        } else {
            match self.application {
                Application::Voip => 115,
                Application::Audio => 48,
                Application::RestrictedLowDelay => 0,
            }
        }
    }

    /// opus_encode_native's multi-frame path: `frame_size` exceeds what one
    /// Opus frame of `mode` can hold (>20 ms CELT/hybrid, >60 ms anything), so
    /// code it as several sub-frames with the packet's mode + bandwidth locked,
    /// and join them into one code-1/2/3 packet. Sub-frame size as libopus:
    /// SILK 80 ms = 2x40, 120 ms = 2x60, 100 ms = 5x20; CELT/hybrid 20 ms.
    fn encode_multiframe(
        &mut self,
        input: &[f32],
        frame_size: usize,
        output: &mut [u8],
        mode: OpusMode,
        to_celt: bool,
    ) -> Result<usize, Error> {
        let fs = self.sampling_rate as usize;
        let enc_frame_size = if mode == OpusMode::SilkOnly {
            if frame_size == 2 * fs / 25 {
                fs / 25
            } else if frame_size == 3 * fs / 25 {
                3 * fs / 50
            } else {
                fs / 50
            }
        } else {
            fs / 50
        };
        let nb_frames = frame_size / enc_frame_size;
        // Worst-case repacketizer header: code 2 = 3 bytes, code-3 VBR =
        // 2 + 2 per length field (opus_encoder.c max_header_bytes).
        let max_header = if nb_frames == 2 {
            3
        } else {
            2 + (nb_frames - 1) * 2
        };
        let per_frame = (output.len().saturating_sub(max_header) / nb_frames).clamp(2, 1276);
        let ch = self.channels;

        self.mf_lock = Some((mode, self.bandwidth));
        // Taken out of `self` for the loop (the sub-frame encodes borrow it);
        // `mem::take` of a Vec does not allocate, and both go back below.
        let mut rp = std::mem::take(&mut self.mf_rp);
        let mut buf = std::mem::take(&mut self.mf_buf);
        rp.reset();
        buf.resize(per_frame, 0);
        let mut result = Ok(());
        for i in 0..nb_frames {
            let sub = &input[i * enc_frame_size * ch..(i + 1) * enc_frame_size * ch];
            // libopus: only the last frame of the packet asks for the switch.
            self.mf_to_celt = to_celt && i == nb_frames - 1;
            let r = self.encode(sub, enc_frame_size, &mut buf);
            self.mf_to_celt = false;
            match r {
                Ok(n) => {
                    if let Err(e) = rp.cat(&buf[..n]) {
                        result = Err(e);
                        break;
                    }
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        self.mf_lock = None;
        let written = result.and_then(|()| {
            let len = rp.out_into(output)?;
            // CBR: pad the joined packet to the packet's byte budget, as libopus
            // does when repacketizing a non-VBR multi-frame packet.
            if self.use_cbr {
                let target =
                    ((self.bitrate_bps as i64 * frame_size as i64) / (8 * fs as i64)) as usize;
                let target = target.min(output.len());
                if target > len {
                    return rp.out_padded_into(target, output);
                }
            }
            Ok(len)
        });
        self.mf_rp = rp;
        self.mf_buf = buf;
        written
    }

    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `frame_size` is not a valid Opus frame duration
    /// (2.5–120 ms) at this sampling rate or for the selected mode;
    /// [`Error::BufferTooSmall`] if `output` cannot hold the packet;
    /// [`Error::Internal`] if a codec stage fails.
    pub fn encode(
        &mut self,
        input: &[f32],
        frame_size: usize,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        let _prof_total = crate::prof::scope(crate::prof::Stage::Total);
        if output.len() < 2 {
            return Err(Error::BufferTooSmall("Output buffer too small"));
        }

        // Every Opus frame size (opus_encoder.c frame_size_select): 2.5/5/10/20
        // ms, then 40/60/80/100/120 ms as multiples of 20 ms. 60/100/120 ms do
        // not divide the rate evenly at 48 kHz, so they are matched explicitly
        // rather than by `Fs % frame_size` (which rejected all of them).
        let fs = self.sampling_rate as usize;
        let valid = [
            fs / 400,
            fs / 200,
            fs / 100,
            fs / 50,
            fs / 25,
            3 * fs / 50,
            4 * fs / 50,
            5 * fs / 50,
            6 * fs / 50,
        ]
        .contains(&frame_size);
        if !valid || frame_size == 0 {
            return Err(Error::BadArg("Invalid frame size for sampling rate"));
        }
        // libopus: frame_rate = Fs/frame_size (integer; 16 for 60 ms).
        let frame_rate = self.sampling_rate / frame_size as i32;

        // ---- Tonality analysis (opus_encoder.c:1123) ----
        // A multi-frame packet's sub-frames skip it: the whole packet was
        // analysed by the outer call that made the mode decision.
        let mut analysis_info = analysis::AnalysisInfo::default();
        if self.mf_lock.is_none() && self.complexity >= 7 && self.sampling_rate >= 16000 {
            if let Some(kfft) = &self.analysis_kfft {
                analysis_info = analysis::run_analysis(
                    &mut self.tonality,
                    kfft,
                    input,
                    frame_size,
                    frame_size,
                    self.channels,
                    self.sampling_rate,
                    self.lsb_depth,
                );
            }
        } else if self.tonality.initialized() {
            self.tonality.reset();
        }

        // voice_ratio / detected_bandwidth from the analysis (opus_encoder.c:1154).
        let silence_thresh = 1.0f32 / (1i64 << self.lsb_depth) as f32;
        let is_silence = input[..(frame_size * self.channels).min(input.len())]
            .iter()
            .fold(0.0f32, |m, &v| m.max(v.abs()))
            <= silence_thresh;
        if !is_silence {
            self.voice_ratio = -1;
        }
        // Voice-activity flag for DTX (opus_encoder.c:1160). Silence is always
        // inactive; with analysis, use the VAD probability; without it, assume
        // active (conservative — never DTX away real audio). We skip the
        // peak-energy SNR fallback, which only ever ADDS activity.
        let activity = if is_silence {
            false
        } else if analysis_info.valid {
            analysis_info.activity_probability >= 0.1
        } else {
            true
        };
        // Analysis warm-up guard (see the `analysis_warmup` field doc): until
        // the classifier has seen enough frames to converge, leave
        // `voice_ratio` at -1 so `compute_voice_est` uses the APPLICATION
        // default instead of a half-climbed verdict. That is the right answer
        // for both applications — Audio falls back to 48 (music-leaning, which
        // is what these clips settle on anyway) and Voip falls back to 115
        // (speech-leaning, which is what voip content wants from frame 0).
        if analysis_info.valid {
            self.analysis_frames = self.analysis_frames.saturating_add(1);
        }
        let analysis_converged = self.analysis_frames >= self.analysis_warmup;

        self.detected_bandwidth = 0;
        if analysis_info.valid && analysis_converged {
            // Auto path (signal_type override applies later in compute_voice_est):
            // pick the hysteresis-correct probability.
            let prob = if self.prev_enc_mode.is_none() {
                analysis_info.music_prob
            } else if self.prev_enc_mode == Some(OpusMode::CeltOnly) {
                analysis_info.music_prob_max
            } else {
                analysis_info.music_prob_min
            };
            self.voice_ratio = (0.5 + 100.0 * (1.0 - prob)).floor() as i32;
            let ab = analysis_info.bandwidth;
            self.detected_bandwidth = if ab <= 12 {
                Bandwidth::Narrowband as i32
            } else if ab <= 14 {
                Bandwidth::Mediumband as i32
            } else if ab <= 16 {
                Bandwidth::Wideband as i32
            } else if ab <= 18 {
                Bandwidth::Superwideband as i32
            } else {
                Bandwidth::Fullband as i32
            };
        }

        // Mode selection: match C's opus_encode_native() behavior.
        // C reference auto-selects between SILK_ONLY and CELT_ONLY; Hybrid is
        // produced afterwards by bandwidth overrides (SILK-only + FB/SWB → Hybrid).
        let mut mode = if self.application == Application::RestrictedLowDelay {
            OpusMode::CeltOnly
        } else {
            let equiv = compute_equiv_rate(
                self.bitrate_bps,
                self.channels,
                frame_rate,
                !self.use_cbr,
                self.complexity,
                self.packet_loss_perc,
            );
            let prev_was_celt = self.prev_enc_mode == Some(OpusMode::CeltOnly);
            let has_prev_mode = self.prev_enc_mode.is_some();
            let voice_est = self.compute_voice_est();
            let threshold = compute_mode_threshold(
                self.application,
                self.channels,
                prev_was_celt,
                has_prev_mode,
                voice_est,
            );
            if equiv >= threshold && self.sampling_rate >= 24000 {
                OpusMode::CeltOnly
            } else {
                OpusMode::SilkOnly
            }
        };
        // SILK and hybrid have no frame shorter than 10 ms: a 2.5/5 ms request is
        // CELT-only (opus_encoder.c:1533). Without this, the voip/audio selector
        // could pick SILK for a 2.5/5 ms frame, and gen_toc then wrote a TOC
        // claiming 10/20 ms over 2.5/5 ms of audio -- every decoder output 4x
        // the samples and libopus failed the range check on frame 1.
        if mode != OpusMode::CeltOnly && frame_rate > 100 {
            mode = OpusMode::CeltOnly;
        }

        // ---- Automatic rate-dependent bandwidth selection (opus_encoder.c:1456) ----
        // Walk down from FB; stop at the first bandwidth whose hysteresis-adjusted
        // threshold the equivalent rate meets. Thresholds interpolate voice<->music
        // by voice_est^2. Without the tonality analysis we cannot do
        // detected-bandwidth reduction, so this reproduces libopus's
        // complexity-0 choices (measured: WB @16k, SWB @20k, FB @24k+ voip mono).
        {
            let equiv = compute_equiv_rate(
                self.bitrate_bps,
                self.channels,
                frame_rate,
                !self.use_cbr,
                self.complexity,
                self.packet_loss_perc,
            );
            let voice_est: i32 = self.compute_voice_est();
            let (vt, mt) = if self.channels == 2 {
                (
                    &STEREO_VOICE_BANDWIDTH_THRESHOLDS,
                    &STEREO_MUSIC_BANDWIDTH_THRESHOLDS,
                )
            } else {
                (
                    &MONO_VOICE_BANDWIDTH_THRESHOLDS,
                    &MONO_MUSIC_BANDWIDTH_THRESHOLDS,
                )
            };
            let mut th = [0i32; 8];
            for i in 0..8 {
                // libopus' formula verbatim (voice_est squared, scaled by >> 14).
                #[allow(clippy::suspicious_operation_groupings)]
                {
                    th[i] = mt[i] + ((voice_est * voice_est * (vt[i] - mt[i])) >> 14);
                }
            }
            const NB: i32 = Bandwidth::Narrowband as i32; // 1101
            const MB: i32 = Bandwidth::Mediumband as i32; // 1102
            const FB: i32 = Bandwidth::Fullband as i32; // 1105
            let mut bw = FB;
            while bw > NB {
                let idx = (2 * (bw - MB)) as usize;
                let mut threshold = th[idx];
                let hysteresis = th[idx + 1];
                if !self.first_frame {
                    if self.auto_bandwidth >= bw {
                        threshold -= hysteresis;
                    } else {
                        threshold += hysteresis;
                    }
                }
                if equiv >= threshold {
                    break;
                }
                bw -= 1;
            }
            // Mediumband is no longer used by libopus's selector.
            if bw == MB {
                bw = Bandwidth::Wideband as i32;
            }
            self.auto_bandwidth = bw;
            // OPUS_SET_MAX_BANDWIDTH, then OPUS_SET_BANDWIDTH (opus_encoder.c:1629-1633).
            // The user's forced bandwidth is applied HERE, BEFORE the safety and
            // Nyquist caps below, exactly as libopus orders it. It used to replace
            // the result after every cap, which let a forced SWB/FB code hybrid
            // from 16 kHz input, a forced FB label 8 kHz packets, and a forced MB
            // reach CELT (which has no MB config) -- all streams libopus's
            // decoder rejects with a range-coder mismatch.
            bw = bw.min(self.max_bandwidth as i32);
            if let Some(f) = self.force_bandwidth {
                bw = f as i32;
            }
            // Hybrid at unsafe CBR rates starves SILK: cap at WB below 15 kb/s.
            if mode != OpusMode::CeltOnly && self.use_cbr && self.bitrate_bps < 15000 {
                bw = bw.min(Bandwidth::Wideband as i32);
            }
            // (A WB floor for SILK from >16 kHz input used to sit here: the
            // 48/24 -> 8/12 kHz encode resamplers did not exist. They do now
            // -- SilkDownFirResampler covers every libopus ratio -- so NB/MB
            // SILK is codeable from every API rate, as in libopus.)
            // Never code above the input's Nyquist (opus_encoder.c:1516).
            if self.sampling_rate <= 24000 {
                bw = bw.min(Bandwidth::Superwideband as i32);
            }
            if self.sampling_rate <= 16000 {
                bw = bw.min(Bandwidth::Wideband as i32);
            }
            if self.sampling_rate <= 12000 {
                bw = bw.min(Bandwidth::Mediumband as i32);
            }
            if self.sampling_rate <= 8000 {
                bw = bw.min(Bandwidth::Narrowband as i32);
            }
            // (An MB -> WB remap above 12 kHz used to sit here for the same
            // missing-resampler reason; a user/max MB is honoured as in libopus.
            // The AUTO walk still never yields MB -- that remap is above.)
            // Use the detected bandwidth to reduce the coded bandwidth
            // (opus_encoder.c:1526), conservatively floored by rate. (For
            // CELT-only this is currently undone below — no end-band support.)
            // For CELT-only, hold the detected-bandwidth narrowing until the
            // leak_boost dynalloc lands: decisions already match libopus
            // frame-for-frame (64k st music: 27:704/31:680/23:90 both), but our
            // dynalloc lacks C's leakage compensation at the spectral cut, so
            // the same narrowing costs 0.25 ODG more than C pays (PEAQ-gated
            // out). Hybrid/SILK caps (incl. hybrid SWB) stay live.
            // CELT-only keeps FULL bandwidth by choice: C's detected-bandwidth
            // narrowing costs PEAQ universally (libopus's own -2.11 at 64k st
            // IS its narrowed score; our FB encode scores -1.65 on the same
            // clip). leak_boost did NOT change this verdict (tested 2026-07-09
            // with the full dynalloc live: narrowing still -2.37). Hybrid/SILK
            // caps stay (they pick coding MODE, not spectral truncation).
            if self.detected_bandwidth != 0
                && self.force_bandwidth.is_none()
                && mode != OpusMode::CeltOnly
            {
                let ch = self.channels as i32;
                let equiv2 = equiv; // same 20-ms equivalent rate as the walk
                let min_det = if equiv2 <= 18000 * ch && mode == OpusMode::CeltOnly {
                    NB
                } else if equiv2 <= 24000 * ch && mode == OpusMode::CeltOnly {
                    MB
                } else if equiv2 <= 30000 * ch {
                    Bandwidth::Wideband as i32
                } else if equiv2 <= 44000 * ch {
                    Bandwidth::Superwideband as i32
                } else {
                    FB
                };
                bw = bw.min(self.detected_bandwidth.max(min_det));
            }
            // (max/forced bandwidth were applied above, before the caps.)
            // CELT has no mediumband config: libopus uses WIDEBAND instead
            // (opus_encoder.c:1680, "CELT mode doesn't support mediumband").
            if mode == OpusMode::CeltOnly && bw == MB {
                bw = Bandwidth::Wideband as i32;
            }
            self.bandwidth = match bw {
                x if x == NB => Bandwidth::Narrowband,
                x if x == MB => Bandwidth::Mediumband,
                x if x == Bandwidth::Wideband as i32 => Bandwidth::Wideband,
                x if x == Bandwidth::Superwideband as i32 => Bandwidth::Superwideband,
                x if x == FB => Bandwidth::Fullband,
                _ => Bandwidth::Wideband,
            };
            self.first_frame = false;
        }

        let curr_bw = self.bandwidth;
        if mode == OpusMode::SilkOnly
            && (curr_bw == Bandwidth::Superwideband || curr_bw == Bandwidth::Fullband)
        {
            mode = OpusMode::Hybrid;
        }
        if mode == OpusMode::Hybrid
            && (curr_bw == Bandwidth::Narrowband
                || curr_bw == Bandwidth::Mediumband
                || curr_bw == Bandwidth::Wideband)
        {
            mode = OpusMode::SilkOnly;
        }

        // Stereo hybrid is now CONFORMANT (the CELT intensity-clamp fix), but
        // our FIXED-point stereo SILK executes it worse than plain CELT-FB above
        // ~28 kb/s: PEAQ on stereo speech (ODG) measured hybrid −2.196/−2.193 vs
        // CELT-FB −2.136/−2.057 at 32k/48k (CELT-FB wins), while at 24k hybrid
        // −2.198 beats CELT-FB −2.240. libopus's FLOAT stereo SILK hybrid beats
        // both everywhere — the gap is fixed-vs-float, not a bug. So route
        // stereo hybrid to CELT-FB except at the low rates where it wins. (Force
        // via OPUS_SET_BANDWIDTH if the true hybrid path is wanted.) The clean
        // fix is float stereo SILK — a large port, tracked in the roadmap.
        if self.channels == 2 && mode == OpusMode::Hybrid && self.bitrate_bps > 28000 {
            mode = OpusMode::CeltOnly;
            self.bandwidth = Bandwidth::Fullband;
        }

        // ---- Mode-dwell hysteresis (Great Gate P2) — MEASURED INEFFECTIVE ----
        // Require a proposed mode change to persist for `mode_dwell` frames
        // before committing. `mode_dwell <= 1` is OFF and byte-identical.
        //
        // REFUTED for the defect it was built for (2026-08-07), kept behind the
        // env toggle so re-testing is cheap if the mode pattern ever changes.
        // The non-CELT frames it was meant to suppress are NOT isolated flips:
        // they are a single contiguous run at the START of the stream (frames
        // 0-23 on every clip measured), while the analysis classifier warms up.
        // Dwell delays transitions in BOTH directions, so on one long run it
        // only postpones the exit — measured non-CELT frames went UP with
        // dwell, 24 -> 25/26/28/33 for dwell 2/3/5/10, i.e. exactly +(N-1).
        // The fix that works is `analysis_warmup` below.
        if self.mode_dwell > 1 {
            match self.prev_enc_mode {
                Some(prev) if mode != prev => {
                    self.mode_dwell_run += 1;
                    if self.mode_dwell_run < self.mode_dwell {
                        // Not yet persistent: hold the previous mode. Bandwidth
                        // was chosen for the proposed mode, so reconcile it or
                        // the TOC config would be invalid.
                        mode = prev;
                        self.bandwidth = reconcile_bandwidth(mode, self.bandwidth);
                    } else {
                        // Persisted long enough — commit and re-arm.
                        self.mode_dwell_run = 0;
                    }
                }
                _ => self.mode_dwell_run = 0,
            }
        }

        // Great Gate truth-table lever: pin the mode after the auto decision,
        // reconciling bandwidth to a valid TOC config for the forced mode.
        // Unset = byte-identical to the auto path above.
        if let Some(fm) = self.force_mode {
            mode = fm;
            self.bandwidth = reconcile_bandwidth(fm, self.bandwidth);
        }

        // A sub-frame of a multi-frame packet codes with the packet's decision.
        if let Some((m, bw)) = self.mf_lock {
            mode = m;
            self.bandwidth = bw;
        }

        // ---- Transition redundancy (opus_encoder.c:1541) ----
        // CELT->SILK/hybrid: this frame carries a 5 ms CELT frame continuing the
        // old CELT state (the decoder fades it into the new mode). SILK/hybrid
        // ->CELT: stay ONE more frame in the old mode and end it with a 5 ms CELT
        // frame from a fresh CELT state that the following CELT frames continue
        // ("to_celt"); below 10 ms there is no room, so switch directly.
        let mut redundancy = false;
        let mut celt_to_silk = false;
        let mut to_celt = false;
        if let Some(prev) = self.prev_enc_mode {
            if mode != OpusMode::CeltOnly && prev == OpusMode::CeltOnly {
                redundancy = true;
                celt_to_silk = true;
            } else if self.mf_lock.is_none()
                && mode == OpusMode::CeltOnly
                && prev != OpusMode::CeltOnly
                && frame_size >= fs / 100
            {
                mode = prev;
                to_celt = true;
                redundancy = true;
                // The bandwidth was picked for CELT; SILK/hybrid follow it.
                let bw = self.bandwidth;
                if mode == OpusMode::SilkOnly
                    && matches!(bw, Bandwidth::Superwideband | Bandwidth::Fullband)
                {
                    mode = OpusMode::Hybrid;
                } else if mode == OpusMode::Hybrid
                    && !matches!(bw, Bandwidth::Superwideband | Bandwidth::Fullband)
                {
                    mode = OpusMode::SilkOnly;
                }
            }
        }
        if self.mf_lock.is_some() && self.mf_to_celt {
            // Last sub-frame of a multi-frame to_celt packet.
            redundancy = true;
            celt_to_silk = false;
            to_celt = true;
        }

        // opus_encode_native: ">60 ms frames, and >20 ms when in Hybrid or
        // CELT-only modes" are coded as several frames in one packet.
        if self.mf_lock.is_none()
            && ((frame_size > fs / 50 && mode != OpusMode::SilkOnly) || frame_size > 3 * fs / 50)
        {
            return self.encode_multiframe(input, frame_size, output, mode, to_celt);
        }

        if mode == OpusMode::CeltOnly {
            match frame_rate {
                400 | 200 | 100 | 50 => {}
                _ => return Err(Error::BadArg("Unsupported frame size for CELT-only mode")),
            }
        }

        if mode == OpusMode::Hybrid {
            match frame_rate {
                100 | 50 => {}
                _ => return Err(Error::BadArg("Unsupported frame size for Hybrid mode")),
            }
        }

        if mode == OpusMode::SilkOnly {
            // 10/20/40/60 ms (60 ms: frame_rate = Fs/frame_size = 16). Below
            // 10 ms the selector already switched to CELT.
            match frame_rate {
                100 | 50 | 25 | 16 => {}
                _ => return Err(Error::BadArg("Unsupported frame size for SILK-only mode")),
            }
        }

        let n400 = (self.sampling_rate / 400) as usize;

        // The CELT->SILK redundant frame continues the OLD CELT state, which
        // the reset below discards for hybrid: keep a copy for it.
        let mut red_celt = if redundancy && celt_to_silk {
            Some(self.celt_enc.clone())
        } else {
            None
        };

        // ---- Mode-transition resets (opus_encoder.c:1449 + 2054) ----
        // The decoder resets its CELT state on ANY mode change (when there is
        // no redundancy) and its SILK state when leaving CELT-only; the
        // encoder must mirror both or the streams desync from that frame on.
        // CELT_SET_PREDICTION(2) every CELT/hybrid frame, (0) right after a reset.
        self.celt_enc.prediction_off = false;
        if let Some(prev) = self.prev_enc_mode {
            if prev != mode {
                if mode != OpusMode::SilkOnly {
                    let ch = self.channels;
                    self.celt_enc = CeltEncoder::new(modes::default_mode(), ch);
                    self.celt_enc.upsample = (48000 / self.sampling_rate) as usize;
                    // Prefill 2.5 ms so the fresh state has real preemph/overlap
                    // history instead of a hard edge (opus_encoder.c:2060).
                    let n400 = (self.sampling_rate / 400) as usize;
                    if self.celt_prefill_tail.len() == n400 * ch {
                        let mut dummy = RangeCoder::new_encoder(2);
                        let tail = std::mem::take(&mut self.celt_prefill_tail);
                        self.celt_enc
                            .encode_with_budget(&tail, n400, &mut dummy, 0, 21, 16);
                        self.celt_prefill_tail = tail;
                    }
                    self.celt_enc.prediction_off = true;
                }
                if mode != OpusMode::CeltOnly && prev == OpusMode::CeltOnly {
                    self.silk_initialized = false;
                    self.silk_prefill_pending = true;
                }
            }
        }

        // SILK prefill tail: last 10 ms of API-rate mono input.
        if self.channels == 1 {
            let n10 = (self.sampling_rate / 100) as usize;
            if frame_size >= n10 {
                self.silk_prefill_tail.resize(n10, 0);
                for i in 0..n10 {
                    self.silk_prefill_tail[i] =
                        (input[frame_size - n10 + i] * 32768.0).clamp(-32768.0, 32767.0) as i16;
                }
            }
        }

        // Save THIS frame's last 2.5 ms (planar) for a possible prefill at the
        // next mode transition. (The transition block above consumed the
        // PREVIOUS frame's tail.)
        {
            let ch = self.channels;
            self.celt_prefill_tail.resize(n400 * ch, 0.0);
            let base = frame_size - n400;
            for c in 0..ch {
                for i in 0..n400 {
                    self.celt_prefill_tail[c * n400 + i] = input[(base + i) * ch + c];
                }
            }
        }

        let toc = gen_toc(mode, frame_rate, self.bandwidth, self.channels);
        output[0] = toc;

        // opus_encode_native: with fewer than 3 bytes there is no room for a
        // coded frame, so emit a TOC-only packet that the decoder conceals.
        // Reached directly with a 2-byte buffer, and by the sub-frames of a
        // multi-frame packet whose buffer is too small to share out.
        if output.len() < 3 {
            self.prev_enc_mode = Some(mode);
            self.range_final = 0;
            return Ok(1);
        }

        // ---- DTX decision (opus_encoder.c:2137 decide_dtx_mode) ----
        // After enough consecutive inactive frames, emit a TOC-only 1-byte
        // packet: the decoder sees an empty payload and runs comfort-noise /
        // PLC. We decide before the (skipped) SILK/CELT encode — SILK's own DTX
        // likewise stops coding, so the encoder state simply doesn't advance;
        // the codecs resync on the next active frame.
        if self.use_dtx && (analysis_info.valid || is_silence) {
            let frame_ms_q1 = 2 * 1000 * frame_size as i32 / self.sampling_rate;
            let dtx = if !activity {
                self.nb_no_activity_ms_q1 += frame_ms_q1;
                const LO: i32 = silk::define::NB_SPEECH_FRAMES_BEFORE_DTX * 20 * 2; // 400
                const HI: i32 = (silk::define::NB_SPEECH_FRAMES_BEFORE_DTX
                    + silk::define::MAX_CONSECUTIVE_DTX)
                    * 20
                    * 2; // 1200
                if self.nb_no_activity_ms_q1 > LO {
                    if self.nb_no_activity_ms_q1 <= HI {
                        true
                    } else {
                        self.nb_no_activity_ms_q1 = LO;
                        false
                    }
                } else {
                    false
                }
            } else {
                self.nb_no_activity_ms_q1 = 0;
                false
            };
            if dtx {
                self.prev_enc_mode = Some(mode);
                self.range_final = 0;
                return Ok(1);
            }
        } else {
            self.nb_no_activity_ms_q1 = 0;
        }

        let target_bits =
            (self.bitrate_bps as i64 * frame_size as i64 / self.sampling_rate as i64) as i32;
        let cbr_bytes = ((target_bits + 4) / 8) as usize;
        // opus_encode_native: no single Opus frame exceeds 1275 bytes (+1 TOC),
        // whatever the caller's buffer; CBR at a high rate must not plan more.
        let max_data_bytes = output.len().min(1276);

        // CBR: the packet is exactly the target size. VBR: start the coder on a
        // generous buffer — SILK-only packets end at whatever SILK produced, and
        // the CELT layer picks its own frame size (compute_vbr) and shrinks the
        // coder to it (libopus opus_encoder.c / celt_encoder.c VBR flow).
        let n_bytes = if self.use_cbr {
            cbr_bytes.min(max_data_bytes).max(1)
        } else {
            max_data_bytes
                .min(1276)
                .max(cbr_bytes.min(max_data_bytes))
                .max(3)
        };

        // Redundant-frame budget (opus_encoder.c compute_redundancy_bytes); too
        // few bytes to be worth it -> rely on the decoder's transition PLC.
        let mut redundancy_bytes = 0usize;
        if mode == OpusMode::CeltOnly {
            redundancy = false;
        }
        if redundancy {
            redundancy_bytes =
                compute_redundancy_bytes(n_bytes, self.bitrate_bps, frame_rate, self.channels);
            if redundancy_bytes == 0 {
                redundancy = false;
            }
        }

        let init_rc_size = n_bytes - 1;
        self.rc.reset_for_encode(init_rc_size as u32);

        if mode == OpusMode::SilkOnly || mode == OpusMode::Hybrid {
            // SILK's internal rate follows the coded BANDWIDTH (NB 8 / MB 12 /
            // WB 16 kHz; hybrid is WB SILK), capped by the API rate -- libopus
            // maxInternalSampleRate. It used to follow the API rate alone, so a
            // NB TOC from 12/16 kHz input carried SILK coded at 12/16 kHz and the
            // decoder (which takes the rate from the TOC) desynced on frame 1.
            let silk_fs_khz = if mode == OpusMode::Hybrid {
                16
            } else {
                // self.bandwidth, not curr_bw: it is what gen_toc wrote, after
                // every later reconciliation (forced mode, dwell).
                let bw_khz = match self.bandwidth {
                    Bandwidth::Narrowband => 8,
                    Bandwidth::Mediumband => 12,
                    _ => 16,
                };
                bw_khz.min(self.sampling_rate / 1000)
            };
            let silk_fs_hz = silk_fs_khz * 1000;

            let frame_ms = (frame_size as i32 * 1000) / self.sampling_rate;
            if !self.silk_initialized || self.silk_enc.s_cmn.fs_khz != silk_fs_khz {
                let silk_init_bitrate = if self.use_cbr {
                    (((n_bytes - 1) * 8) as i64 * self.sampling_rate as i64 / frame_size as i64)
                        as i32
                } else {
                    self.bitrate_bps
                };
                silk_control_encoder(
                    &mut self.silk_enc,
                    silk_fs_khz,
                    frame_ms,
                    silk_init_bitrate,
                    self.complexity,
                );
                self.silk_enc.s_cmn.use_cbr = i32::from(self.use_cbr);

                self.silk_enc.s_cmn.n_channels = self.channels as i32;
                self.silk_initialized = true;
                self.down2_state_first = [0; 2];
                self.down2_state_second = [0; 2];
                self.down2_3_state = [0; 6];
                self.down_1_3_state = silk::resampler::SilkResamplerDown1_3::default();
                self.down2_3_state_r = [0; 6];
                self.down_1_3_state_r = silk::resampler::SilkResamplerDown1_3::default();
                // API -> SILK-internal (None when the rates are equal: copy path).
                self.down_fir_l =
                    silk::resampler::SilkDownFirResampler::new(self.sampling_rate, silk_fs_hz);
                self.down_fir_r =
                    silk::resampler::SilkDownFirResampler::new(self.sampling_rate, silk_fs_hz);
            } else if self.silk_enc.s_cmn.packet_size_ms != frame_ms {
                // silk_control_encoder runs every packet in libopus and re-derives
                // nFramesPerPacket/nb_subfr on a PacketSize_ms change. Skipping it
                // left SILK at 20 ms after a hybrid multiframe run (20 ms subframes)
                // while the TOC said 60 ms: one coded frame per 60 ms packet.
                silk::control_codec::silk_setup_fs(&mut self.silk_enc, silk_fs_khz, frame_ms);
            }

            // SILK prefill after CELT-only (opus_encoder.c prefill=1): run 10 ms
            // of the previous audio through the fresh resampler + SILK warmup
            // path so the first coded SILK frame has real LTP/shape history.
            if self.silk_prefill_pending {
                self.silk_prefill_pending = false;
                let n10 = (self.sampling_rate / 100) as usize;
                if self.channels == 1 && self.silk_prefill_tail.len() == n10 {
                    let need = silk_fs_khz as usize * 10;
                    let mut resampled = vec![0i16; need];
                    if self.sampling_rate != silk_fs_hz {
                        if let Some(r) = &mut self.down_fir_l {
                            r.process(&mut resampled, &self.silk_prefill_tail);
                        }
                    } else {
                        resampled.copy_from_slice(&self.silk_prefill_tail[..need]);
                    }
                    silk::enc_api::silk_encode_prefill(&mut self.silk_enc, &resampled, 0);
                }
            }

            self.silk_enc.s_cmn.use_in_band_fec = i32::from(self.use_inband_fec);
            self.silk_enc.s_cmn.packet_loss_perc = self.packet_loss_perc.clamp(0, 100);

            let lbrr_in_previous_packet = self.silk_enc.s_cmn.lbrr_enabled != 0;
            self.silk_enc.s_cmn.lbrr_enabled = i32::from(self.use_inband_fec);

            // libopus silk_setup_LBRR: the first LBRR packet copies frames coded
            // at the full rate, so it takes the coarsest step (7); after that the
            // step shrinks as loss rises, max(7 - 0.4 loss%, 2). FEC-off path
            // unaffected.
            self.silk_enc.s_cmn.lbrr_gain_increases = if lbrr_in_previous_packet {
                (7 - ((self.packet_loss_perc.clamp(0, 100) * 26214) >> 16)).max(2)
            } else {
                7
            };

            let hp_freq_smth1 = if mode == OpusMode::CeltOnly {
                silk_lin2log(60) << 8
            } else {
                self.silk_enc.s_cmn.variable_hp_smth1_q15
            };

            const VARIABLE_HP_SMTH_COEF2_Q16: i32 = 984;
            self.variable_hp_smth2_q15 = silk_smlawb(
                self.variable_hp_smth2_q15,
                hp_freq_smth1 - self.variable_hp_smth2_q15,
                VARIABLE_HP_SMTH_COEF2_Q16,
            );

            let cutoff_hz = silk_log2lin(silk_rshift(self.variable_hp_smth2_q15, 8));

            let prof_rs = crate::prof::scope(crate::prof::Stage::Resample);
            let required_size = frame_size * self.channels;
            self.buf_filtered.resize(required_size, 0);
            if self.application == Application::Voip {
                hp_cutoff(
                    input,
                    cutoff_hz,
                    &mut self.buf_filtered,
                    &mut self.hp_mem,
                    frame_size,
                    self.channels,
                    self.sampling_rate,
                );
            } else {
                for (i, &x) in input.iter().enumerate() {
                    self.buf_filtered[i] = (x * 32768.0).clamp(-32768.0, 32767.0) as i16;
                }
            }

            let input_i16 = &self.buf_filtered;

            let silk_input: &[i16] = if self.channels == 2 {
                // Stereo SILK/hybrid: deinterleave, resample EACH channel to the
                // SILK-internal rate (separate filter states), then split
                // mid/side — C's order (per-channel resampling inside
                // silk_Encode, then silk_stereo_LR_to_MS). The old code only
                // handled stereo at <=16 kHz and fed resampled INTERLEAVED
                // audio to a stereo-configured SILK above that (never
                // exercised until the analysis started picking stereo hybrid).
                let frame_length = input_i16.len() / 2;
                self.buf_left.resize(frame_length, 0);
                self.buf_right.resize(frame_length, 0);
                for i in 0..frame_length {
                    self.buf_left[i] = input_i16[2 * i];
                    self.buf_right[i] = input_i16[2 * i + 1];
                }
                let need_resample = self.sampling_rate != silk_fs_hz;
                let ds_len =
                    (frame_length as i64 * silk_fs_hz as i64 / self.sampling_rate as i64) as usize;
                if need_resample {
                    self.buf_stereo_mid.resize(ds_len, 0);
                    self.buf_stereo_side.resize(ds_len, 0);
                    if let (Some(rl), Some(rr)) = (&mut self.down_fir_l, &mut self.down_fir_r) {
                        rl.process(&mut self.buf_stereo_mid, &self.buf_left);
                        rr.process(&mut self.buf_stereo_side, &self.buf_right);
                    }
                    self.buf_left.resize(ds_len, 0);
                    self.buf_right.resize(ds_len, 0);
                    self.buf_left
                        .copy_from_slice(&self.buf_stereo_mid[..ds_len]);
                    self.buf_right
                        .copy_from_slice(&self.buf_stereo_side[..ds_len]);
                }
                self.buf_stereo_mid.resize(ds_len, 0);
                self.buf_stereo_side.resize(ds_len, 0);
                for i in 0..ds_len {
                    let l = self.buf_left[i] as i32;
                    let r = self.buf_right[i] as i32;
                    self.buf_stereo_mid[i] = ((l + r) / 2) as i16;
                    self.buf_stereo_side[i] = (l - r) as i16;
                }
                self.silk_enc.stereo.side.resize(ds_len, 0);
                self.silk_enc
                    .stereo
                    .side
                    .copy_from_slice(&self.buf_stereo_side[..ds_len]);
                &self.buf_stereo_mid
            } else if self.sampling_rate != silk_fs_hz {
                // Mono SILK/hybrid: API -> SILK-internal through libopus's
                // silk_resampler down-FIR for this ratio (48k->16k is the same
                // direct FIR as before; the old down2 + down2_3 chain ALIASED --
                // a 1 kHz sine came out with a 7 kHz mirror).
                let silk_frame_size =
                    (frame_size as i64 * silk_fs_hz as i64 / self.sampling_rate as i64) as usize;
                self.buf_silk_input.resize(silk_frame_size, 0);
                if let Some(r) = &mut self.down_fir_l {
                    r.process(&mut self.buf_silk_input, input_i16);
                }
                &self.buf_silk_input
            } else {
                input_i16
            };

            drop(prof_rs);

            let mut pn_bytes = 0;

            // The frames-per-second math below divides by silk_input.len(), which is
            // at the SILK-INTERNAL rate — so the rate here must be internal too.
            // Using the API rate at 48 kHz told SILK to target 3x the real budget
            // with a hard max_bits cap -> the gain loop crushed every frame to fit
            // -> near-silent output (only worked at 16 kHz API where they coincide).
            let silk_rate_for_calc = silk_fs_hz;
            let silk_frame_len = silk_input.len();

            let silk_bitrate = if mode == OpusMode::Hybrid {
                let frame_duration_ms = frame_size as i32 * 1000 / self.sampling_rate;
                let frame20ms = frame_duration_ms >= 20;
                compute_silk_rate_for_hybrid(self.bitrate_bps, curr_bw, frame20ms, !self.use_cbr)
            } else if self.use_cbr {
                (8i64 * (n_bytes - 1 - redundancy_bytes) as i64 * silk_rate_for_calc as i64
                    / silk_frame_len as i64) as i32
            } else {
                // VBR: n_bytes is only the buffer cap; target the configured rate.
                self.bitrate_bps
            };
            // Max bits for SILK, counting ToC, redundancy bytes, and 1 bit for
            // the redundancy position + 20 for flag/size (hybrid only).
            let red_bits = if redundancy && redundancy_bytes >= 2 {
                (redundancy_bytes * 8 + 1) as i32 + if mode == OpusMode::Hybrid { 20 } else { 0 }
            } else {
                0
            };
            let silk_max_bits = if mode == OpusMode::Hybrid {
                let total_max_bits = ((n_bytes - 1) * 8) as i32 - red_bits;
                if self.use_cbr {
                    let silk_bits = (silk_bitrate as i64 * silk_frame_len as i64
                        / silk_rate_for_calc as i64) as i32;
                    let other_bits = 0i32.max(total_max_bits - silk_bits);
                    0i32.max(total_max_bits - other_bits * 3 / 4)
                } else {
                    let frame_duration_ms = frame_size as i32 * 1000 / self.sampling_rate;
                    let frame20ms = frame_duration_ms >= 20;
                    let max_bit_rate = compute_silk_rate_for_hybrid(
                        total_max_bits * self.sampling_rate / frame_size as i32,
                        curr_bw,
                        frame20ms,
                        !self.use_cbr,
                    );
                    max_bit_rate * frame_size as i32 / self.sampling_rate
                }
            } else {
                ((n_bytes - 1) * 8) as i32 - red_bits
            };
            let silk_use_cbr = if mode == OpusMode::Hybrid && self.use_cbr {
                0
            } else {
                i32::from(self.use_cbr)
            };
            let ret = silk_encode(
                &mut self.silk_enc,
                silk_input,
                silk_input.len(),
                &mut self.rc,
                &mut pn_bytes,
                silk_bitrate,
                silk_max_bits,
                silk_use_cbr,
                1,
            );
            if ret != 0 {
                return Err(Error::Internal("SILK encoding failed"));
            }
        }

        // Redundancy signalling (opus_encoder.c): only when >= 17 (+20 hybrid)
        // bits remain -- the decoder gates its read identically. Hybrid codes the
        // flag, position and size; SILK-only implies redundancy from the length
        // and codes only the position (celt_to_silk) bit.
        let hybrid = mode == OpusMode::Hybrid;
        if mode != OpusMode::CeltOnly
            && self.rc.tell() + 17 + if hybrid { 20 } else { 0 } <= ((n_bytes - 1) * 8) as i32
        {
            if hybrid {
                self.rc.encode_bit_logp(redundancy, 12);
            }
            if redundancy {
                self.rc.encode_bit_logp(celt_to_silk, 1);
                // Hybrid reserves the 8 size bits and a few CELT bits.
                let max_redundancy = if hybrid {
                    (n_bytes - 1) as i32 - ((self.rc.tell() + 8 + 3 + 7) >> 3)
                } else {
                    (n_bytes - 1) as i32 - ((self.rc.tell() + 7) >> 3)
                };
                // Not `clamp`: max_redundancy may be < 2, where clamp panics;
                // this order (cap, then floor at 2, then 257) matches libopus.
                #[allow(clippy::manual_clamp)]
                let capped = (redundancy_bytes as i32)
                    .min(max_redundancy)
                    .max(2)
                    .min(257) as usize;
                redundancy_bytes = capped;
                if hybrid {
                    self.rc.enc_uint((redundancy_bytes - 2) as u32, 256);
                }
            }
        } else {
            redundancy = false;
        }
        if !redundancy {
            redundancy_bytes = 0;
        }

        if hybrid {
            let nb_compr_bytes = (n_bytes - 1 - redundancy_bytes) as u32;
            self.rc.shrink(nb_compr_bytes);
        }

        let celt_end_band = match self.bandwidth {
            Bandwidth::Narrowband => 13,
            Bandwidth::Mediumband | Bandwidth::Wideband => 17,
            Bandwidth::Superwideband => 19,
            _ => 21,
        };
        let celt_analysis = celt::AnalysisInfo {
            valid: analysis_info.valid,
            tonality: analysis_info.tonality,
            tonality_slope: analysis_info.tonality_slope,
            noisiness: analysis_info.noisiness,
            activity: analysis_info.activity,
            music_prob: analysis_info.music_prob,
            music_prob_min: analysis_info.music_prob_min,
            music_prob_max: analysis_info.music_prob_max,
            bandwidth: analysis_info.bandwidth,
            activity_probability: analysis_info.activity_probability,
            max_pitch_ratio: analysis_info.max_pitch_ratio,
            leak_boost: analysis_info.leak_boost,
        };
        // Planar copy of `len` input samples from `start` (CELT takes planar).
        let api_ch = self.channels;
        let planar = |start: usize, len: usize| -> Vec<f32> {
            let ch = api_ch;
            let mut v = vec![0.0f32; len * ch];
            for c in 0..ch {
                for i in 0..len {
                    v[c * len + i] = input[(start + i) * ch + c];
                }
            }
            v
        };

        // 5 ms redundant frame for CELT->SILK: the OLD CELT state, start band 0,
        // CBR at the redundancy size; written after the main payload.
        let mut red_data: Vec<u8> = Vec::new();
        let mut redundant_rng = 0u32;
        if redundancy && celt_to_silk {
            if let Some(mut enc) = red_celt.take() {
                let n2 = (self.sampling_rate / 200) as usize;
                enc.analysis = celt_analysis;
                enc.vbr_rate = 0;
                let mut rrc = RangeCoder::new_encoder(redundancy_bytes as u32);
                enc.encode_with_budget(
                    &planar(0, n2),
                    n2,
                    &mut rrc,
                    0,
                    celt_end_band,
                    (redundancy_bytes * 8) as i32,
                );
                rrc.done();
                redundant_rng = rrc.rng;
                red_data = rrc.buf[..redundancy_bytes].to_vec();
            }
        }

        let silk_ret_bytes = if mode == OpusMode::SilkOnly {
            ((self.rc.tell() + 7) >> 3) as usize
        } else {
            0
        };

        if mode == OpusMode::CeltOnly || mode == OpusMode::Hybrid {
            self.celt_enc.analysis = celt_analysis;
            self.celt_enc.complexity = self.complexity;
            self.celt_enc.lsb_depth = self.lsb_depth;
            // Census 2026-08-07 fix: loss_rate was never assigned, so CELT's
            // prefilter loss ladder (celt.rs) and coarse-energy intra bias were
            // dead even with OPUS_SET_PACKET_LOSS_PERC set. Default 0 = no
            // change on the default path (libopus opus_encoder.c parity).
            self.celt_enc.loss_rate = self.packet_loss_perc;
            let start_band = if mode == OpusMode::Hybrid { 17 } else { 0 };
            // CELT end band from the coded bandwidth (mirrors the decoder's
            // celt_endband_for_bandwidth): NB->13, MB/WB->17, SWB->19, FB->21.
            let end_band = celt_end_band;
            // nb_compr_bytes: the redundant frame's bytes are not CELT's.
            let total_packet_bits = ((n_bytes - 1 - redundancy_bytes) * 8) as i32;
            // VBR: hand CELT the target in eighth-bits per frame; it picks the
            // frame's size (compute_vbr) and shrinks the range coder to it. The
            // hybrid target covers the whole packet (CELT adds back the SILK
            // bits via `target += tell`).
            self.celt_enc.vbr_rate = if self.use_cbr {
                0
            } else {
                let den = self.sampling_rate >> 3; // Fs >> BITRES
                ((self.bitrate_bps as i64 * frame_size as i64 + (den >> 1) as i64) / den as i64)
                    as i32
            };

            let celt_input: &[f32] = if self.channels == 1 {
                input
            } else {
                let n = frame_size * self.channels;
                self.buf_celt_input.resize(n, 0.0);
                for i in 0..frame_size {
                    for ch in 0..self.channels {
                        self.buf_celt_input[ch * frame_size + i] = input[i * self.channels + ch];
                    }
                }
                &self.buf_celt_input
            };

            if self.rc.tell() <= total_packet_bits {
                self.celt_enc.encode_with_budget(
                    celt_input,
                    frame_size,
                    &mut self.rc,
                    start_band,
                    end_band,
                    total_packet_bits,
                );
            }
        }

        self.rc.done();

        // 5 ms redundant frame for SILK->CELT: a FRESH CELT state (reset, start
        // band 0, prediction off, CBR), prefilled with the 2.5 ms before it, codes
        // the frame's last 5 ms. That state becomes the encoder's, so the next
        // (CELT) frame continues from it -- as the decoder's does.
        if redundancy && !celt_to_silk {
            let n2 = (self.sampling_rate / 200) as usize;
            let n4 = (self.sampling_rate / 400) as usize;
            let mut enc = CeltEncoder::new(modes::default_mode(), self.channels);
            enc.upsample = (48000 / self.sampling_rate) as usize;
            enc.complexity = self.complexity;
            enc.lsb_depth = self.lsb_depth;
            enc.loss_rate = self.packet_loss_perc;
            enc.analysis = celt_analysis;
            enc.prediction_off = true;
            enc.vbr_rate = 0;
            let mut dummy = RangeCoder::new_encoder(2);
            enc.encode_with_budget(
                &planar(frame_size - n2 - n4, n4),
                n4,
                &mut dummy,
                0,
                celt_end_band,
                16,
            );
            let mut rrc = RangeCoder::new_encoder(redundancy_bytes as u32);
            enc.encode_with_budget(
                &planar(frame_size - n2, n2),
                n2,
                &mut rrc,
                0,
                celt_end_band,
                (redundancy_bytes * 8) as i32,
            );
            rrc.done();
            redundant_rng = rrc.rng;
            red_data = rrc.buf[..redundancy_bytes].to_vec();
            enc.prediction_off = false;
            self.celt_enc = enc;
        }
        self.range_final = self.rc.rng ^ redundant_rng;
        // libopus prev_mode: CELT after a to_celt frame (the redundant frame
        // primed CELT; the next CELT frame must not reset it). Set on EVERY
        // path -- the VBR SILK-only return used to skip it, so after one CELT
        // frame every SILK frame re-ran the CELT->SILK reset + prefill.
        let next_prev_mode = if to_celt { OpusMode::CeltOnly } else { mode };
        self.prev_enc_mode = Some(next_prev_mode);

        if mode == OpusMode::SilkOnly {
            let mut ret = silk_ret_bytes.min(self.rc.storage as usize);
            // Trailing zeros may be stripped (the decoder pads them) -- but not
            // with redundancy, whose position is inferred from the length.
            while !redundancy && ret > 2 && self.rc.buf[ret - 1] == 0 {
                ret -= 1;
            }
            // Payload = SILK bytes ++ redundant CELT frame, copied straight into
            // `output` (was: chained-iterator collect into a Vec, then copied).
            let (main, red) = (&self.rc.buf[..ret], &red_data[..]);
            let silk_len = ret + red.len();
            let put = |dst: &mut [u8], n: usize| {
                let a = n.min(main.len());
                dst[..a].copy_from_slice(&main[..a]);
                dst[a..n].copy_from_slice(&red[..n - a]);
            };

            let target_total = if self.use_cbr {
                n_bytes.min(output.len())
            } else {
                (silk_len + 1).min(output.len())
            };

            if !self.use_cbr || silk_len + 1 >= target_total {
                // VBR or payload fills the target: simple code 0 packet
                output[0] = toc;
                let copy_len = silk_len.min(target_total - 1);
                put(&mut output[1..], copy_len);
                return Ok((copy_len + 1).min(output.len()));
            }

            output[0] = toc | 0x03;

            if silk_len + 2 >= target_total {
                output[1] = 0x01;
                let copy_len = (target_total - 2).min(silk_len);
                put(&mut output[2..], copy_len);
                return Ok(target_total.min(output.len()));
            }

            let pad_amount = target_total - silk_len - 2;
            output[1] = 0x41;

            let nb_255s = (pad_amount - 1) / 255;
            let mut ptr = 2;
            for _ in 0..nb_255s {
                output[ptr] = 255;
                ptr += 1;
            }
            output[ptr] = (pad_amount - 255 * nb_255s - 1) as u8;
            ptr += 1;

            put(&mut output[ptr..], silk_len);
            ptr += silk_len;

            let fill_end = target_total.min(output.len());
            for byte in &mut output[ptr..fill_end] {
                *byte = 0;
            }

            return Ok(target_total.min(output.len()));
        }

        // CBR: fixed payload. VBR (CELT/hybrid): the CELT layer shrank the coder
        // to this frame's chosen size — emit exactly that many payload bytes.
        // The redundant frame (if any) follows the main payload.
        let nb_compr_bytes = n_bytes - 1 - redundancy_bytes;
        let main_len = if self.use_cbr {
            nb_compr_bytes
        } else {
            (self.rc.storage as usize).min(nb_compr_bytes)
        };
        output[1..1 + main_len].copy_from_slice(&self.rc.buf[..main_len]);
        output[1 + main_len..1 + main_len + red_data.len()].copy_from_slice(&red_data);
        let payload_len = main_len + red_data.len();
        // Great Gate harvest tap (observe-only; see the field doc). Signals are
        // recomputed read-only here — the decision code above is untouched.
        if self.gate_tap.is_some() {
            let equiv = compute_equiv_rate(
                self.bitrate_bps,
                self.channels,
                frame_rate,
                !self.use_cbr,
                self.complexity,
                self.packet_loss_perc,
            );
            let voice_est = self.compute_voice_est();
            let (clip, frame) = (self.gate_clip.clone(), self.gate_frame);
            if let Some(tap) = self.gate_tap.as_mut() {
                use std::io::Write as _;
                let mode_s = match mode {
                    OpusMode::SilkOnly => "silk",
                    OpusMode::CeltOnly => "celt",
                    OpusMode::Hybrid => "hybrid",
                };
                let _ = writeln!(
                    tap,
                    "{},{},{},{},{},{},{},{},{},{},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{},{:.4},{}",
                    clip,
                    frame,
                    mode_s,
                    self.bandwidth as i32,
                    self.channels,
                    self.bitrate_bps,
                    self.complexity,
                    equiv,
                    voice_est,
                    is_silence as u8,
                    activity as u8,
                    analysis_info.valid as u8,
                    analysis_info.tonality,
                    analysis_info.tonality_slope,
                    analysis_info.noisiness,
                    analysis_info.activity_probability,
                    analysis_info.music_prob,
                    analysis_info.music_prob_min,
                    analysis_info.music_prob_max,
                    self.detected_bandwidth,
                    analysis_info.max_pitch_ratio,
                    1 + payload_len,
                );
            }
        }
        self.gate_frame += 1;

        Ok(1 + payload_len)
    }
}

/// An Opus decoder for one mono or stereo stream.
///
/// Decodes packets from any Opus encoder at any of the five API rates and
/// either channel count (a stereo stream can be decoded to mono and vice
/// versa). Pass an empty packet to conceal a lost one. After the first few
/// frames, decoding performs no heap allocation.
pub struct OpusDecoder {
    /// Range-decoder payload buffer, reused across frames (was a fresh copy
    /// of every payload).
    rc_scratch: Vec<u8>,
    /// Payload buffer for the redundant CELT frame's range decoder (at most
    /// 257 bytes), separate because `rc_scratch` is in use when it is decoded.
    red_rc_scratch: Vec<u8>,
    celt_dec: CeltDecoder,
    silk_dec: silk::dec_api::SilkDecoder,
    sampling_rate: i32,
    channels: usize,

    prev_mode: Option<OpusMode>,
    frame_size: usize,
    /// libopus st->frame_size: the last packet's PER-FRAME size (caps PLC chunks).
    last_frame_size: usize,

    bandwidth: Bandwidth,

    stream_channels: usize,

    silk_resampler: silk::resampler::SilkResampler,
    // Second resampler for the SILK stereo right channel (L uses silk_resampler).
    silk_resampler_r: silk::resampler::SilkResampler,

    prev_internal_rate: i32,

    w_pcm_i16: Vec<i16>,
    w_silk_out: Vec<f32>,
    w_pcm_resampled: Vec<i16>,
    w_celt_planar: Vec<f32>,
    w_celt_out: Vec<f32>,

    // SILK per-frame history: libopus prepends the previous frame's last two
    // decoded samples (`sStereo.sMid`) and feeds the resampler from offset 1, a
    // 1-internal-sample delay line. Replicated here so our SILK output aligns
    // with the reference across every bandwidth (was leading by 1 internal
    // sample = 3/4/6 output samples at WB/MB/NB).
    silk_s_mid: [i16; 2],

    /// Final range-decoder state of the last decoded packet (libopus
    /// `OPUS_GET_FINAL_RANGE`): equal to the encoder's for a correctly
    /// transmitted packet, so it detects corruption and desynchronisation.
    pub last_range: u32,

    // Auxiliary decoder for packets whose channel count differs from ours
    // (a stream may switch between mono and stereo). It decodes at the packet's
    // native channel count; we then up/downmix to our output count. Persistent
    // so the "other" channel mode keeps its own inter-frame state.
    aux: Option<Box<Self>>,
    // Set when a packet was just decoded by the aux (a mono packet in a stereo
    // stream); triggers seeding the primary CELT decoder's overlap/energy state
    // from the aux at the next primary (stereo) CELT/Hybrid packet, so the MDCT
    // overlap-add is continuous across the mono->stereo switch.
    prev_used_aux: bool,
    // libopus st->prev_redundancy: the previous frame carried a SILK->CELT
    // redundant frame (redundancy && !celt_to_silk). Suppresses the CELT reset on
    // the following mode change (the redundant frame already primed CELT state).
    prev_redundancy: bool,
    /// Redundancy flag of the current packet's FIRST frame (libopus cancels a
    /// CELT->SILK/hybrid transition fade when the frame carries redundancy).
    first_frame_redundancy: bool,
    /// CELT->SILK/hybrid switch: samples of CELT concealment still owed for the
    /// transition fade. Deferred into the SILK/hybrid arm because libopus only
    /// conceals once it knows the frame has NO redundancy (`if (redundancy)
    /// transition = 0`) -- concealing first advanced the CELT state the
    /// redundant frame continues from.
    transition_pending: usize,
    transition_pcm: Option<Vec<f32>>,
}

impl OpusDecoder {
    ///
    /// # Errors
    ///
    /// [`Error::BadArg`] if `sampling_rate` is not 8000, 12000, 16000, 24000 or
    /// 48000 Hz or `channels` is not 1 or 2.
    pub fn new(sampling_rate: i32, channels: usize) -> Result<Self, Error> {
        if ![8000, 12000, 16000, 24000, 48000].contains(&sampling_rate) {
            return Err(Error::BadArg("Invalid sampling rate"));
        }
        if ![1, 2].contains(&channels) {
            return Err(Error::BadArg("Invalid number of channels"));
        }

        let mode = modes::default_mode();
        let mut celt_dec = CeltDecoder::new(mode, channels);
        // CELT always decodes the 48 kHz frame (libopus resampling_factor).
        celt_dec.downsample = (48000 / sampling_rate) as usize;

        let mut silk_dec = silk::dec_api::SilkDecoder::new();
        silk_dec.init(sampling_rate.min(16000), channels as i32);
        silk_dec.channel_state[0].fs_api_hz = sampling_rate;

        Ok(Self {
            rc_scratch: Vec::with_capacity(1275),
            red_rc_scratch: Vec::with_capacity(257),
            celt_dec,
            silk_dec,
            sampling_rate,
            channels,
            prev_mode: None,
            frame_size: 0,
            last_frame_size: 0,
            bandwidth: Bandwidth::Auto,
            stream_channels: channels,
            silk_resampler: silk::resampler::SilkResampler::default(),
            silk_resampler_r: silk::resampler::SilkResampler::default(),
            prev_internal_rate: 0,

            // SILK internal scratch: max frame is 60 ms at the 16 kHz WB internal
            // rate (960 samples/ch), i.e. 1920 stereo. Sized like the sibling
            // buffers below for headroom — the old fixed 640 overflowed on any
            // 60 ms SILK frame (panic decoding valid streams).
            w_pcm_i16: vec![0i16; 5760 * channels],

            w_silk_out: vec![0.0f32; 5760 * channels],
            w_pcm_resampled: vec![0i16; 5760 * channels],
            w_celt_planar: vec![0.0f32; 5760 * channels],
            w_celt_out: vec![0.0f32; 5760 * channels],
            silk_s_mid: [0; 2],
            last_range: 0,
            aux: None,
            prev_used_aux: false,
            prev_redundancy: false,
            first_frame_redundancy: false,
            transition_pending: 0,
            transition_pcm: None,
        })
    }

    /// Packet-loss concealment for a lost frame (empty/None packet). Runs the
    /// SILK PLC (LTP+LPC extrapolation) for the last-known SILK/hybrid mode and
    /// resamples to the output rate. CELT-only loss has no CELT PLC yet, so it
    /// yields silence (a documented Tier-1 follow-up); the SILK path covers the
    /// dominant VoIP case. Mono conceal is duplicated to both channels on a
    /// stereo output.
    fn decode_plc(&mut self, frame_size: usize, output: &mut [f32]) -> Result<usize, Error> {
        if output.len() < frame_size * self.channels {
            return Err(Error::BufferTooSmall("Output buffer too small"));
        }
        // opus_decode_native(data==NULL) + opus_decode_frame: conceal in chunks
        // of at most the last packet's frame size and 20 ms, snapping shorter
        // requests to 10 ms (or 5 ms outside SILK). CELT never conceals more
        // than 20 ms per call -- which is what lets its history buffer be
        // libopus's 2048 samples, and the PLC pitch search see the same window.
        let fs = self.sampling_rate as usize;
        let (f20, f10, f5) = (fs / 50, fs / 100, fs / 200);
        let mode = if self.prev_redundancy {
            OpusMode::CeltOnly
        } else {
            self.prev_mode.unwrap_or(OpusMode::SilkOnly)
        };
        let cap = if self.last_frame_size > 0 {
            self.last_frame_size
        } else {
            frame_size
        };
        let ch = self.channels;
        let mut done = 0;
        while done < frame_size {
            let mut n = (frame_size - done).min(cap);
            if n > f20 {
                n = f20;
            } else if n < f20 {
                if n > f10 {
                    n = f10;
                } else if mode != OpusMode::SilkOnly && n > f5 && n < f10 {
                    n = f5;
                }
            }
            self.decode_plc_frame(n, &mut output[done * ch..])?;
            done += n;
        }
        Ok(frame_size)
    }

    /// One opus_decode_frame(data==NULL): conceal `frame_size` (<= 20 ms) in the
    /// last mode -- CELT if the last frame ended in SILK->CELT redundancy. SILK
    /// conceals at least 10 ms (keeping the head); a hybrid frame adds the CELT
    /// high band (start band 17 -> noise PLC) on top of the SILK concealment.
    fn decode_plc_frame(&mut self, frame_size: usize, output: &mut [f32]) -> Result<usize, Error> {
        let ch = self.channels;
        let out_samples = frame_size * ch;
        for v in output.iter_mut().take(out_samples) {
            *v = 0.0;
        }
        let Some(prev) = self.prev_mode else {
            // No packet yet: all we can do is return zeros.
            return Ok(frame_size);
        };
        let mode = if self.prev_redundancy {
            OpusMode::CeltOnly
        } else {
            prev
        };
        if mode != OpusMode::CeltOnly {
            let f10 = (self.sampling_rate / 100) as usize;
            if frame_size < f10 {
                let mut tmp = vec![0.0f32; f10 * ch];
                self.decode_plc_silk(f10, &mut tmp, mode)?;
                output[..out_samples].copy_from_slice(&tmp[..out_samples]);
            } else {
                self.decode_plc_silk(frame_size, output, mode)?;
            }
        }
        if mode != OpusMode::SilkOnly {
            let celt_n = frame_size.min((self.sampling_rate / 50) as usize);
            if mode == OpusMode::Hybrid {
                // celt_accum: the high band adds onto the SILK concealment.
                let mut tmp = vec![0.0f32; celt_n * ch];
                self.celt_dec.plc_start = 17;
                self.celt_dec.conceal_lost(celt_n, &mut tmp);
                self.celt_dec.plc_start = 0;
                for (o, t) in output[..celt_n * ch].iter_mut().zip(&tmp) {
                    *o += *t;
                }
            } else {
                self.celt_dec.conceal_lost(celt_n, output);
            }
        }
        self.prev_mode = Some(mode);
        self.prev_redundancy = false;
        Ok(frame_size)
    }

    /// The SILK half of decode_plc_frame (`frame_size` >= 10 ms).
    fn decode_plc_silk(
        &mut self,
        frame_size: usize,
        output: &mut [f32],
        mode: OpusMode,
    ) -> Result<(), Error> {
        let frame_ms = (frame_size as i32 * 1000 / self.sampling_rate).max(1);
        let internal_rate = if mode == OpusMode::Hybrid {
            16000
        } else {
            match self.bandwidth {
                Bandwidth::Narrowband => 8000,
                Bandwidth::Mediumband => 12000,
                _ => 16000,
            }
        };
        if internal_rate != self.prev_internal_rate {
            self.silk_resampler.init(internal_rate, self.sampling_rate);
            self.prev_internal_rate = internal_rate;
        }
        let n_silk = match frame_ms {
            40 => 2,
            60 => 3,
            _ => 1,
        };
        let internal_frame = (frame_ms * internal_rate / 1000) as usize;
        let internal_sub = internal_frame / n_silk.max(1);
        let ratio = self.sampling_rate as f64 / internal_rate as f64;
        // Conceal with the previous frame's internal channel count: libopus runs
        // PLC on both SILK channels of a stereo stream and unmixes M/S -> L/R
        // with the previous predictor (dec_API.c). Concealing mid only and
        // duplicating it put every stereo mode transition off by ~3k LSB.
        let silk_lr = self.channels == 2 && self.silk_dec.n_channels_internal == 2;
        self.silk_dec.produce_lr = silk_lr;

        let mut off = 0usize; // output samples/ch written so far
        for sf in 0..n_silk {
            let mut rc = RangeCoder::new_decoder(&[]);
            let n16 = internal_sub;
            if n16 + 2 > self.w_pcm_i16.len() {
                return Err(Error::BufferTooSmall("opus PLC: frame exceeds buffer"));
            }
            self.w_pcm_i16[0] = self.silk_s_mid[0];
            self.w_pcm_i16[1] = self.silk_s_mid[1];
            let ret = self.silk_dec.decode(
                &mut rc,
                &mut self.w_pcm_i16[2..n16 + 2],
                silk::decode_frame::FLAG_PACKET_LOST,
                sf == 0,
                frame_ms,
                internal_rate,
            );
            if ret < 0 {
                return Err(Error::Internal("SILK PLC failed"));
            }
            let dec = ret as usize;
            if dec >= 2 {
                self.silk_s_mid[0] = self.w_pcm_i16[dec];
                self.silk_s_mid[1] = self.w_pcm_i16[dec + 1];
            }
            let base = off * self.channels;
            // Always through the resampler, even at equal rates: its Copy mode
            // carries libopus's delay_matrix_dec delay (see decode()).
            let out_len = (dec as f64 * ratio) as usize;
            if silk_lr {
                // L and R each through their own resampler, as the normal path.
                // Disjoint fields: resample straight from l_out/r_out (were
                // `.to_vec()` copies per lost frame).
                self.silk_resampler.process(
                    &mut self.w_pcm_resampled[..out_len],
                    &self.silk_dec.l_out[..dec],
                    dec as i32,
                );
                for i in 0..out_len {
                    let idx = base + i * 2;
                    if idx < output.len() {
                        output[idx] = self.w_pcm_resampled[i] as f32 / 32768.0;
                    }
                }
                self.silk_resampler_r.process(
                    &mut self.w_pcm_resampled[..out_len],
                    &self.silk_dec.r_out[..dec],
                    dec as i32,
                );
                for i in 0..out_len {
                    let idx = base + i * 2 + 1;
                    if idx < output.len() {
                        output[idx] = self.w_pcm_resampled[i] as f32 / 32768.0;
                    }
                }
            } else {
                let src = &self.w_pcm_i16[1..1 + dec];
                self.silk_resampler
                    .process(&mut self.w_pcm_resampled[..out_len], src, dec as i32);
                for i in 0..out_len {
                    let v = self.w_pcm_resampled[i] as f32 / 32768.0;
                    for ch in 0..self.channels {
                        let idx = base + i * self.channels + ch;
                        if idx < output.len() {
                            output[idx] = v;
                        }
                    }
                }
                // Mono into a stereo output: keep the right-channel resampler
                // continuous, as the normal path does (dec_API.c:351-355).
                if self.channels == 2 {
                    self.silk_resampler_r.process(
                        &mut self.w_pcm_resampled[..out_len],
                        src,
                        dec as i32,
                    );
                    for i in 0..out_len {
                        let idx = base + i * 2 + 1;
                        if idx < output.len() {
                            output[idx] = self.w_pcm_resampled[i] as f32 / 32768.0;
                        }
                    }
                }
            }
            off += out_len;
        }
        Ok(())
    }

    /// Forward-error-correction decode: reconstruct a LOST frame from the LBRR
    /// (low-bitrate redundancy) embedded in the NEXT received `packet`. Drives
    /// the SILK decoder in FLAG_DECODE_LBRR mode, which self-selects: it decodes
    /// the redundant frame when the packet carries LBRR for it, and falls back
    /// to PLC extrapolation when it doesn't. CELT-only or multi-frame packets
    /// fall back to plain PLC (no SILK LBRR to recover). After this call the
    /// caller decodes `packet` normally for the following frame.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidPacket`] if `input` is malformed or truncated;
    /// [`Error::BufferTooSmall`] if `frame_size` exceeds the decoder's capacity or
    /// `output`; [`Error::Internal`] if a codec stage fails.
    pub fn decode_fec(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, Error> {
        if packet.is_empty() {
            return self.decode_plc(frame_size, output);
        }
        let toc = packet[0];
        let mode = mode_from_toc(toc);
        // FEC only lives in SILK/hybrid low band; code-0 (single frame) only.
        if mode == OpusMode::CeltOnly || (toc & 0x03) != 0 {
            return self.decode_plc(frame_size, output);
        }
        let bandwidth = bandwidth_from_toc(toc);
        let payload = &packet[1..];

        let out_samples = frame_size * self.channels;
        if output.len() < out_samples {
            return Err(Error::BufferTooSmall("Output buffer too small"));
        }
        for v in output.iter_mut().take(out_samples) {
            *v = 0.0;
        }
        let frame_ms = (frame_size as i32 * 1000 / self.sampling_rate).max(1);
        let internal_rate = if mode == OpusMode::Hybrid {
            16000
        } else {
            match bandwidth {
                Bandwidth::Narrowband => 8000,
                Bandwidth::Mediumband => 12000,
                _ => 16000,
            }
        };
        if internal_rate != self.prev_internal_rate {
            self.silk_resampler.init(internal_rate, self.sampling_rate);
            self.prev_internal_rate = internal_rate;
        }
        let internal_frame = (frame_ms * internal_rate / 1000) as usize;
        let ratio = self.sampling_rate as f64 / internal_rate as f64;
        self.silk_dec.produce_lr = false;
        self.silk_dec.n_channels_internal = 1;

        let mut rc = RangeCoder::new_decoder(payload);
        let n16 = internal_frame;
        if n16 + 2 > self.w_pcm_i16.len() {
            return Err(Error::BufferTooSmall("opus FEC: frame exceeds buffer"));
        }
        self.w_pcm_i16[0] = self.silk_s_mid[0];
        self.w_pcm_i16[1] = self.silk_s_mid[1];
        let ret = self.silk_dec.decode(
            &mut rc,
            &mut self.w_pcm_i16[2..n16 + 2],
            silk::decode_frame::FLAG_DECODE_LBRR,
            true,
            frame_ms,
            internal_rate,
        );
        if ret < 0 {
            return Err(Error::Internal("SILK FEC failed"));
        }
        let dec = ret as usize;
        if dec >= 2 {
            self.silk_s_mid[0] = self.w_pcm_i16[dec];
            self.silk_s_mid[1] = self.w_pcm_i16[dec + 1];
        }
        // Always through the resampler (equal rates included): Copy mode carries
        // libopus's delay_matrix_dec delay (see decode()).
        {
            let out_len = (dec as f64 * ratio) as usize;
            let src: Vec<i16> = self.w_pcm_i16[1..1 + dec].to_vec();
            self.silk_resampler
                .process(&mut self.w_pcm_resampled[..out_len], &src, dec as i32);
            for i in 0..out_len {
                let v = self.w_pcm_resampled[i] as f32 / 32768.0;
                for ch in 0..self.channels {
                    let idx = i * self.channels + ch;
                    if idx < output.len() {
                        output[idx] = v;
                    }
                }
            }
        }
        self.prev_mode = Some(mode);
        Ok(frame_size)
    }

    ///
    /// # Errors
    ///
    /// [`Error::InvalidPacket`] if `input` is malformed, truncated, or longer than
    /// 120 ms; [`Error::BufferTooSmall`] if `output` cannot hold the decoded frame;
    /// [`Error::Internal`] if a codec stage fails. A malformed packet never panics.
    pub fn decode(
        &mut self,
        input: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, Error> {
        // Lost packet (data==NULL / empty) -> packet-loss concealment.
        if input.is_empty() {
            return self.decode_plc(frame_size, output);
        }

        let toc = input[0];
        let mode = mode_from_toc(toc);
        let packet_channels = channels_from_toc(toc);
        let bandwidth = bandwidth_from_toc(toc);
        let frame_duration_ms = frame_duration_ms_from_toc(toc);

        // A mono SILK packet inside a stereo stream is decoded through the PRIMARY
        // decoder (unified path), not a separate aux — the aux's SILK/resampler
        // state is blind to the interleaved stereo packets, so its state is stale
        // at every mono<->stereo switch. libopus keeps ONE decoder whose channel-0
        // resampler and stereo state run continuously across the switches.
        // A mono packet of ANY mode in a stereo stream decodes through the PRIMARY
        // (unified path) so inter-frame state stays one continuous chain across
        // mono<->stereo switches — SILK resampler/stereo state; CELT (and the
        // redundant/silence transition frames) via stream_channels=1 (C=1/CC=2) —
        // matching libopus's single decoder.
        let mono_in_stereo = packet_channels == 1 && self.channels == 2;

        if packet_channels != self.channels && !mono_in_stereo {
            // The packet's channel count differs from ours (a stream can switch
            // between mono and stereo). Decode it at its native channel count in
            // a persistent auxiliary decoder, then render to our output count:
            // mono->stereo duplicates, stereo->mono averages the two channels.
            if self
                .aux
                .as_ref()
                .is_none_or(|a| a.channels != packet_channels)
            {
                let mut aux = Box::new(Self::new(self.sampling_rate, packet_channels)?);
                // The C decoder is ONE mono decoder (disable_inv), not a stereo
                // one averaged afterwards: ignore inversion when we downmix.
                aux.celt_dec.disable_inv = self.channels == 1;
                self.aux = Some(aux);
            }
            // Reverse of the mono->stereo seed: on a stereo->mono switch, seed the
            // aux (mono) CELT decoder from the primary (stereo channel 0) so its
            // MDCT-overlap/energy state is continuous with the preceding stereo
            // packets (the primary was the continuous decoder during them).
            if !self.prev_used_aux
                && packet_channels == 1
                && self.channels == 2
                && (mode == OpusMode::CeltOnly || mode == OpusMode::Hybrid)
            {
                let (aux_opt, primary) = (&mut self.aux, &self.celt_dec);
                if let Some(aux) = aux_opt.as_mut() {
                    aux.celt_dec.seed_from(primary);
                }
            }
            let Some(aux) = self.aux.as_mut() else {
                return Err(Error::Internal("auxiliary decoder missing"));
            };
            let mut buf = vec![0.0f32; frame_size * packet_channels];
            let n = aux.decode(input, frame_size, &mut buf)?;
            self.last_range = aux.last_range;
            if packet_channels == 1 && self.channels == 2 {
                for i in 0..n {
                    let v = buf[i];
                    output[2 * i] = v;
                    output[2 * i + 1] = v;
                }
            } else if packet_channels == 2 && self.channels == 1 {
                for i in 0..n {
                    output[i] = 0.5 * (buf[2 * i] + buf[2 * i + 1]);
                }
            } else {
                let m = (n * self.channels).min(output.len()).min(buf.len());
                output[..m].copy_from_slice(&buf[..m]);
            }
            self.prev_mode = Some(mode);
            self.prev_used_aux = true;
            return Ok(n);
        }

        // First primary (native-channel) packet after a run of aux (mono-in-stereo)
        // packets: seed the primary CELT decoder's inter-frame state from the aux
        // so the mono->stereo MDCT overlap-add is continuous (matches libopus's
        // single continuous decoder). SILK carries its own state through the
        // primary already; this is for the CELT/Hybrid high band.
        if self.prev_used_aux {
            self.prev_used_aux = false;
            if (mode == OpusMode::CeltOnly || mode == OpusMode::Hybrid) && self.channels == 2 {
                if let Some(aux) = self.aux.as_ref() {
                    self.celt_dec.seed_from(&aux.celt_dec);
                }
            }
        }

        let code = toc & 0x03;
        let frame_count: usize;
        // At most 48 frames per packet (RFC 6716 3.2.5): a fixed table, not a
        // Vec per packet.
        let mut payload_tab: [&[u8]; 48] = [&[]; 48];

        match code {
            0 => {
                frame_count = 1;
                payload_tab[0] = &input[1..];
            }
            1 => {
                frame_count = 2;
                // Two frames of equal size (RFC 6716 3.2.3), possibly both
                // empty (concealed); an odd payload length is malformed.
                if (input.len() - 1) % 2 != 0 {
                    return Err(Error::InvalidPacket("Code 1: odd payload length"));
                }
                let half = (input.len() - 1) / 2;
                payload_tab[0] = &input[1..1 + half];
                payload_tab[1] = &input[1 + half..];
            }
            2 => {
                frame_count = 2;
                let data = &input[1..];
                if data.is_empty() {
                    return Err(Error::InvalidPacket("Code 2 packet has no data"));
                }
                let (first_len, header_size) = read_opus_frame_len(data, 0)?;
                if header_size + first_len > data.len() {
                    return Err(Error::InvalidPacket(
                        "Code 2: first frame size exceeds packet",
                    ));
                }
                payload_tab[0] = &data[header_size..header_size + first_len];
                payload_tab[1] = &data[header_size + first_len..];
            }
            _ => {
                // code == 3.
                // RFC 6716 §3.2.5. Frame-count byte: bit 7 = VBR flag, bit 6 =
                // padding flag, bits 5..0 = frame count M. VBR and padding are
                // independent; the earlier code conflated them (and used a
                // non-standard length coding), which mis-parsed CBR and padded
                // packets — exactly what the RFC test vectors exercise.
                if input.len() < 2 {
                    return Err(Error::InvalidPacket("Code 3 packet too short"));
                }
                let count_byte = input[1];
                let m = (count_byte & 0x3F) as usize;
                if !(1..=48).contains(&m) {
                    return Err(Error::InvalidPacket("Code 3: invalid frame count"));
                }
                // libopus opus.c opus_packet_parse_impl (code 3):
                //   if (count <= 0 || framesize*(opus_int32)count > 5760)
                //      return OPUS_INVALID_PACKET;
                // (framesize at 48 kHz; 5760 = 120 ms, the RFC 6716 packet cap.)
                // A hostile frame count past this cap would otherwise shrink our
                // per-frame size below the redundancy-fade windows further down.
                if m as i32 * repacketizer::samples_per_frame(toc, 48000) > 5760 {
                    return Err(Error::InvalidPacket(
                        "Code 3: packet duration exceeds 120 ms",
                    ));
                }
                frame_count = m;
                let vbr = (count_byte & 0x80) != 0;
                let padding = (count_byte & 0x40) != 0;

                // Padding length indicator bytes follow the count byte; the
                // padding data itself sits at the end of the packet.
                let mut ptr = 2usize;
                let mut pad_len = 0usize;
                if padding {
                    loop {
                        let p = *input
                            .get(ptr)
                            .ok_or(Error::InvalidPacket("Code 3: padding overflow"))?
                            as usize;
                        ptr += 1;
                        if p == 255 {
                            pad_len += 254;
                        } else {
                            pad_len += p;
                            break;
                        }
                    }
                }
                let end = input
                    .len()
                    .checked_sub(pad_len)
                    .ok_or(Error::InvalidPacket("Code 3: padding exceeds packet"))?;
                if ptr > end {
                    return Err(Error::InvalidPacket("Code 3: padding exceeds packet"));
                }
                // Frame-data region, with the length headers (VBR) at its front
                // and the trailing padding already excluded.
                let region = &input[ptr..end];

                if vbr {
                    // M-1 explicit frame lengths, contiguous, then the frame
                    // data; the last frame is the remainder.
                    let mut lens = [0usize; 48];
                    let mut hp = 0usize;
                    for l_out in lens.iter_mut().take(m - 1) {
                        let (l, nb) = read_opus_frame_len(region, hp)?;
                        hp += nb;
                        *l_out = l;
                    }
                    let mut fp = hp;
                    for (i, &l) in lens[..m - 1].iter().enumerate() {
                        if fp + l > region.len() {
                            return Err(Error::InvalidPacket(
                                "Code 3 VBR: frame length exceeds packet",
                            ));
                        }
                        payload_tab[i] = &region[fp..fp + l];
                        fp += l;
                    }
                    if fp > region.len() {
                        return Err(Error::InvalidPacket("Code 3 VBR: no data for last frame"));
                    }
                    payload_tab[m - 1] = &region[fp..];
                } else {
                    // CBR: the region splits into M equal frames (possibly all
                    // empty, e.g. DTX).
                    if region.len() % m != 0 {
                        return Err(Error::InvalidPacket(
                            "Code 3 CBR: frame data not divisible by frame count",
                        ));
                    }
                    let frame_len = region.len() / m;
                    for (i, p) in payload_tab[..m].iter_mut().enumerate() {
                        *p = &region[i * frame_len..(i + 1) * frame_len];
                    }
                }
            }
        }
        let frame_payloads = &payload_tab[..frame_count];
        // No Opus frame exceeds 1275 bytes (RFC 6716 3.2.1, R2).
        if frame_payloads.iter().any(|p| p.len() > 1275) {
            return Err(Error::InvalidPacket("frame exceeds 1275 bytes"));
        }

        // libopus opus_decoder.c opus_decode_native:
        //   if (count*packet_frame_size > frame_size)
        //      return OPUS_BUFFER_TOO_SMALL;
        // The packet's own TOC duration must fit the caller's frame_size. We split
        // the caller's buffer as sub_frame_size = frame_size / frame_count, so a
        // malformed multi-frame packet (large frame count vs. a small caller
        // buffer) would otherwise make sub_frame_size smaller than the 2.5/5 ms
        // redundancy-fade region — the fuzzer-found out-of-bounds/underflow panics
        // in redundancy_fade_start/redundancy_fade_end. C rejects such packets
        // here; so do we.
        let packet_frame_samples =
            repacketizer::samples_per_frame(toc, self.sampling_rate) as usize;
        // ...and the decoded samples must fit the caller's `output` slice, which
        // is independent of `frame_size` (an undersized slice used to panic on
        // an out-of-range slice index; found by the decode property tests).
        if frame_count * packet_frame_samples > frame_size
            || output.len() < frame_count * packet_frame_samples * self.channels
        {
            return Err(Error::BufferTooSmall("Output buffer too small"));
        }
        // The caller's frame_size is a CAPACITY (libopus): decode exactly the
        // packet's own duration and return it.
        let frame_size = frame_count * packet_frame_samples;

        // ---- Mode-transition frame (opus_decoder.c opus_decode_frame) ----
        // Entering CELT from SILK/hybrid without a redundant frame, or leaving
        // CELT for SILK/hybrid: libopus conceals min(5 ms, frame) in the
        // PREVIOUS mode before decoding, then uses it for the first 2.5 ms and
        // cross-fades into the decoded audio over the next 2.5 ms. Without it
        // our first post-switch frame differed from libopus by up to ~4.7k LSB.
        // Generated BEFORE bandwidth/stream_channels move to the new packet, so
        // the concealment runs on the previous mode's state (as data==NULL
        // does). Only the packet's first frame can be a switch: all frames in
        // one packet share a mode.
        let f5 = (self.sampling_rate / 200) as usize;
        let audiosize = frame_size / frame_count;
        let mut pcm_transition: Option<Vec<f32>> = None;
        if let Some(pm) = self.prev_mode {
            let transition =
                (mode == OpusMode::CeltOnly && pm != OpusMode::CeltOnly && !self.prev_redundancy)
                    || (mode != OpusMode::CeltOnly && pm == OpusMode::CeltOnly);
            if transition && pm == OpusMode::CeltOnly {
                self.transition_pending = f5.min(audiosize);
            } else if transition {
                // Conceal min(5 ms, frame) in the previous mode (SILK pads its
                // concealment to 10 ms internally; hybrid adds the CELT high band).
                let n = f5.min(audiosize);
                let mut buf = vec![0.0f32; n * self.channels];
                self.decode_plc(n, &mut buf)?;
                pcm_transition = Some(buf);
            }
        }
        self.first_frame_redundancy = false;

        // opus_decode_frame: `if (st->prev_mode==MODE_CELT_ONLY)
        // silk_ResetDecoder(silk_dec)` before any SILK/hybrid frame. That clears
        // fs_kHz too, so the SILK resampler restarts from zero state, and it
        // zeroes sStereo (incl. the 2-sample mid history). Carrying the old
        // SILK/resampler/stereo history across a CELT run left every SILK frame
        // after the switch 20-300 LSB off libopus.
        if mode != OpusMode::CeltOnly && self.prev_mode == Some(OpusMode::CeltOnly) {
            self.silk_dec.reset();
            self.silk_s_mid = [0; 2];
            self.prev_internal_rate = 0; // re-init both SILK resamplers
        }

        self.frame_size = frame_size;
        self.last_frame_size = frame_size / frame_count;
        self.bandwidth = bandwidth;
        self.stream_channels = packet_channels;

        let sub_frame_size = frame_size / frame_count;
        let sub_output_len = sub_frame_size * self.channels;

        let result = match mode {
            OpusMode::SilkOnly => {
                let internal_sample_rate = match bandwidth {
                    Bandwidth::Narrowband => 8000,
                    Bandwidth::Mediumband => 12000,
                    Bandwidth::Wideband => 16000,
                    _ => 16000,
                };
                let internal_frame_size =
                    (frame_duration_ms * internal_sample_rate / 1000) as usize;

                // Initialised at EQUAL rates too: libopus always runs
                // silk_resampler, whose Copy mode delays SILK by
                // delay_matrix_dec[in][out] (8k:4, 12k:9, 16k:12 samples) so it
                // stays aligned with CELT. Bypassing it at 8/12/16 kHz output
                // shifted every SILK sample against libopus's decoder.
                if internal_sample_rate != self.prev_internal_rate {
                    self.silk_resampler
                        .init(internal_sample_rate, self.sampling_rate);
                    self.silk_resampler_r
                        .init(internal_sample_rate, self.sampling_rate);
                    self.prev_internal_rate = internal_sample_rate;
                }

                // Pure-SILK stereo (both stream and output are 2ch): reconstruct
                // true L/R via SILK MS->LR instead of duplicating the mono mid.
                let silk_lr = self.channels == 2 && packet_channels == 2;
                self.silk_dec.produce_lr = silk_lr;

                // Per-packet internal channel switch (libopus dec_API.c:119-166).
                let prev_internal_ch = self.silk_dec.n_channels_internal;
                if packet_channels as i32 > prev_internal_ch {
                    // mono -> stereo: reset the side channel decoder.
                    silk::init_decoder::silk_init_decoder(&mut self.silk_dec.channel_state[1]);
                }
                if self.channels == 2 && packet_channels == 2 && prev_internal_ch == 1 {
                    // Switching to stereo: clear stereo prediction/side history and
                    // seed the right-channel resampler from the (continuous) left.
                    self.silk_dec.s_stereo_pred_prev_q13 = [0; 2];
                    self.silk_dec.s_stereo_side = [0; 2];
                    self.silk_resampler_r = self.silk_resampler.clone();
                }
                self.silk_dec.n_channels_internal = packet_channels as i32;

                // A 40/60 ms Opus frame carries 2/3 internal 20 ms SILK frames;
                // 10/20 ms carry one. libopus calls silk_Decode once per internal
                // frame (continuing the same range coder within the payload). We
                // must too — decoding only the first internal frame leaves the
                // rest of a 40/60 ms packet silent (the "collapse" bug).
                let n_silk = match frame_duration_ms {
                    40 => 2,
                    60 => 3,
                    _ => 1,
                };
                let internal_sub_frame_size = internal_frame_size / n_silk;
                let ratio = self.sampling_rate as f64 / internal_sample_rate as f64;
                // Per-FRAME previous mode (libopus updates prev_mode per frame; for
                // payloads after the first, the previous frame is this same packet).
                let mut prev_mode_frame = self.prev_mode;

                for (fi, payload) in frame_payloads.iter().enumerate() {
                    let mut rc =
                        RangeCoder::new_decoder_in(std::mem::take(&mut self.rc_scratch), payload);
                    let pcm_i16_len = internal_sub_frame_size * self.channels;
                    // A malformed packet can imply a frame larger than our scratch
                    // buffer; reject it gracefully instead of slicing out of bounds
                    // (a decode-path DoS on attacker-controlled input).
                    if pcm_i16_len + 2 > self.w_pcm_i16.len() {
                        return Err(Error::InvalidPacket("opus: SILK frame size exceeds buffer"));
                    }
                    let out_start = fi * sub_output_len;
                    let mut silk_off = 0usize; // output samples/ch within this Opus frame

                    for sf in 0..n_silk {
                        let s_mid = self.silk_s_mid;
                        let ret = {
                            let (silk_dec, pcm_i16) = (&mut self.silk_dec, &mut self.w_pcm_i16);
                            // Prepend the previous frame's last two samples (sMid) at
                            // [0..2] and decode at offset 2, matching libopus's
                            // samplesOut1_tmp[n][2] layout.
                            pcm_i16[0] = s_mid[0];
                            pcm_i16[1] = s_mid[1];
                            silk_dec.decode(
                                &mut rc,
                                &mut pcm_i16[2..pcm_i16_len + 2],
                                silk::decode_frame::FLAG_DECODE_NORMAL,
                                sf == 0,
                                frame_duration_ms,
                                internal_sample_rate,
                            )
                        };

                        if ret < 0 {
                            return Err(Error::Internal("SILK decoding failed"));
                        }

                        let decoded_samples = ret as usize;
                        // Carry the last two decoded samples as next frame's sMid.
                        if decoded_samples >= 2 {
                            self.silk_s_mid[0] = self.w_pcm_i16[decoded_samples];
                            self.silk_s_mid[1] = self.w_pcm_i16[decoded_samples + 1];
                        }
                        let base = out_start + silk_off * self.channels;

                        // Stereo SILK: L in silk_dec.l_out, R in silk_dec.r_out,
                        // both already in the 1-sample-delay-line layout. Resample
                        // each channel through its own resampler.
                        let out_len = if silk_lr {
                            let out_len = (decoded_samples as f64 * ratio) as usize;
                            // Left
                            self.silk_resampler.process(
                                &mut self.w_pcm_resampled[..out_len],
                                &self.silk_dec.l_out[..decoded_samples],
                                decoded_samples as i32,
                            );
                            for i in 0..out_len {
                                let idx = base + i * 2;
                                if idx < output.len() {
                                    output[idx] = self.w_pcm_resampled[i] as f32 / 32768.0;
                                }
                            }
                            // Right (reuse the scratch)
                            self.silk_resampler_r.process(
                                &mut self.w_pcm_resampled[..out_len],
                                &self.silk_dec.r_out[..decoded_samples],
                                decoded_samples as i32,
                            );
                            for i in 0..out_len {
                                let idx = base + i * 2 + 1;
                                if idx < output.len() {
                                    output[idx] = self.w_pcm_resampled[i] as f32 / 32768.0;
                                }
                            }
                            out_len
                        } else {
                            let out_len = (decoded_samples as f64 * ratio) as usize;
                            debug_assert!(out_len <= self.w_pcm_resampled.len());
                            {
                                let (silk_res, pcm_i16, pcm_out) = (
                                    &mut self.silk_resampler,
                                    &self.w_pcm_i16,
                                    &mut self.w_pcm_resampled,
                                );
                                silk_res.process(
                                    &mut pcm_out[..out_len],
                                    &pcm_i16[1..1 + decoded_samples],
                                    decoded_samples as i32,
                                );
                            }
                            for i in 0..out_len {
                                let v = self.w_pcm_resampled[i] as f32 / 32768.0;
                                for ch in 0..self.channels {
                                    let idx = base + i * self.channels + ch;
                                    if idx < output.len() {
                                        output[idx] = v;
                                    }
                                }
                            }
                            // Stereo output, mono packet: also run the mono signal
                            // through the RIGHT-channel resampler so its state stays
                            // continuous for the next stereo packet (libopus
                            // dec_API.c:351-355). Its output overwrites channel 1,
                            // which is numerically ~identical to the left here.
                            if self.channels == 2 {
                                self.silk_resampler_r.process(
                                    &mut self.w_pcm_resampled[..out_len],
                                    &self.w_pcm_i16[1..1 + decoded_samples],
                                    decoded_samples as i32,
                                );
                                for i in 0..out_len {
                                    let idx = base + i * 2 + 1;
                                    if idx < output.len() {
                                        output[idx] = self.w_pcm_resampled[i] as f32 / 32768.0;
                                    }
                                }
                            }
                            out_len
                        };
                        silk_off += out_len;
                    }

                    // --- Opus redundancy layer (opus_decoder.c:420-580) ---
                    // A SILK-only frame carries IMPLICIT CELT redundancy: if >= 17
                    // bits remain after SILK, the trailing bytes ARE a 5 ms CELT
                    // frame (no flag) used to smooth mode/bandwidth transitions.
                    let mut redundant_rng = 0u32;
                    let mut redundancy = false;
                    let mut celt_to_silk = false;
                    let plen = payload.len();
                    let f5 = (self.sampling_rate / 200) as usize;
                    let f2_5 = f5 / 2;
                    let red_end_band = celt_endband_for_bandwidth(bandwidth);
                    let mut red_buf = [0.0f32; 480]; // F5 * <=2ch, planar
                    let mut red_bytes = 0usize;
                    if rc.tell() + 17 <= (plen as i32) * 8 {
                        redundancy = true;
                        celt_to_silk = rc.decode_bit_logp(1);
                        red_bytes = plen - (((rc.tell() + 7) >> 3) as usize);
                        if red_bytes < 2 || red_bytes >= plen {
                            redundancy = false;
                            red_bytes = 0;
                        }
                    }
                    self.run_transition_plc(fi, redundancy);
                    // CELT->SILK: the redundant frame continues the prior CELT
                    // state (a fade-out of the previous CELT mode). Decode BEFORE
                    // the hybrid->SILK silence frame to keep libopus state order.
                    if redundancy && celt_to_silk {
                        redundant_rng = self.decode_redundant_celt(
                            &payload[plen - red_bytes..],
                            false,
                            packet_channels,
                            red_end_band,
                            &mut red_buf[..f5 * self.channels],
                        );
                    }
                    // Hybrid->SILK transition: let the CELT MDCT fade out by
                    // decoding a 2-byte silence frame; its 2.5 ms overlap tail is
                    // ADDED to the output (libopus decodes it into pcm before the
                    // SILK sum).
                    if prev_mode_frame == Some(OpusMode::Hybrid)
                        && !(redundancy && celt_to_silk && self.prev_redundancy)
                    {
                        let silence = [0xFFu8, 0xFF];
                        let mut sil_buf = [0.0f32; 240]; // F2_5 * <=2ch, planar
                        self.celt_dec.set_stream_channels(packet_channels);
                        let mut src = RangeCoder::new_decoder(&silence);
                        self.celt_dec.decode_from_range_coder_with_band_range(
                            &mut src,
                            16,
                            f2_5,
                            &mut sil_buf[..f2_5 * self.channels],
                            0,
                            red_end_band,
                        );
                        let region = &mut output[out_start..out_start + sub_output_len];
                        for i in 0..f2_5 {
                            for c in 0..self.channels {
                                region[i * self.channels + c] += sil_buf[c * f2_5 + i];
                            }
                        }
                    }
                    // SILK->CELT: reset, then decode — this PRIMES the CELT state
                    // for the upcoming CELT-mode frames (which is why the next mode
                    // change skips its reset when prev_redundancy is set).
                    if redundancy && !celt_to_silk {
                        redundant_rng = self.decode_redundant_celt(
                            &payload[plen - red_bytes..],
                            true,
                            packet_channels,
                            red_end_band,
                            &mut red_buf[..f5 * self.channels],
                        );
                    }
                    if redundancy {
                        let window = modes::default_mode().window;
                        let region = &mut output[out_start..out_start + sub_output_len];
                        if celt_to_silk {
                            redundancy_fade_start(
                                region,
                                &red_buf,
                                f5,
                                f2_5,
                                self.channels,
                                window,
                            );
                        } else {
                            redundancy_fade_end(
                                region,
                                sub_frame_size,
                                &red_buf,
                                f5,
                                f2_5,
                                self.channels,
                                window,
                            );
                        }
                    }
                    if fi == 0 {
                        self.first_frame_redundancy = redundancy;
                    }
                    self.prev_redundancy = redundancy && !celt_to_silk;
                    prev_mode_frame = Some(OpusMode::SilkOnly);
                    self.last_range = rc.rng ^ redundant_rng;
                    self.rc_scratch = rc.buf; // reuse the payload buffer next frame
                }
                self.prev_mode = Some(OpusMode::SilkOnly);
                Ok(frame_size)
            }

            OpusMode::CeltOnly => {
                let celt_end_band = Self::celt_end_band_from_toc(toc);
                // libopus opus_decoder.c:515 — discard CELT state on a mode change
                // unless the previous frame's SILK->CELT redundant frame already
                // primed it.
                if let Some(pm) = self.prev_mode {
                    if pm != OpusMode::CeltOnly && !self.prev_redundancy {
                        self.celt_dec.reset();
                    }
                }
                self.prev_redundancy = false;
                // Mono packet in a stereo stream => C=1, CC=2 (continuous state).
                self.celt_dec.set_stream_channels(packet_channels);

                for (fi, payload) in frame_payloads.iter().enumerate() {
                    let mut rc =
                        RangeCoder::new_decoder_in(std::mem::take(&mut self.rc_scratch), payload);
                    let total_bits = (payload.len() * 8) as i32;
                    let needed = sub_frame_size * self.channels;
                    let out_start = fi * needed;
                    let out_end = (out_start + needed).min(output.len());

                    if output.len() < out_end {
                        return Err(Error::BufferTooSmall("Output buffer too small"));
                    }

                    if self.channels == 1 {
                        self.celt_dec.decode_from_range_coder_with_band_range(
                            &mut rc,
                            total_bits,
                            sub_frame_size,
                            &mut output[out_start..out_end],
                            0,
                            celt_end_band,
                        );
                        for sample in &mut output[out_start..out_end] {
                            *sample = sample.clamp(-1.0, 1.0);
                        }
                    } else {
                        self.celt_dec.decode_from_range_coder_with_band_range(
                            &mut rc,
                            total_bits,
                            sub_frame_size,
                            &mut self.w_celt_planar[..needed],
                            0,
                            celt_end_band,
                        );
                        for i in 0..sub_frame_size {
                            for ch in 0..self.channels {
                                let idx = out_start + i * self.channels + ch;
                                output[idx] =
                                    self.w_celt_planar[ch * sub_frame_size + i].clamp(-1.0, 1.0);
                            }
                        }
                    }
                    self.last_range = rc.rng;
                    self.rc_scratch = rc.buf; // reuse the payload buffer next frame
                }
                self.prev_mode = Some(OpusMode::CeltOnly);
                Ok(frame_size)
            }

            OpusMode::Hybrid => {
                let internal_sample_rate = 16000;
                let internal_frame_size =
                    (frame_duration_ms * internal_sample_rate / 1000) as usize;
                let celt_end_band = Self::celt_end_band_from_toc(toc);

                // Initialised at EQUAL rates too: libopus always runs
                // silk_resampler, whose Copy mode delays SILK by
                // delay_matrix_dec[in][out] (8k:4, 12k:9, 16k:12 samples) so it
                // stays aligned with CELT. Bypassing it at 8/12/16 kHz output
                // shifted every SILK sample against libopus's decoder.
                if internal_sample_rate != self.prev_internal_rate {
                    self.silk_resampler
                        .init(internal_sample_rate, self.sampling_rate);
                    self.silk_resampler_r
                        .init(internal_sample_rate, self.sampling_rate);
                    self.prev_internal_rate = internal_sample_rate;
                }

                // Same SILK stereo/channel handling as the SilkOnly arm: true L/R
                // low band via MS->LR for stereo packets; per-packet internal
                // channel switch with side-channel/stereo-state resets.
                let silk_lr = self.channels == 2 && packet_channels == 2;
                self.silk_dec.produce_lr = silk_lr;
                let prev_internal_ch = self.silk_dec.n_channels_internal;
                if packet_channels as i32 > prev_internal_ch {
                    silk::init_decoder::silk_init_decoder(&mut self.silk_dec.channel_state[1]);
                }
                if self.channels == 2 && packet_channels == 2 && prev_internal_ch == 1 {
                    self.silk_dec.s_stereo_pred_prev_q13 = [0; 2];
                    self.silk_dec.s_stereo_side = [0; 2];
                    self.silk_resampler_r = self.silk_resampler.clone();
                }
                self.silk_dec.n_channels_internal = packet_channels as i32;

                for (fi, payload) in frame_payloads.iter().enumerate() {
                    let mut rc =
                        RangeCoder::new_decoder_in(std::mem::take(&mut self.rc_scratch), payload);
                    let pcm_silk_i16_len = internal_frame_size * self.channels;
                    if pcm_silk_i16_len + 2 > self.w_pcm_i16.len() {
                        return Err(Error::InvalidPacket("opus: SILK frame size exceeds buffer"));
                    }

                    // Prepend the previous frame's last two samples (sMid) and
                    // decode at offset 2, matching libopus's samplesOut1_tmp[n][2]
                    // layout — the resampler is fed from offset 1 (the 1-sample
                    // delay line), keeping the SILK low band aligned with the CELT
                    // high band exactly as in the reference.
                    let s_mid = self.silk_s_mid;
                    let ret = {
                        let (silk_dec, pcm_i16) = (&mut self.silk_dec, &mut self.w_pcm_i16);
                        pcm_i16[0] = s_mid[0];
                        pcm_i16[1] = s_mid[1];
                        silk_dec.decode(
                            &mut rc,
                            &mut pcm_i16[2..pcm_silk_i16_len + 2],
                            silk::decode_frame::FLAG_DECODE_NORMAL,
                            true,
                            frame_duration_ms,
                            internal_sample_rate,
                        )
                    };

                    if ret < 0 {
                        return Err(Error::Internal("SILK decoding failed"));
                    }

                    let silk_out_len = sub_frame_size * self.channels;
                    self.w_silk_out[..silk_out_len].fill(0.0);
                    if ret > 0 {
                        let decoded_samples = ret as usize;
                        if decoded_samples >= 2 {
                            self.silk_s_mid[0] = self.w_pcm_i16[decoded_samples];
                            self.silk_s_mid[1] = self.w_pcm_i16[decoded_samples + 1];
                        }
                        let ratio = self.sampling_rate as f64 / internal_sample_rate as f64;
                        let out_len =
                            ((decoded_samples as f64 * ratio) as usize).min(sub_frame_size);
                        debug_assert!(out_len <= self.w_pcm_resampled.len());
                        if silk_lr {
                            // Stereo low band: L/R from dec_api (already in the
                            // 1-sample-delay layout), each through its own resampler.
                            self.silk_resampler.process(
                                &mut self.w_pcm_resampled[..out_len],
                                &self.silk_dec.l_out[..decoded_samples],
                                decoded_samples as i32,
                            );
                            for i in 0..out_len {
                                self.w_silk_out[i * 2] = self.w_pcm_resampled[i] as f32 / 32768.0;
                            }
                            self.silk_resampler_r.process(
                                &mut self.w_pcm_resampled[..out_len],
                                &self.silk_dec.r_out[..decoded_samples],
                                decoded_samples as i32,
                            );
                            for i in 0..out_len {
                                self.w_silk_out[i * 2 + 1] =
                                    self.w_pcm_resampled[i] as f32 / 32768.0;
                            }
                        } else {
                            self.silk_resampler.process(
                                &mut self.w_pcm_resampled[..out_len],
                                &self.w_pcm_i16[1..1 + decoded_samples],
                                decoded_samples as i32,
                            );
                            for i in 0..out_len {
                                let v = self.w_pcm_resampled[i] as f32 / 32768.0;
                                for ch in 0..self.channels {
                                    self.w_silk_out[i * self.channels + ch] = v;
                                }
                            }
                            // Mono packet, stereo output: keep the right-channel
                            // resampler continuous (libopus dec_API.c:351-355).
                            if self.channels == 2 {
                                self.silk_resampler_r.process(
                                    &mut self.w_pcm_resampled[..out_len],
                                    &self.w_pcm_i16[1..1 + decoded_samples],
                                    decoded_samples as i32,
                                );
                                for i in 0..out_len {
                                    self.w_silk_out[i * 2 + 1] =
                                        self.w_pcm_resampled[i] as f32 / 32768.0;
                                }
                            }
                        }
                    }

                    // --- Opus redundancy layer, hybrid form (opus_decoder.c) ---
                    // redundancy = bit(12); if set: celt_to_silk = bit(1),
                    // redundancy_bytes = uint(256)+2 taken from the END of the
                    // packet — the MAIN CELT layer still decodes, but with the
                    // range coder's storage shrunk by those bytes (this changes
                    // its raw-bit region and tell budget).
                    let plen = payload.len();
                    let mut redundancy = false;
                    let mut celt_to_silk = false;
                    let mut red_bytes = 0usize;
                    let mut effective_len = plen;
                    if rc.tell() + 37 <= (plen as i32) * 8 {
                        redundancy = rc.decode_bit_logp(12);
                        if redundancy {
                            celt_to_silk = rc.decode_bit_logp(1);
                            red_bytes = rc.dec_uint(256) as usize + 2;
                            if red_bytes <= effective_len {
                                effective_len -= red_bytes;
                            } else {
                                red_bytes = 0;
                                redundancy = false;
                            }
                            if redundancy && (effective_len as i32) * 8 < rc.tell() {
                                effective_len = plen;
                                red_bytes = 0;
                                redundancy = false;
                            }
                            if redundancy {
                                rc.storage -= red_bytes as u32;
                            }
                        }
                    }
                    self.run_transition_plc(fi, redundancy);
                    let f5 = (self.sampling_rate / 200) as usize;
                    let f2_5 = f5 / 2;
                    let red_end_band = celt_endband_for_bandwidth(bandwidth);
                    let mut red_buf = [0.0f32; 480];
                    let mut redundant_rng = 0u32;
                    let do_red = redundancy;
                    // CELT->SILK: redundant frame decodes BEFORE the main CELT,
                    // continuing the prior CELT state (fade-out of previous CELT).
                    if do_red && celt_to_silk {
                        redundant_rng = self.decode_redundant_celt(
                            &payload[plen - red_bytes..],
                            false,
                            packet_channels,
                            red_end_band,
                            &mut red_buf[..f5 * self.channels],
                        );
                    }

                    // Main CELT high band. libopus opus_decoder.c:515 — reset CELT
                    // on a mode change unless primed by prior SILK->CELT redundancy.
                    if fi == 0 {
                        if let Some(pm) = self.prev_mode {
                            if pm != OpusMode::Hybrid && !self.prev_redundancy {
                                self.celt_dec.reset();
                            }
                        }
                    }
                    self.celt_dec.set_stream_channels(packet_channels);
                    let total_bits = (effective_len * 8) as i32;
                    {
                        let (celt_dec, celt_planar) = (&mut self.celt_dec, &mut self.w_celt_planar);
                        celt_dec.decode_from_range_coder_with_band_range(
                            &mut rc,
                            total_bits,
                            sub_frame_size,
                            &mut celt_planar[..silk_out_len],
                            17,
                            celt_end_band,
                        );

                        if self.channels == 1 {
                            self.w_celt_out[..silk_out_len]
                                .copy_from_slice(&self.w_celt_planar[..silk_out_len]);
                        } else {
                            for i in 0..sub_frame_size {
                                for ch in 0..self.channels {
                                    self.w_celt_out[i * self.channels + ch] =
                                        self.w_celt_planar[ch * sub_frame_size + i];
                                }
                            }
                        }
                    }

                    let out_start = fi * silk_out_len;
                    let total = silk_out_len.min(output.len() - out_start);
                    for j in 0..total {
                        output[out_start + j] =
                            (self.w_silk_out[j] + self.w_celt_out[j]).clamp(-1.0, 1.0);
                    }

                    // SILK->CELT: reset + decode the redundant frame AFTER the main
                    // decode; it primes the CELT state for the upcoming CELT mode.
                    if do_red && !celt_to_silk {
                        redundant_rng = self.decode_redundant_celt(
                            &payload[plen - red_bytes..],
                            true,
                            packet_channels,
                            red_end_band,
                            &mut red_buf[..f5 * self.channels],
                        );
                    }
                    if do_red {
                        let window = modes::default_mode().window;
                        let region = &mut output[out_start..out_start + silk_out_len];
                        if celt_to_silk {
                            redundancy_fade_start(
                                region,
                                &red_buf,
                                f5,
                                f2_5,
                                self.channels,
                                window,
                            );
                        } else {
                            redundancy_fade_end(
                                region,
                                sub_frame_size,
                                &red_buf,
                                f5,
                                f2_5,
                                self.channels,
                                window,
                            );
                        }
                    }
                    if fi == 0 {
                        self.first_frame_redundancy = redundancy;
                    }
                    self.prev_redundancy = redundancy && !celt_to_silk;
                    self.last_range = rc.rng ^ redundant_rng;
                    self.rc_scratch = rc.buf; // reuse the payload buffer next frame
                }
                self.prev_mode = Some(OpusMode::Hybrid);
                Ok(frame_size)
            }
        };

        // A CELT->SILK/hybrid transition is cancelled when the new frame
        // carries redundancy (its redundant CELT frame does the fade).
        self.transition_pending = 0;
        if let Some(t) = self.transition_pcm.take() {
            pcm_transition = Some(t);
        }
        if let (Some(t), Ok(_)) = (pcm_transition.as_ref(), &result) {
            if !(mode != OpusMode::CeltOnly && self.first_frame_redundancy) {
                let ch = self.channels;
                let f2_5 = f5 / 2;
                let window = modes::default_mode().window;
                let inc = (48000 / self.sampling_rate) as usize;
                // smooth_fade(in1, in2, out): out = w*in2 + (1-w)*in1, w = win^2.
                if audiosize >= f5 {
                    output[..ch * f2_5].copy_from_slice(&t[..ch * f2_5]);
                    for c in 0..ch {
                        for i in 0..f2_5 {
                            let w = window[i * inc] * window[i * inc];
                            let idx = (f2_5 + i) * ch + c;
                            output[idx] = w * output[idx] + (1.0 - w) * t[idx];
                        }
                    }
                } else {
                    // Shorter than 5 ms: fade over the first 2.5 ms anyway.
                    for c in 0..ch {
                        for i in 0..f2_5 {
                            let w = window[i * inc] * window[i * inc];
                            let idx = i * ch + c;
                            output[idx] = w * output[idx] + (1.0 - w) * t[idx];
                        }
                    }
                }
            }
        }
        result
    }
}

impl OpusDecoder {
    #[inline(always)]
    fn celt_end_band_from_toc(toc: u8) -> usize {
        let mode = modes::default_mode();
        let top = mode.eff_ebands;
        if mode_from_toc(toc) == OpusMode::CeltOnly && toc >= 0x80 {
            const FROM_OPUS_TABLE: [u8; 16] = [
                0x80, 0x88, 0x90, 0x98, 0x40, 0x48, 0x50, 0x58, 0x20, 0x28, 0x30, 0x38, 0x00, 0x08,
                0x10, 0x18,
            ];
            let idx = ((toc >> 3) - 16) as usize;
            let data0 = FROM_OPUS_TABLE[idx] | (toc & 0x7);
            let trim = (data0 >> 5) as usize;
            return top.saturating_sub(2 * trim).max(1);
        }
        // Hybrid: libopus maps the packet bandwidth to a CELT end band
        // (opus_decoder.c: SWB -> 19, FB -> 21). Decoding SWB hybrid with 21
        // reads two bands the encoder never coded -> range desync every packet.
        if mode_from_toc(toc) == OpusMode::Hybrid
            && bandwidth_from_toc(toc) == Bandwidth::Superwideband
        {
            return 19.min(top);
        }
        top
    }

    /// Decode a redundant CELT frame (opus_decoder.c "5 ms redundant frame"):
    /// start band 0, end band from the packet bandwidth, 5 ms, its own range
    /// decoder. Returns the redundant final range; PLANAR output in `buf`
    /// (F5 samples per state channel). Only valid at 48 kHz output.
    /// The deferred CELT->SILK/hybrid transition concealment (opus_decode_frame:
    /// after the redundancy decision, before any CELT decode of the frame).
    fn run_transition_plc(&mut self, fi: usize, redundancy: bool) {
        let n = std::mem::take(&mut self.transition_pending);
        if fi == 0 && n > 0 && !redundancy {
            let mut buf = vec![0.0f32; n * self.channels];
            self.celt_dec.conceal_lost(n, &mut buf);
            self.transition_pcm = Some(buf);
        }
    }

    fn decode_redundant_celt(
        &mut self,
        red: &[u8],
        reset_first: bool,
        packet_channels: usize,
        end_band: usize,
        buf: &mut [f32],
    ) -> u32 {
        if reset_first {
            self.celt_dec.reset();
        }
        self.celt_dec.set_stream_channels(packet_channels);
        let f5 = (self.sampling_rate / 200) as usize;
        let mut rrc = RangeCoder::new_decoder_in(std::mem::take(&mut self.red_rc_scratch), red);
        let total_bits = (red.len() * 8) as i32;
        self.celt_dec
            .decode_from_range_coder_with_band_range(&mut rrc, total_bits, f5, buf, 0, end_band);
        let rng = rrc.rng;
        self.red_rc_scratch = rrc.buf;
        rng
    }
}

/// libopus opus_decoder.c bandwidth -> CELT end band for the packet.
fn celt_endband_for_bandwidth(bw: Bandwidth) -> usize {
    match bw {
        Bandwidth::Narrowband => 13,
        Bandwidth::Mediumband | Bandwidth::Wideband => 17,
        Bandwidth::Superwideband => 19,
        _ => 21,
    }
}

/// smooth_fade cross-fades (w = window[i*inc]^2, inc = 48000/Fs) applied to the
/// interleaved output region of one frame. `red` is PLANAR (F5 per channel).
/// celt_to_silk: redundant frame occupies the START of the frame — first 2.5 ms
/// copied verbatim, next 2.5 ms fades redundant -> main.
///
/// Indexing invariant: `out.len() >= f5 * channels` (writes reach sample
/// f5-1 = 2*f2_5-1). A malformed multi-frame packet used to violate this (a
/// hostile frame count made the per-frame region tinier than F5, fuzzer-found
/// OOB panics here); decode() now rejects such packets up front exactly as C
/// libopus does (opus_decode_native's count*packet_frame_size > frame_size ->
/// OPUS_BUFFER_TOO_SMALL, and the 120 ms cap of opus_packet_parse_impl), so a
/// redundant frame always has >= 10 ms of frame to fade into, as in C.
fn redundancy_fade_start(
    out: &mut [f32],
    red: &[f32],
    f5: usize,
    f2_5: usize,
    channels: usize,
    window: &[f32],
) {
    // smooth_fade steps the 48 kHz window by inc = 48000/Fs (F2.5 = 120 at 48k).
    let inc = 120 / f2_5;
    for i in 0..f2_5 {
        for c in 0..channels {
            out[i * channels + c] = red[c * f5 + i];
        }
    }
    for i in 0..f2_5 {
        let w = window[i * inc] * window[i * inc];
        for c in 0..channels {
            let idx = (f2_5 + i) * channels + c;
            out[idx] = (1.0 - w) * red[c * f5 + f2_5 + i] + w * out[idx];
        }
    }
}

/// SILK->CELT: redundant frame occupies the END of the frame — the last 2.5 ms
/// fades main -> redundant (second half of the redundant frame).
///
/// Indexing invariant: `frame_samples >= f2_5` and `out.len() >=
/// frame_samples * channels` (the index `frame_samples - f2_5 + i` would
/// otherwise underflow). A malformed multi-frame packet used to violate this
/// (fuzzer-found subtract-with-overflow panic here); decode() now rejects such
/// packets up front exactly as C libopus does (opus_decode_native's
/// count*packet_frame_size > frame_size -> OPUS_BUFFER_TOO_SMALL, plus the
/// 120 ms cap of opus_packet_parse_impl), so redundancy only ever runs on
/// frames of >= 10 ms, as in C.
fn redundancy_fade_end(
    out: &mut [f32],
    frame_samples: usize,
    red: &[f32],
    f5: usize,
    f2_5: usize,
    channels: usize,
    window: &[f32],
) {
    // smooth_fade steps the 48 kHz window by inc = 48000/Fs (F2.5 = 120 at 48k).
    let inc = 120 / f2_5;
    for i in 0..f2_5 {
        let w = window[i * inc] * window[i * inc];
        for c in 0..channels {
            let idx = (frame_samples - f2_5 + i) * channels + c;
            out[idx] = (1.0 - w) * out[idx] + w * red[c * f5 + f2_5 + i];
        }
    }
}

// Test helper only: encode() validates against the full frame_size_select list
// (this `Fs % frame_size` form rejects 60/100/120 ms at 48 kHz).
#[cfg(test)]
fn frame_rate_from_params(sampling_rate: i32, frame_size: usize) -> Option<i32> {
    let frame_size = frame_size as i32;
    if frame_size == 0 || sampling_rate % frame_size != 0 {
        return None;
    }
    Some(sampling_rate / frame_size)
}

fn gen_toc(mode: OpusMode, frame_rate: i32, bandwidth: Bandwidth, channels: usize) -> u8 {
    let mut rate = frame_rate;
    let mut period = 0;
    while rate < 400 {
        rate <<= 1;
        period += 1;
    }

    let mut toc = match mode {
        OpusMode::SilkOnly => {
            let bw = (bandwidth as i32 - Bandwidth::Narrowband as i32) << 5;
            let per = (period - 2) << 3;
            (bw | per) as u8
        }
        OpusMode::CeltOnly => {
            let mut tmp = bandwidth as i32 - Bandwidth::Mediumband as i32;
            if tmp < 0 {
                tmp = 0;
            }
            let per = period << 3;
            (0x80 | (tmp << 5) | per) as u8
        }
        OpusMode::Hybrid => {
            let base_config = if bandwidth == Bandwidth::Superwideband {
                12
            } else {
                14
            };
            let period_offset = i32::from(frame_rate < 100);
            ((base_config + period_offset) << 3) as u8
        }
    };

    if channels == 2 {
        toc |= 0x04;
    }
    toc
}

fn mode_from_toc(toc: u8) -> OpusMode {
    if toc & 0x80 != 0 {
        OpusMode::CeltOnly
    } else if toc & 0x60 == 0x60 {
        OpusMode::Hybrid
    } else {
        OpusMode::SilkOnly
    }
}

fn bandwidth_from_toc(toc: u8) -> Bandwidth {
    let mode = mode_from_toc(toc);
    match mode {
        OpusMode::SilkOnly => {
            let bw_bits = (toc >> 5) & 0x03;
            match bw_bits {
                0 => Bandwidth::Narrowband,
                1 => Bandwidth::Mediumband,
                2 => Bandwidth::Wideband,
                _ => Bandwidth::Wideband,
            }
        }
        OpusMode::Hybrid => {
            let bw_bit = (toc >> 4) & 0x01;
            if bw_bit == 0 {
                Bandwidth::Superwideband
            } else {
                Bandwidth::Fullband
            }
        }
        OpusMode::CeltOnly => {
            let bw_bits = (toc >> 5) & 0x03;
            match bw_bits {
                0 => Bandwidth::Mediumband,
                1 => Bandwidth::Wideband,
                2 => Bandwidth::Superwideband,
                3 => Bandwidth::Fullband,
                _ => Bandwidth::Fullband,
            }
        }
    }
}

fn frame_duration_ms_from_toc(toc: u8) -> i32 {
    let mode = mode_from_toc(toc);
    match mode {
        OpusMode::SilkOnly => {
            let config = (toc >> 3) & 0x03;
            match config {
                0 => 10,
                1 => 20,
                2 => 40,
                3 => 60,
                _ => 20,
            }
        }
        OpusMode::Hybrid => {
            let config = (toc >> 3) & 0x01;
            if config == 0 { 10 } else { 20 }
        }
        OpusMode::CeltOnly => {
            let config = (toc >> 3) & 0x03;
            match config {
                0 => 2,
                1 => 5,
                2 => 10,
                3 => 20,
                _ => 20,
            }
        }
    }
}

fn channels_from_toc(toc: u8) -> usize {
    if toc & 0x04 != 0 { 2 } else { 1 }
}

/// RFC 6716 §3.1 frame-length coding (used by code 2 and VBR code 3): a length
/// of 0..=251 is one byte with that value; 252..=1275 is two bytes `b0` (252..255)
/// then `b1`, giving `b1*4 + b0`. Returns `(length, bytes_consumed)`.
fn read_opus_frame_len(data: &[u8], ptr: usize) -> Result<(usize, usize), Error> {
    let b0 = *data
        .get(ptr)
        .ok_or(Error::InvalidPacket("Opus frame length: truncated"))? as usize;
    if b0 < 252 {
        Ok((b0, 1))
    } else {
        let b1 = *data
            .get(ptr + 1)
            .ok_or(Error::InvalidPacket("Opus frame length: truncated 2-byte"))?
            as usize;
        Ok((b1 * 4 + b0, 2))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_size_from_toc(toc: u8, sampling_rate: i32) -> Option<usize> {
        let mode = mode_from_toc(toc);
        match mode {
            OpusMode::CeltOnly => {
                let period = ((toc >> 3) & 0x03) as i32;
                let frame_rate = 400 >> period;
                if frame_rate == 0 || sampling_rate % frame_rate != 0 {
                    return None;
                }
                Some((sampling_rate / frame_rate) as usize)
            }
            OpusMode::SilkOnly => {
                let duration_ms = frame_duration_ms_from_toc(toc);
                Some((sampling_rate as i64 * duration_ms as i64 / 1000) as usize)
            }
            OpusMode::Hybrid => {
                let duration_ms = frame_duration_ms_from_toc(toc);
                Some((sampling_rate as i64 * duration_ms as i64 / 1000) as usize)
            }
        }
    }

    #[test]
    fn gen_toc_matches_celt_reference_values() {
        let sampling_rate = 48_000;
        let cases = [
            (120usize, 0xE0u8),
            (240usize, 0xE8u8),
            (480usize, 0xF0u8),
            (960usize, 0xF8u8),
        ];

        for (frame_size, expected_toc) in cases {
            let frame_rate = frame_rate_from_params(sampling_rate, frame_size).unwrap();
            let toc = gen_toc(OpusMode::CeltOnly, frame_rate, Bandwidth::Fullband, 1);
            assert_eq!(
                toc, expected_toc,
                "frame_size {frame_size} expected TOC {expected_toc:02X} got {toc:02X}"
            );
            let decoded_size = frame_size_from_toc(toc, sampling_rate).unwrap();
            assert_eq!(decoded_size, frame_size);
        }

        let stereo_toc = gen_toc(
            OpusMode::CeltOnly,
            frame_rate_from_params(sampling_rate, 960).unwrap(),
            Bandwidth::Fullband,
            2,
        );
        assert_eq!(channels_from_toc(stereo_toc), 2);
    }

    #[test]
    fn test_celt_decoder_large_frame_sizes() {
        let sampling_rate = 48000;
        let channels = 1;

        let mut decoder = OpusDecoder::new(sampling_rate, channels).unwrap();

        let frame_sizes = [120, 240, 480, 960];

        for frame_size in frame_sizes {
            let toc = gen_toc(
                OpusMode::CeltOnly,
                frame_rate_from_params(sampling_rate, frame_size).unwrap(),
                Bandwidth::Fullband,
                channels,
            );
            let packet = [toc, 0, 0, 0, 0];

            let mut output = vec![0.0f32; frame_size * channels];

            let _ = decoder.decode(&packet, frame_size, &mut output);
        }

        let channels = 2;
        let mut decoder = OpusDecoder::new(sampling_rate, channels).unwrap();

        for frame_size in frame_sizes {
            let toc = gen_toc(
                OpusMode::CeltOnly,
                frame_rate_from_params(sampling_rate, frame_size).unwrap(),
                Bandwidth::Fullband,
                channels,
            );
            let packet = [toc, 0, 0, 0, 0];

            let mut output = vec![0.0f32; frame_size * channels];
            let _ = decoder.decode(&packet, frame_size, &mut output);
        }
    }

    #[test]
    fn test_celt_decoder_edge_case_frame_sizes() {
        let sampling_rate = 48000;
        let channels = 1;
        let mut decoder = OpusDecoder::new(sampling_rate, channels).unwrap();

        let edge_sizes = [2048, 2167, 2168, 2169, 2880, 3072];

        for frame_size in edge_sizes {
            let mut output = vec![0.0f32; frame_size * channels];

            let _ = decoder.decode(&[0x80, 0, 0, 0], frame_size, &mut output);
        }
    }

    // Regression test for: "index out of bounds: the len is 48 but the index is 119"
    // Root cause: frame_size=48 at 48kHz gives frame_rate=1000, which is not a valid
    // Hybrid-mode frame rate but was not validated.  CELT's lm-search then silently
    // fell back to lm=0, computed n2=120, and wrote output[119] into a 48-element
    // slice.  Triggered via G.729-decoded PCM (8kHz) passed to a 48kHz Opus encoder
    // without proper resampling, so the encoder received 48 samples instead of 480.
    #[test]
    fn test_invalid_small_frame_size_returns_error_not_panic() {
        let mut enc = OpusEncoder::new(48000, 2, Application::Voip).unwrap();
        enc.bitrate_bps = 64000;
        enc.complexity = 5;
        enc.use_cbr = true;

        // 48 samples at 48kHz = 1ms → frame_rate=1000, invalid for Hybrid mode.
        let input = vec![0.0f32; 48 * 2]; // stereo interleaved
        let mut output = vec![0u8; 256];

        let result = enc.encode(&input, 48, &mut output);
        assert!(
            result.is_err(),
            "encode with invalid frame_size=48 should return Err, not panic"
        );
    }

    // Also verify that the Audio application path (always Hybrid at 48 kHz) rejects
    // the same bad frame size.
    #[test]
    fn test_invalid_small_frame_size_audio_application_returns_error() {
        let mut enc = OpusEncoder::new(48000, 1, Application::Audio).unwrap();
        let input = vec![0.0f32; 48];
        let mut output = vec![0u8; 256];

        let result = enc.encode(&input, 48, &mut output);
        assert!(
            result.is_err(),
            "Audio/48kHz encoder with frame_size=48 should return Err"
        );
    }
}
