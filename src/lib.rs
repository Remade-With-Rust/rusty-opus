#![allow(unsafe_op_in_unsafe_fn)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_range_loop)]

pub mod analysis;
pub mod analysis_data;
pub mod bands;
pub mod celt;
pub mod celt_lpc;
pub mod hp_cutoff;
pub mod kiss_fft;
pub mod mdct;
pub mod modes;
pub mod parallel;
pub mod pitch;
pub mod prof;
pub mod pvq;
pub mod quant_bands;
pub mod range_coder;
pub mod rate;
pub mod silk;

pub use silk::{SilkResampler, SilkResamplerDown1_3, SilkResamplerDown1_6};

pub use celt::{CeltDecoder, CeltEncoder};
use hp_cutoff::hp_cutoff;
use range_coder::RangeCoder;
use silk::control_codec::silk_control_encoder;
use silk::enc_api::silk_encode;
use silk::init_encoder::silk_init_encoder;
use silk::lin2log::silk_lin2log;
use silk::log2lin::silk_log2lin;
use silk::macros::*;
use silk::resampler::{silk_resampler_down2, silk_resampler_down2_3};
use silk::structs::SilkEncoderState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    Voip = 2048,
    Audio = 2049,
    RestrictedLowDelay = 2051,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    Auto = -1000,
    Narrowband = 1101,
    Mediumband = 1102,
    Wideband = 1103,
    Superwideband = 1104,
    Fullband = 1105,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpusMode {
    SilkOnly,
    Hybrid,
    CeltOnly,
}

pub struct OpusEncoder {
    celt_enc: CeltEncoder,
    silk_enc: Box<SilkEncoderState>,
    application: Application,
    sampling_rate: i32,
    channels: usize,
    bandwidth: Bandwidth,
    pub bitrate_bps: i32,
    pub complexity: i32,
    pub use_cbr: bool,

    pub use_inband_fec: bool,

    /// Discontinuous transmission: after enough consecutive inactive frames,
    /// emit a 1-byte (TOC-only) packet so the decoder runs comfort-noise/PLC.
    pub use_dtx: bool,
    /// Consecutive inactive milliseconds, in Q1 (opus_encoder.c nb_no_activity).
    nb_no_activity_ms_q1: i32,
    /// Final range-coder state of the last packet (0 for DTX/PLC packets, which
    /// carry no coded range — opus_encoder.c st->rangeFinal).
    range_final: u32,

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
}

// libopus opus_encoder.c bandwidth thresholds: (threshold, hysteresis) pairs for
// NB<->MB, MB<->WB, WB<->SWB, SWB<->FB, interpolated voice<->music by voice_est^2.
const MONO_VOICE_BANDWIDTH_THRESHOLDS: [i32; 8] = [9000, 700, 9000, 700, 13500, 1000, 14000, 2000];
const MONO_MUSIC_BANDWIDTH_THRESHOLDS: [i32; 8] = [9000, 700, 9000, 700, 11000, 1000, 12000, 2000];
const STEREO_VOICE_BANDWIDTH_THRESHOLDS: [i32; 8] = [9000, 700, 9000, 700, 13500, 1000, 14000, 2000];
const STEREO_MUSIC_BANDWIDTH_THRESHOLDS: [i32; 8] = [9000, 700, 9000, 700, 11000, 1000, 12000, 2000];

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
mod silk_rate_tests {
    use super::compute_silk_rate_for_hybrid;
    use crate::Bandwidth;

    #[test]
    fn test_reference_table_exact_entries() {
        assert_eq!(compute_silk_rate_for_hybrid(12000, Bandwidth::Fullband, true, true), 10000);
        assert_eq!(compute_silk_rate_for_hybrid(16000, Bandwidth::Fullband, true, true), 13500);
        assert_eq!(compute_silk_rate_for_hybrid(20000, Bandwidth::Fullband, true, true), 16000);
        assert_eq!(compute_silk_rate_for_hybrid(24000, Bandwidth::Fullband, true, true), 18000);
        assert_eq!(compute_silk_rate_for_hybrid(32000, Bandwidth::Fullband, true, true), 22000);
        assert_eq!(compute_silk_rate_for_hybrid(64000, Bandwidth::Fullband, true, true), 38000);
    }

