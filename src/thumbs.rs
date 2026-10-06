//! Shell thumbnails are fetched on a background thread; results are handed to the
//! UI thread through `deliver`.

use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::time::SystemTime;

use crate::gfx::Pixels;

pub struct Job {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub size: i32,
}

pub struct Done {
    pub path: PathBuf,
    pub modified: SystemTime,
    pub pixels: Option<Pixels>,
}

pub fn spawn_worker(deliver: impl Fn(Done) + Send + 'static) -> Sender<Job> {
    let (tx, rx) = channel::<Job>();
    std::thread::Builder::new()
        .name("stash-thumbs".into())
        .spawn(move || {
            crate::shell::init_com_thread();
            while let Ok(job) = rx.recv() {
                let pixels = crate::shell::thumbnail(&job.path, job.size);
                deliver(Done { path: job.path, modified: job.modified, pixels });
            }
        })
        .expect("thumbnail thread");
    tx
}
