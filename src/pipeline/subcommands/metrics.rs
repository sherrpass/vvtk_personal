use std::{
    ffi::OsString,
    fs::File,
    io::{BufWriter},
    path::{Path, PathBuf},
};

use clap::Parser;

use crate::{
    metrics::calculate_metrics,
    pipeline::{PipelineMessage, Progress},
    utils::{find_all_files, read_file_to_point_cloud},
};

use super::Subcommand;

#[derive(Parser)]
struct Args {
    #[clap(short, long)]
    reference: Vec<OsString>,

    #[clap(short, long)]
    output_dir: OsString,
}

pub struct Metrics {
    files: Vec<PathBuf>,
    output_path: OsString,
    count: usize,
}

impl Metrics {
    pub fn from_args(args: Vec<String>) -> Box<dyn Subcommand> {
        let args: Args = Args::parse_from(args);
        std::fs::create_dir_all(Path::new(&args.output_dir))
            .expect("Failed to create output directory");
        let mut files = find_all_files(&args.reference);
        files.sort();
        Box::new(Metrics {
            files,
            count: 0,
            output_path: args.output_dir,
        })
    }
}

impl Subcommand for Metrics {
    fn handle(
        &mut self,
        message: crate::pipeline::PipelineMessage,
        out: &std::sync::mpsc::Sender<crate::pipeline::PipelineMessage>,
        progress: &std::sync::mpsc::Sender<crate::pipeline::Progress>,
    ) {
        match &message {
            PipelineMessage::PointCloud(pc) => {
                let original = read_file_to_point_cloud(&self.files[self.count]).expect(&format!(
                    "Failed to read file {:?}",
                    self.files[self.count].as_os_str()
                ));
                let metrics = calculate_metrics(&original, &pc);
                let output_path = Path::new(&self.output_path);
                let file_name = format!("{}.metrics", self.count);
                self.count += 1;
                let file_name = Path::new(&file_name);
                let output_file = output_path.join(file_name);
                let file = File::create(&output_file).expect(&format!(
                    "Failed to open file {:?}",
                    output_file.as_os_str()
                ));
                let mut writer = BufWriter::new(file);
                metrics.write_to(&mut writer)
                    .expect("should be able to write");

                progress.send(Progress::Incr)
                    .expect("should be able to send");
            }
            PipelineMessage::End => {
                progress.send(Progress::Completed)
                    .expect("should be able to send")
            }
        }
        out.send(message)
            .expect("should be able to send")
    }
}