    #[test]
    fn test_32kbps_gives_22kbps_silk() {
        assert_eq!(compute_silk_rate_for_hybrid(32000, Bandwidth::Fullband, true, true), 22000);
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
    pub fn new(
        sampling_rate: i32,
        channels: usize,
        application: Application,
    ) -> Result<Self, &'static str> {
        if ![8000, 12000, 16000, 24000, 48000].contains(&sampling_rate) {
            return Err("Invalid sampling rate");
        }
        if ![1, 2].contains(&channels) {
            return Err("Invalid number of channels");
        }

        let mode = modes::default_mode();
        let celt_enc = CeltEncoder::new(mode, channels);

        let mut silk_enc = Box::new(SilkEncoderState::default());
        if silk_init_encoder(&mut silk_enc, 0) != 0 {
            return Err("SILK encoder initialization failed");
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
            tonality: analysis::TonalityAnalysisState::new(sampling_rate),
            analysis_kfft: kiss_fft::KissFftState::new(480),
            lsb_depth: 24,
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
        })
    }

    pub fn enable_hybrid_mode(&mut self) -> Result<(), &'static str> {
        if self.sampling_rate != 24000 && self.sampling_rate != 48000 {
            return Err("Hybrid mode requires 24kHz or 48kHz sampling rate");
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

    /// opus_encoder.c:1296 voice_est ladder (signal_type is AUTO for us):
    /// analysis-driven when voice_ratio is known, else application defaults.
    fn compute_voice_est(&self) -> i32 {
        if self.voice_ratio >= 0 {
            let mut v = self.voice_ratio * 327 >> 8;
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

    pub fn encode(
        &mut self,
        input: &[f32],
        frame_size: usize,
        output: &mut [u8],
    ) -> Result<usize, &'static str> {
        let _prof_total = crate::prof::scope(crate::prof::Stage::Total);
        if output.len() < 2 {
            return Err("Output buffer too small");
        }

        let frame_rate = frame_rate_from_params(self.sampling_rate, frame_size)
            .ok_or("Invalid frame size for sampling rate")?;

        // ---- Tonality analysis (opus_encoder.c:1123) ----
        let mut analysis_info = analysis::AnalysisInfo::default();
        if self.complexity >= 7 && self.sampling_rate >= 16000 {
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
        self.detected_bandwidth = 0;
        if analysis_info.valid {
            // signal_type is AUTO: pick the hysteresis-correct probability.
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
                th[i] = mt[i] + ((voice_est * voice_est * (vt[i] - mt[i])) >> 14);
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
            // Hybrid at unsafe CBR rates starves SILK: cap at WB below 15 kb/s.
            if mode != OpusMode::CeltOnly && self.use_cbr && self.bitrate_bps < 15000 {
                bw = bw.min(Bandwidth::Wideband as i32);
            }
            // NB/MB SILK-internal rates (8/12 kHz) aren't wired for >16 kHz API
            // input yet (no 48k->8k/12k encode resamplers); clamp to WB.
            if mode != OpusMode::CeltOnly && self.sampling_rate > 16000 {
                bw = bw.max(Bandwidth::Wideband as i32);
            }
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
            // (MB remap above may have been undone by the caps; keep WB floor
            // only where the API rate allows it.)
            if bw == Bandwidth::Mediumband as i32 && self.sampling_rate > 12000 {
                bw = Bandwidth::Wideband as i32;
            }
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
            // The CELT TOC has no mediumband config; C maps MB down to NB.
            if mode == OpusMode::CeltOnly && bw == MB {
                bw = NB;
            }
            self.bandwidth = match self.force_bandwidth {
                Some(f) => f,
                None => match bw {
                    x if x == NB => Bandwidth::Narrowband,
                    x if x == MB => Bandwidth::Mediumband,
                    x if x == Bandwidth::Wideband as i32 => Bandwidth::Wideband,
                    x if x == Bandwidth::Superwideband as i32 => Bandwidth::Superwideband,
                    x if x == FB => Bandwidth::Fullband,
                    _ => Bandwidth::Wideband,
                },
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

        // Stereo HYBRID is not validated yet (the stereo SILK layer desyncs in
        // the hybrid configuration); code those frames as CELT fullband. Plain
        // stereo SILK-only (<= WB) is fine and stays.
        if self.channels == 2 && mode == OpusMode::Hybrid {
            mode = OpusMode::CeltOnly;
            self.bandwidth = Bandwidth::Fullband;
        }

        if mode == OpusMode::CeltOnly {
            match frame_rate {
                400 | 200 | 100 | 50 => {}
                _ => return Err("Unsupported frame size for CELT-only mode"),
            }
        }

        if mode == OpusMode::Hybrid {
            match frame_rate {
                100 | 50 => {}
                _ => return Err("Unsupported frame size for Hybrid mode"),
            }
        }

        if mode == OpusMode::SilkOnly {
            match frame_rate {
                400 | 200 | 100 | 50 | 25 => {}
                _ => return Err("Unsupported frame size for SILK-only mode"),
            }
        }

        let n400 = (self.sampling_rate / 400) as usize;

        // ---- Mode-transition resets (opus_encoder.c:1449 + 2054) ----
        // The decoder resets its CELT state on ANY mode change (when there is
        // no redundancy) and its SILK state when leaving CELT-only; the
        // encoder must mirror both or the streams desync from that frame on.
        if let Some(prev) = self.prev_enc_mode {
            if prev != mode {
                if mode != OpusMode::SilkOnly {
                    let ch = self.channels;
                    self.celt_enc = CeltEncoder::new(modes::default_mode(), ch);
                    // Prefill 2.5 ms so the fresh state has real preemph/overlap
                    // history instead of a hard edge (opus_encoder.c:2060).
                    let n400 = (self.sampling_rate / 400) as usize;
                    if self.celt_prefill_tail.len() == n400 * ch {
                        let mut dummy = RangeCoder::new_encoder(2);
                        let tail = std::mem::take(&mut self.celt_prefill_tail);
                        self.celt_enc.encode_with_budget(&tail, n400, &mut dummy, 0, 21, 16);
                        self.celt_prefill_tail = tail;
                    }
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
                    self.silk_prefill_tail[i] = (input[frame_size - n10 + i] * 32768.0)
                        .clamp(-32768.0, 32767.0) as i16;
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
                const HI: i32 = (silk::define::NB_SPEECH_FRAMES_BEFORE_DTX + silk::define::MAX_CONSECUTIVE_DTX) * 20 * 2; // 1200
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
        let max_data_bytes = output.len();

        // CBR: the packet is exactly the target size. VBR: start the coder on a
        // generous buffer — SILK-only packets end at whatever SILK produced, and
        // the CELT layer picks its own frame size (compute_vbr) and shrinks the
        // coder to it (libopus opus_encoder.c / celt_encoder.c VBR flow).
        let n_bytes = if self.use_cbr {
            cbr_bytes.min(max_data_bytes).max(1)
        } else {
            max_data_bytes.min(1276).max(cbr_bytes.min(max_data_bytes)).max(3)
        };

        let init_rc_size = n_bytes - 1;
        self.rc.reset_for_encode(init_rc_size as u32);

        if mode == OpusMode::SilkOnly || mode == OpusMode::Hybrid {
            let silk_fs_khz = if mode == OpusMode::Hybrid {
                16
            } else {
                self.sampling_rate.min(16000) / 1000
            };

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
                self.silk_enc.s_cmn.use_cbr = if self.use_cbr { 1 } else { 0 };

                self.silk_enc.s_cmn.n_channels = self.channels as i32;
                self.silk_initialized = true;
                self.down2_state_first = [0; 2];
                self.down2_state_second = [0; 2];
                self.down2_3_state = [0; 6];
                self.down_1_3_state = silk::resampler::SilkResamplerDown1_3::default();
                self.down2_3_state_r = [0; 6];
                self.down_1_3_state_r = silk::resampler::SilkResamplerDown1_3::default();
                self.down_fir_l =
                    silk::resampler::SilkDownFirResampler::new(self.sampling_rate, 16000);
                self.down_fir_r =
                    silk::resampler::SilkDownFirResampler::new(self.sampling_rate, 16000);
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
                    if self.sampling_rate > 16000 {
                        if let Some(r) = &mut self.down_fir_l {
                            r.process(&mut resampled, &self.silk_prefill_tail);
                        }
                    } else {
                        resampled.copy_from_slice(&self.silk_prefill_tail[..need]);
                    }
                    silk::enc_api::silk_encode_prefill(&mut self.silk_enc, &resampled, 0);
                }
            }

            self.silk_enc.s_cmn.use_in_band_fec = if self.use_inband_fec { 1 } else { 0 };
            self.silk_enc.s_cmn.packet_loss_perc = self.packet_loss_perc.clamp(0, 100);

            self.silk_enc.s_cmn.lbrr_enabled = if self.use_inband_fec { 1 } else { 0 };

            if self.silk_enc.s_cmn.lbrr_gain_increases == 0 {
                self.silk_enc.s_cmn.lbrr_gain_increases = 2;
            }

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

            let _prof_rs = crate::prof::scope(crate::prof::Stage::Resample);
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
                let need_resample = self.sampling_rate > 16000;
                let ds_len = if !need_resample {
                    frame_length
                } else if self.sampling_rate == 48000 {
                    frame_length / 3
                } else {
                    frame_length * 2 / 3
                };
                if need_resample {
                    self.buf_stereo_mid.resize(ds_len, 0);
                    self.buf_stereo_side.resize(ds_len, 0);
                    if let (Some(rl), Some(rr)) = (&mut self.down_fir_l, &mut self.down_fir_r) {
                        rl.process(&mut self.buf_stereo_mid, &self.buf_left);
                        rr.process(&mut self.buf_stereo_side, &self.buf_right);
                    }
                    self.buf_left.resize(ds_len, 0);
                    self.buf_right.resize(ds_len, 0);
                    self.buf_left.copy_from_slice(&self.buf_stereo_mid[..ds_len]);
                    self.buf_right.copy_from_slice(&self.buf_stereo_side[..ds_len]);
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
            } else if mode == OpusMode::SilkOnly && self.sampling_rate > 16000 {
                if self.sampling_rate == 48000 {
                    // 48k -> 16k via the same direct FIR the Hybrid path uses. The
                    // old down2 + down2_3 two-stage chain ALIASES: a 1 kHz sine
                    // came out with a 7 kHz mirror at ~1/3 amplitude (spectrum-
                    // verified), wrecking every SILK-only encode from 48 kHz input.
                    let silk_frame_size = frame_size / 3;
                    self.buf_silk_input.resize(silk_frame_size, 0);
                    if let Some(r) = &mut self.down_fir_l {
                        r.process(&mut self.buf_silk_input, input_i16);
                    }
                    &self.buf_silk_input
                } else if self.sampling_rate == 24000 {
                    let silk_frame_size = frame_size * 2 / 3;
                    self.buf_silk_input.resize(silk_frame_size, 0);
                    if let Some(r) = &mut self.down_fir_l {
                        r.process(&mut self.buf_silk_input, input_i16);
                    }
                    &self.buf_silk_input
                } else {
                    input_i16
                }
            } else if mode == OpusMode::Hybrid && self.sampling_rate > 16000 {
                let silk_frame_size = if self.sampling_rate == 48000 {
                    frame_size / 3
                } else {
                    frame_size * 2 / 3
                };
                self.buf_silk_input.resize(silk_frame_size, 0);
                if let Some(r) = &mut self.down_fir_l {
                    r.process(&mut self.buf_silk_input, input_i16);
                }
                &self.buf_silk_input
            } else {
                input_i16
            };

            drop(_prof_rs);

            let mut pn_bytes = 0;

            // The frames-per-second math below divides by silk_input.len(), which is
            // at the SILK-INTERNAL rate — so the rate here must be internal too.
            // Using the API rate at 48 kHz told SILK to target 3x the real budget
            // with a hard max_bits cap -> the gain loop crushed every frame to fit
            // -> near-silent output (only worked at 16 kHz API where they coincide).
            let silk_rate_for_calc = if mode == OpusMode::Hybrid {
                16000
            } else {
                self.sampling_rate.min(16000)
            };
            let silk_frame_len = silk_input.len();

            let silk_bitrate = if mode == OpusMode::Hybrid {
                let frame_duration_ms = frame_size as i32 * 1000 / self.sampling_rate;
                let frame20ms = frame_duration_ms >= 20;
                compute_silk_rate_for_hybrid(self.bitrate_bps, curr_bw, frame20ms, !self.use_cbr)
            } else if self.use_cbr {
                (8i64 * (n_bytes - 1) as i64 * silk_rate_for_calc as i64 / silk_frame_len as i64)
                    as i32
            } else {
                // VBR: n_bytes is only the buffer cap; target the configured rate.
                self.bitrate_bps
            };
            let silk_max_bits = if mode == OpusMode::Hybrid {
                let total_max_bits = ((n_bytes - 1) * 8) as i32;
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
                ((n_bytes - 1) * 8) as i32
            };
            let silk_use_cbr = if mode == OpusMode::Hybrid && self.use_cbr {
                0
            } else if self.use_cbr {
                1
            } else {
                0
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
                return Err("SILK encoding failed");
            }
        }

        // The hybrid redundancy flag is only present when >=37 bits remain
        // (opus_encoder.c: ec_tell+17+20 <= 8*(max_data_bytes-1)); the decoder
        // gates its read identically. Writing it unconditionally desynced every
        // frame where SILK left fewer than 37 bits (starved low-rate hybrid).
        if mode == OpusMode::Hybrid && self.rc.tell() + 37 <= ((n_bytes - 1) * 8) as i32 {
            self.rc.encode_bit_logp(false, 12); // redundancy = 0
        }

        if mode == OpusMode::Hybrid {
            let nb_compr_bytes = (n_bytes - 1) as u32;
            self.rc.shrink(nb_compr_bytes);
        }

        let silk_ret_bytes = if mode == OpusMode::SilkOnly {
            ((self.rc.tell() + 7) >> 3) as usize
        } else {
            0
        };

        if mode == OpusMode::CeltOnly || mode == OpusMode::Hybrid {
            self.celt_enc.analysis = celt::AnalysisInfo {
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
            self.celt_enc.complexity = self.complexity;
            self.celt_enc.lsb_depth = self.lsb_depth;
            let start_band = if mode == OpusMode::Hybrid { 17 } else { 0 };
            // CELT end band from the coded bandwidth (mirrors the decoder's
            // celt_endband_for_bandwidth): NB->13, MB/WB->17, SWB->19, FB->21.
            let end_band = match self.bandwidth {
                Bandwidth::Narrowband => 13,
                Bandwidth::Mediumband | Bandwidth::Wideband => 17,
                Bandwidth::Superwideband => 19,
                _ => 21,
            };
            let total_packet_bits = ((n_bytes - 1) * 8) as i32;
            // VBR: hand CELT the target in eighth-bits per frame; it picks the
            // frame's size (compute_vbr) and shrinks the range coder to it. The
            // hybrid target covers the whole packet (CELT adds back the SILK
            // bits via `target += tell`).
            self.celt_enc.vbr_rate = if self.use_cbr {
                0
            } else {
                let den = self.sampling_rate >> 3; // Fs >> BITRES
                ((self.bitrate_bps as i64 * frame_size as i64 + (den >> 1) as i64)
                    / den as i64) as i32
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
        self.range_final = self.rc.rng;

        if mode == OpusMode::SilkOnly {
            let mut ret = silk_ret_bytes.min(self.rc.storage as usize);
            while ret > 2 && self.rc.buf[ret - 1] == 0 {
                ret -= 1;
            }

            let target_total = if self.use_cbr {
                n_bytes.min(output.len())
            } else {
                (ret + 1).min(output.len())
            };

            let silk_len = ret;

            if !self.use_cbr || silk_len + 1 >= target_total {
                // VBR or payload fills the target: simple code 0 packet
                output[0] = toc;
                let copy_len = silk_len.min(target_total - 1);
                output[1..1 + copy_len].copy_from_slice(&self.rc.buf[..copy_len]);
                return Ok((copy_len + 1).min(output.len()));
            }

            output[0] = toc | 0x03;

            if silk_len + 2 >= target_total {
                output[1] = 0x01;
                let copy_len = (target_total - 2).min(silk_len);
                output[2..2 + copy_len].copy_from_slice(&self.rc.buf[..copy_len]);
                self.prev_enc_mode = Some(mode);
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

            output[ptr..ptr + silk_len].copy_from_slice(&self.rc.buf[..silk_len]);
            ptr += silk_len;

            let fill_end = target_total.min(output.len());
            for byte in output[ptr..fill_end].iter_mut() {
                *byte = 0;
            }

            self.prev_enc_mode = Some(mode);
            return Ok(target_total.min(output.len()));
        }

        // CBR: fixed payload. VBR (CELT/hybrid): the CELT layer shrank the coder
        // to this frame's chosen size — emit exactly that many payload bytes.
        let payload_len = if self.use_cbr {
            n_bytes - 1
        } else {
            (self.rc.storage as usize).min(n_bytes - 1)
        };
        output[1..1 + payload_len].copy_from_slice(&self.rc.buf[..payload_len]);
        self.prev_enc_mode = Some(mode);
        Ok(1 + payload_len)
    }
}

pub struct OpusDecoder {
    celt_dec: CeltDecoder,
    silk_dec: silk::dec_api::SilkDecoder,
    sampling_rate: i32,
    channels: usize,

    prev_mode: Option<OpusMode>,
    frame_size: usize,

    bandwidth: Bandwidth,

    stream_channels: usize,

    silk_resampler: silk::resampler::SilkResampler,
    // Second resampler for the SILK stereo right channel (L uses silk_resampler).
    silk_resampler_r: silk::resampler::SilkResampler,

    prev_internal_rate: i32,

    pub hybrid_skip_celt: bool,

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

    // Range decoder final `rng` from the last decoded frame (conformance/desync
    // diagnostic: compare against the encoder's stored final range).
    pub last_range: u32,

    // Auxiliary decoder for packets whose channel count differs from ours
    // (a stream may switch between mono and stereo). It decodes at the packet's
    // native channel count; we then up/downmix to our output count. Persistent
    // so the "other" channel mode keeps its own inter-frame state.
    aux: Option<Box<OpusDecoder>>,
    // Set when a packet was just decoded by the aux (a mono packet in a stereo
    // stream); triggers seeding the primary CELT decoder's overlap/energy state
    // from the aux at the next primary (stereo) CELT/Hybrid packet, so the MDCT
    // overlap-add is continuous across the mono->stereo switch.
    prev_used_aux: bool,
    // libopus st->prev_redundancy: the previous frame carried a SILK->CELT
    // redundant frame (redundancy && !celt_to_silk). Suppresses the CELT reset on
    // the following mode change (the redundant frame already primed CELT state).
    prev_redundancy: bool,
}

impl OpusDecoder {
    pub fn new(sampling_rate: i32, channels: usize) -> Result<Self, &'static str> {
        if ![8000, 12000, 16000, 24000, 48000].contains(&sampling_rate) {
            return Err("Invalid sampling rate");
        }
        if ![1, 2].contains(&channels) {
            return Err("Invalid number of channels");
        }

        let mode = modes::default_mode();
        let celt_dec = CeltDecoder::new(mode, channels);

        let mut silk_dec = silk::dec_api::SilkDecoder::new();
        silk_dec.init(sampling_rate.min(16000), channels as i32);
        silk_dec.channel_state[0].fs_api_hz = sampling_rate;

        Ok(Self {
            celt_dec,
            silk_dec,
            sampling_rate,
            channels,
            prev_mode: None,
            frame_size: 0,
            bandwidth: Bandwidth::Auto,
            stream_channels: channels,
            silk_resampler: silk::resampler::SilkResampler::default(),
            silk_resampler_r: silk::resampler::SilkResampler::default(),
            prev_internal_rate: 0,
            hybrid_skip_celt: false,

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
        })
    }

    /// Packet-loss concealment for a lost frame (empty/None packet). Runs the
    /// SILK PLC (LTP+LPC extrapolation) for the last-known SILK/hybrid mode and
    /// resamples to the output rate. CELT-only loss has no CELT PLC yet, so it
    /// yields silence (a documented Tier-1 follow-up); the SILK path covers the
    /// dominant VoIP case. Mono conceal is duplicated to both channels on a
    /// stereo output.
    fn decode_plc(
        &mut self,
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, &'static str> {
        let out_samples = frame_size * self.channels;
        for v in output.iter_mut().take(out_samples) {
            *v = 0.0;
        }
        let mode = self.prev_mode.unwrap_or(OpusMode::SilkOnly);
        if mode == OpusMode::CeltOnly {
            // No CELT PLC port yet — graceful silence.
            self.prev_mode = Some(mode);
            return Ok(frame_size);
        }

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
        if self.sampling_rate != internal_rate && internal_rate != self.prev_internal_rate {
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
        // Conceal mono only (the SILK low band); stereo output duplicates it.
        self.silk_dec.produce_lr = false;
        self.silk_dec.n_channels_internal = 1;

        let mut off = 0usize; // output samples/ch written so far
        for sf in 0..n_silk {
            let mut rc = RangeCoder::new_decoder(&[]);
            let n16 = internal_sub;
            if n16 + 2 > self.w_pcm_i16.len() {
                return Err("opus PLC: frame exceeds buffer");
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
                return Err("SILK PLC failed");
            }
            let dec = ret as usize;
            if dec >= 2 {
                self.silk_s_mid[0] = self.w_pcm_i16[dec];
                self.silk_s_mid[1] = self.w_pcm_i16[dec + 1];
            }
            let base = off * self.channels;
            let out_len = if self.sampling_rate == internal_rate {
                for i in 0..dec {
                    let v = self.w_pcm_i16[1 + i] as f32 / 32768.0;
                    for ch in 0..self.channels {
                        let idx = base + i * self.channels + ch;
                        if idx < output.len() {
                            output[idx] = v;
                        }
                    }
                }
                dec
            } else {
                let out_len = (dec as f64 * ratio) as usize;
                let src: Vec<i16> = self.w_pcm_i16[1..1 + dec].to_vec();
                self.silk_resampler
                    .process(&mut self.w_pcm_resampled[..out_len], &src, dec as i32);
                for i in 0..out_len {
                    let v = self.w_pcm_resampled[i] as f32 / 32768.0;
                    for ch in 0..self.channels {
                        let idx = base + i * self.channels + ch;
                        if idx < output.len() {
                            output[idx] = v;
                        }
                    }
                }
                out_len
            };
            off += out_len;
        }
        self.prev_mode = Some(mode);
        Ok(frame_size)
    }

    /// Forward-error-correction decode: reconstruct a LOST frame from the LBRR
    /// (low-bitrate redundancy) embedded in the NEXT received `packet`. Drives
    /// the SILK decoder in FLAG_DECODE_LBRR mode, which self-selects: it decodes
    /// the redundant frame when the packet carries LBRR for it, and falls back
    /// to PLC extrapolation when it doesn't. CELT-only or multi-frame packets
    /// fall back to plain PLC (no SILK LBRR to recover). After this call the
    /// caller decodes `packet` normally for the following frame.
    pub fn decode_fec(
        &mut self,
        packet: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, &'static str> {
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
        if self.sampling_rate != internal_rate && internal_rate != self.prev_internal_rate {
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
            return Err("opus FEC: frame exceeds buffer");
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
            return Err("SILK FEC failed");
        }
        let dec = ret as usize;
        if dec >= 2 {
            self.silk_s_mid[0] = self.w_pcm_i16[dec];
            self.silk_s_mid[1] = self.w_pcm_i16[dec + 1];
        }
        if self.sampling_rate == internal_rate {
            for i in 0..dec {
                let v = self.w_pcm_i16[1 + i] as f32 / 32768.0;
                for ch in 0..self.channels {
                    let idx = i * self.channels + ch;
                    if idx < output.len() {
                        output[idx] = v;
                    }
                }
            }
        } else {
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

    pub fn decode(
        &mut self,
        input: &[u8],
        frame_size: usize,
        output: &mut [f32],
    ) -> Result<usize, &'static str> {
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
                .map(|a| a.channels != packet_channels)
                .unwrap_or(true)
            {
                self.aux = Some(Box::new(OpusDecoder::new(
                    self.sampling_rate,
                    packet_channels,
                )?));
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
            let aux = self.aux.as_mut().unwrap();
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
        let frame_payloads: Vec<&[u8]>;

        match code {
            0 => {
                frame_count = 1;
                frame_payloads = vec![&input[1..]];
            }
            1 => {
                frame_count = 2;
                let half = (input.len() - 1) / 2;
                if half == 0 {
                    return Err("Code 1: empty frame");
                }
                frame_payloads = vec![&input[1..1 + half], &input[1 + half..]];
            }
            2 => {
                frame_count = 2;
                let data = &input[1..];
                if data.is_empty() {
                    return Err("Code 2 packet has no data");
                }
                let (first_len, header_size) = read_opus_frame_len(data, 0)?;
                if header_size + first_len > data.len() {
                    return Err("Code 2: first frame size exceeds packet");
                }
                frame_payloads = vec![
                    &data[header_size..header_size + first_len],
                    &data[header_size + first_len..],
                ];
            }
            3 => {
                // RFC 6716 §3.2.5. Frame-count byte: bit 7 = VBR flag, bit 6 =
                // padding flag, bits 5..0 = frame count M. VBR and padding are
                // independent; the earlier code conflated them (and used a
                // non-standard length coding), which mis-parsed CBR and padded
                // packets — exactly what the RFC test vectors exercise.
                if input.len() < 2 {
                    return Err("Code 3 packet too short");
                }
                let count_byte = input[1];
                let m = (count_byte & 0x3F) as usize;
                if m < 1 || m > 48 {
                    return Err("Code 3: invalid frame count");
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
                        let p = *input.get(ptr).ok_or("Code 3: padding overflow")? as usize;
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
                    .ok_or("Code 3: padding exceeds packet")?;
                if ptr > end {
                    return Err("Code 3: padding exceeds packet");
                }
                // Frame-data region, with the length headers (VBR) at its front
                // and the trailing padding already excluded.
                let region = &input[ptr..end];

                if vbr {
                    // M-1 explicit frame lengths, contiguous, then the frame
                    // data; the last frame is the remainder.
                    let mut lens = Vec::with_capacity(m.saturating_sub(1));
                    let mut hp = 0usize;
                    for _ in 0..m - 1 {
                        let (l, nb) = read_opus_frame_len(region, hp)?;
                        hp += nb;
                        lens.push(l);
                    }
                    let mut payloads = Vec::with_capacity(m);
                    let mut fp = hp;
                    for &l in &lens {
                        if fp + l > region.len() {
                            return Err("Code 3 VBR: frame length exceeds packet");
                        }
                        payloads.push(&region[fp..fp + l]);
                        fp += l;
                    }
                    if fp > region.len() {
                        return Err("Code 3 VBR: no data for last frame");
                    }
                    payloads.push(&region[fp..]);
                    frame_payloads = payloads;
                } else {
                    // CBR: the region splits into M equal frames (possibly all
                    // empty, e.g. DTX).
                    if region.len() % m != 0 {
                        return Err("Code 3 CBR: frame data not divisible by frame count");
                    }
                    let frame_len = region.len() / m;
                    frame_payloads = (0..m)
                        .map(|i| &region[i * frame_len..(i + 1) * frame_len])
                        .collect();
                }
            }
            _ => unreachable!(),
        }

        self.frame_size = frame_size;
        self.bandwidth = bandwidth;
        self.stream_channels = packet_channels;

        let sub_frame_size = frame_size / frame_count;
        let sub_output_len = sub_frame_size * self.channels;

        match mode {
            OpusMode::SilkOnly => {
                let internal_sample_rate = match bandwidth {
                    Bandwidth::Narrowband => 8000,
                    Bandwidth::Mediumband => 12000,
                    Bandwidth::Wideband => 16000,
                    _ => 16000,
                };
                let internal_frame_size =
                    (frame_duration_ms * internal_sample_rate / 1000) as usize;

                if self.sampling_rate != internal_sample_rate
                    && internal_sample_rate != self.prev_internal_rate
                {
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
                    silk::init_decoder::silk_init_decoder(
                        &mut self.silk_dec.channel_state[1],
                    );
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
                    let mut rc = RangeCoder::new_decoder(payload);
                    let pcm_i16_len = internal_sub_frame_size * self.channels;
                    // A malformed packet can imply a frame larger than our scratch
                    // buffer; reject it gracefully instead of slicing out of bounds
                    // (a decode-path DoS on attacker-controlled input).
                    if pcm_i16_len + 2 > self.w_pcm_i16.len() {
                        return Err("opus: SILK frame size exceeds buffer");
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
                            return Err("SILK decoding failed");
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
                            if self.sampling_rate == internal_sample_rate {
                                for i in 0..decoded_samples {
                                    let l = self.silk_dec.l_out[i] as f32 / 32768.0;
                                    let r = self.silk_dec.r_out[i] as f32 / 32768.0;
                                    let idx = base + i * 2;
                                    if idx + 1 < output.len() {
                                        output[idx] = l;
                                        output[idx + 1] = r;
                                    }
                                }
                                decoded_samples
                            } else {
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
                            }
                        } else if self.sampling_rate == internal_sample_rate {
                            let frames = decoded_samples;
                            for i in 0..frames {
                                let v = self.w_pcm_i16[1 + i] as f32 / 32768.0;
                                for ch in 0..self.channels {
                                    let idx = base + i * self.channels + ch;
                                    if idx < output.len() {
                                        output[idx] = v;
                                    }
                                }
                            }
                            frames
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
                    if self.sampling_rate == 48000 && rc.tell() + 17 <= (plen as i32) * 8 {
                        redundancy = true;
                        celt_to_silk = rc.decode_bit_logp(1);
                        red_bytes = plen - (((rc.tell() + 7) >> 3) as usize);
                        if red_bytes < 2 || red_bytes >= plen {
                            redundancy = false;
                            red_bytes = 0;
                        }
                    }
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
                    if self.sampling_rate == 48000
                        && prev_mode_frame == Some(OpusMode::Hybrid)
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
                    self.prev_redundancy = redundancy && !celt_to_silk;
                    prev_mode_frame = Some(OpusMode::SilkOnly);
                    self.last_range = rc.rng ^ redundant_rng;
                }
                self.prev_mode = Some(OpusMode::SilkOnly);
                Ok(frame_size)
            }

            OpusMode::CeltOnly => {
                let celt_end_band = self.celt_end_band_from_toc(toc);
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
                    let mut rc = RangeCoder::new_decoder(payload);
                    let total_bits = (payload.len() * 8) as i32;
                    let needed = sub_frame_size * self.channels;
                    let out_start = fi * needed;
                    let out_end = (out_start + needed).min(output.len());

                    if output.len() < out_end {
                        return Err("Output buffer too small");
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
                }
                self.prev_mode = Some(OpusMode::CeltOnly);
                Ok(frame_size)
            }

            OpusMode::Hybrid => {
                let internal_sample_rate = 16000;
                let internal_frame_size =
                    (frame_duration_ms * internal_sample_rate / 1000) as usize;
                let celt_end_band = self.celt_end_band_from_toc(toc);

                if self.sampling_rate != internal_sample_rate
                    && internal_sample_rate != self.prev_internal_rate
                {
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
                    let mut rc = RangeCoder::new_decoder(payload);
                    let pcm_silk_i16_len = internal_frame_size * self.channels;
                    if pcm_silk_i16_len + 2 > self.w_pcm_i16.len() {
                        return Err("opus: SILK frame size exceeds buffer");
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
                        return Err("SILK decoding failed");
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
                    let f5 = (self.sampling_rate / 200) as usize;
                    let f2_5 = f5 / 2;
                    let red_end_band = celt_endband_for_bandwidth(bandwidth);
                    let mut red_buf = [0.0f32; 480];
                    let mut redundant_rng = 0u32;
                    let do_red = redundancy && self.sampling_rate == 48000;
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
                    self.prev_redundancy = redundancy && !celt_to_silk;
                    self.last_range = rc.rng ^ redundant_rng;
                }
                self.prev_mode = Some(OpusMode::Hybrid);
                Ok(frame_size)
            }
        }
    }
}

impl OpusDecoder {
    #[inline(always)]
    fn celt_end_band_from_toc(&self, toc: u8) -> usize {
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
        let mut rrc = RangeCoder::new_decoder(red);
        let total_bits = (red.len() * 8) as i32;
        self.celt_dec.decode_from_range_coder_with_band_range(
            &mut rrc, total_bits, f5, buf, 0, end_band,
        );
        rrc.rng
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

/// smooth_fade cross-fades (w = window[i]^2, 48 kHz inc=1) applied to the
/// interleaved output region of one frame. `red` is PLANAR (F5 per channel).
/// celt_to_silk: redundant frame occupies the START of the frame — first 2.5 ms
/// copied verbatim, next 2.5 ms fades redundant -> main.
fn redundancy_fade_start(
    out: &mut [f32],
    red: &[f32],
    f5: usize,
    f2_5: usize,
    channels: usize,
    window: &[f32],
) {
    for i in 0..f2_5 {
        for c in 0..channels {
            out[i * channels + c] = red[c * f5 + i];
        }
    }
    for i in 0..f2_5 {
        let w = window[i] * window[i];
        for c in 0..channels {
            let idx = (f2_5 + i) * channels + c;
            out[idx] = (1.0 - w) * red[c * f5 + f2_5 + i] + w * out[idx];
        }
    }
}

/// SILK->CELT: redundant frame occupies the END of the frame — the last 2.5 ms
/// fades main -> redundant (second half of the redundant frame).
fn redundancy_fade_end(
    out: &mut [f32],
    frame_samples: usize,
    red: &[f32],
    f5: usize,
    f2_5: usize,
    channels: usize,
    window: &[f32],
) {
    for i in 0..f2_5 {
        let w = window[i] * window[i];
        for c in 0..channels {
            let idx = (frame_samples - f2_5 + i) * channels + c;
            out[idx] = (1.0 - w) * out[idx] + w * red[c * f5 + f2_5 + i];
        }
    }
}

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
            let period_offset = if frame_rate >= 100 { 0 } else { 1 };
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
fn read_opus_frame_len(data: &[u8], ptr: usize) -> Result<(usize, usize), &'static str> {
    let b0 = *data.get(ptr).ok_or("Opus frame length: truncated")? as usize;
    if b0 < 252 {
        Ok((b0, 1))
    } else {
        let b1 = *data.get(ptr + 1).ok_or("Opus frame length: truncated 2-byte")? as usize;
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
                "frame_size {} expected TOC {:02X} got {:02X}",
                frame_size, expected_toc, toc
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
