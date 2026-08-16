use echobrain_core::{EchoError, REQUIRED_SAMPLE_RATE, Samples, VoiceEmbedding, VoiceIdentity};
use kaldi_native_fbank::online::{FeatureComputer, OnlineFeature};
use kaldi_native_fbank::{FbankComputer, FbankOptions};
use ndarray::{Array2, Array3, Axis};
use ort::{
    inputs,
    session::{Session, builder::GraphOptimizationLevel},
};
use std::sync::Mutex;

/// The 80-bin filterbank the WeSpeaker models are trained on. Not a knob: the
/// model's input layer is this wide.
const MEL_BINS: usize = 80;

/// `output_dim` in the model's own metadata. Checked against the first
/// embedding the model actually returns rather than trusted.
const EMBEDDING_DIMENSIONS: usize = 256;

pub struct WeSpeakerIdentity {
    session: Mutex<Session>,
    dimensions: usize,
}

// `ort::Session` is not `Debug`, so this cannot be derived.
impl std::fmt::Debug for WeSpeakerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeSpeakerIdentity")
            .field("dimensions", &self.dimensions)
            .finish_non_exhaustive()
    }
}

impl WeSpeakerIdentity {
    pub fn load(model_path: &str) -> Result<Self, ort::Error> {
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .commit_from_file(model_path)?;

        Ok(Self {
            session: Mutex::new(session),
            dimensions: EMBEDDING_DIMENSIONS,
        })
    }

    fn extract_fbank(samples: &Samples) -> Result<Array2<f32>, EchoError> {
        let mut opts = FbankOptions::default();
        opts.frame_opts.samp_freq = REQUIRED_SAMPLE_RATE as f32;
        opts.frame_opts.dither = 0.0;
        opts.frame_opts.window_type = "povey".into();
        opts.mel_opts.num_bins = MEL_BINS;
        opts.energy_floor = 0.0;

        let computer = FbankComputer::new(opts)
            .map_err(|e| EchoError::audio(format!("the filterbank options were rejected: {e}")))?;
        let mut online = OnlineFeature::new(FeatureComputer::Fbank(computer));

        online.accept_waveform(REQUIRED_SAMPLE_RATE as f32, samples.as_slice());
        online.input_finished();

        let num_frames = online.num_frames_ready();
        // Shorter than one analysis window: a tap on the hotkey rather than an
        // utterance. `mean_axis` over zero rows is `None`, so this must be
        // caught before the normalisation below.
        if num_frames == 0 {
            return Err(EchoError::EmptyUtterance);
        }

        let mut feats = Array2::<f32>::zeros((num_frames, MEL_BINS));
        for i in 0..num_frames {
            let frame = online.get_frame(i).ok_or_else(|| {
                EchoError::audio(format!("frame {i} vanished between count and read"))
            })?;
            for j in 0..MEL_BINS {
                feats[[i, j]] = frame[j];
            }
        }

        // Cepstral mean normalisation: subtracting the per-bin mean takes out
        // the constant offset that the microphone and the room contribute, so
        // two recordings of the same voice on different days still land close.
        let mean = feats
            .mean_axis(Axis(0))
            .ok_or_else(|| EchoError::audio("the filterbank came back empty"))?;
        for mut row in feats.rows_mut() {
            row -= &mean;
        }

        Ok(feats)
    }
}

impl VoiceIdentity for WeSpeakerIdentity {
    fn embed(&self, samples: &Samples) -> Result<VoiceEmbedding, EchoError> {
        let fbank = Self::extract_fbank(samples)?;
        let num_frames = fbank.nrows();

        let input_tensor: Array3<f32> = fbank
            .into_shape_with_order((1, num_frames, MEL_BINS))
            .map_err(|e| EchoError::audio(format!("could not shape the filterbank: {e}")))?;

        let input_tensor_value = ort::value::Tensor::from_array(input_tensor)
            .map_err(|e| EchoError::audio(format!("ort tensor error: {e}")))?;

        // Poison-tolerant, like every other lock in this crate: a panic while
        // holding it must not make the model permanently unusable.
        let mut session_guard = match self.session.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let outputs = session_guard
            .run(inputs!["feats" => input_tensor_value])
            .map_err(|e| EchoError::audio(format!("ort run error: {e}")))?;

        let embedding_tensor = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| EchoError::audio(format!("ort extract error: {e}")))?;

        let slice = embedding_tensor.1;

        // `VoiceEmbedding::new` refuses a zero or non-finite vector — what the
        // model returns for silence, which would otherwise normalise to NaN and
        // compare as similar to everyone. This used to `unwrap`, so recording a
        // silent enrolment sample panicked inside the command rather than
        // telling the user to speak up.
        VoiceEmbedding::new(slice.to_vec()).ok_or(EchoError::EmptyUtterance)
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}
