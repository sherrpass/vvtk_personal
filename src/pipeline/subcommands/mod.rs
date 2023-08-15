pub mod convert;
pub mod dash;
pub mod downsample;
pub mod info;
pub mod metrics;
pub mod read;
pub mod render;
pub mod upsample;
pub mod write;
pub mod normal_estimation;

pub use convert::Convert;
pub use dash::Dash;
pub use downsample::Downsampler;
pub use info::Info;
pub use metrics::MetricsCalculator;
pub use read::Read;
pub use render::Render;
pub use upsample::Upsampler;
pub use write::Write;
pub use normal_estimation::NormalEstimation;

use super::{channel::Channel, PipelineMessage};

pub trait Subcommand {
    fn handle(&mut self, messages: Vec<PipelineMessage>, out: &Channel);
}
