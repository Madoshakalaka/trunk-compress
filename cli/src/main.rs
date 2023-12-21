use std::{
    iter::FilterMap,
    path::{Path, PathBuf},
};

use async_compression::tokio::write::BrotliEncoder;
use clap::Parser;
use tokio::{
    self,
    fs::File,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tracing::{info, warn, Level};
use tracing_subscriber::{self, FmtSubscriber};
use walkdir::{DirEntry, IntoIter, WalkDir};

const FRONTEND_DIR: &str = r"../frontend/dist/";

fn identity_files() -> FilterMap<IntoIter, fn(walkdir::Result<DirEntry>) -> Option<DirEntry>> {
    WalkDir::new(Path::new(FRONTEND_DIR).join("identity"))
        .follow_links(true)
        .into_iter()
        .filter_map(|e| match e {
            Ok(entry) => {
                if entry.path().is_file() {
                    Some(entry)
                } else {
                    None
                }
            }
            Err(_) => None,
        })
}

struct YewCompressor {
    g: GenerationSpec,
}

impl YewCompressor {
    fn new(g: GenerationSpec) -> Self {
        Self { g }
    }

    async fn compress(&self) {
        let mid_diff = {
            let file = self.g.identity_file.clone();
            let file = file.into_path();

            let parent = file.parent().unwrap();

            pathdiff::diff_paths(parent, Path::new(FRONTEND_DIR).join("identity")).unwrap()
        };

        tokio::fs::create_dir_all(
            Path::new(FRONTEND_DIR)
                .join("brotli")
                .join(mid_diff.clone()),
        )
        .await
        .ok();

        let target = self.g.to_target();

        info!("outputing target {:?}", target);

        let output = File::create(target).await.unwrap();

        let mut buf: Vec<u8> = Vec::new();
        File::open(self.g.identity_file.path())
            .await
            .unwrap()
            .read_to_end(&mut buf)
            .await
            .unwrap();

        let slice = buf.as_slice();

        let mut encoder = BrotliEncoder::new(output);
        encoder.write_all(slice).await.unwrap();
        encoder.shutdown().await.unwrap();
    }
}

struct GenerationSpec {
    identity_file: DirEntry,
    hash: Option<String>,
}

impl GenerationSpec {
    fn to_target(&self) -> PathBuf {
        let identity_file_path = self.identity_file.path();
        let mid_diff = {
            let parent = identity_file_path.parent().unwrap();

            pathdiff::diff_paths(parent, Path::new(FRONTEND_DIR).join("identity")).unwrap()
        };
        match self.hash {
            Some(ref hash) => {
                let (name, suffix) = {
                    let split = identity_file_path
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .split('.')
                        .collect::<Vec<_>>();

                    let suffix = (*split.last().unwrap()).to_owned();

                    (split[..split.len() - 1].join(""), suffix)
                };

                Path::new(FRONTEND_DIR)
                    .join("brotli")
                    .join(mid_diff)
                    .join(format!("{}-{}.{}.br", name, hash, suffix,))
            }
            None => Path::new(FRONTEND_DIR)
                .join("brotli")
                .join(mid_diff)
                .join(format!(
                    "{}.br",
                    identity_file_path.file_name().unwrap().to_string_lossy(),
                )),
        }
    }
}

async fn compress() {
    let identity_dir = Path::new(FRONTEND_DIR).join("identity");
    if !identity_dir.exists() {
        info!("identity directory does not exist, creating");
        tokio::fs::create_dir_all(identity_dir).await.ok();
    }
    let brotli_dir = Path::new(FRONTEND_DIR).join("brotli");
    if !brotli_dir.exists() {
        info!("brotli directory does not exist, creating");
        tokio::fs::create_dir_all(brotli_dir).await.ok();
    }
    // todo: use the above variables consistently across the codebase

    let identity_files: Vec<DirEntry> = identity_files().collect();

    let compression_dir = Path::new(FRONTEND_DIR).join("brotli");

    let mut to_be_generated: Vec<GenerationSpec> = identity_files
        .into_iter()
        .filter_map(|e| {
            // ignore compressed images, audio, and video
            let filename = e.file_name().to_str().unwrap();

            if filename.ends_with(".png")
                || filename.ends_with(".jpg")
                || filename.ends_with(".jpeg")
                || filename.ends_with(".gif")
                || filename.ends_with(".ico")
                || filename.ends_with(".mp3")
                || filename.ends_with(".mp4")
                || filename.ends_with(".webm")
                || filename.ends_with(".ogg")
                || filename.ends_with(".wav")
                || filename == "index.html"
            {
                return None;
            }

            Some(GenerationSpec {
                hash: if e
                    .path()
                    .starts_with(Path::new(FRONTEND_DIR).join("identity").join("assets"))
                {
                    Some(format!(
                        "{:?}",
                        md5::compute(std::fs::read(e.path()).unwrap())
                    ))
                } else {
                    None
                },
                identity_file: e,
            })
        })
        .collect();

    let mut deleting_dirs: Vec<PathBuf> = Vec::new();
    let mut deleting_files: Vec<PathBuf> = Vec::new();

    for old_entry in WalkDir::new(&compression_dir)
        .into_iter()
        .filter_map(Result::ok)
    {
        if old_entry.path().is_dir() {
            let corresponding_identity = Path::new(FRONTEND_DIR).join("identity").join(
                old_entry
                    .path()
                    .strip_prefix(&compression_dir)
                    .unwrap()
                    .to_str()
                    .unwrap(),
            );

            if !corresponding_identity.exists() || !corresponding_identity.is_dir() {
                info!("removing outdated directory {:?}", old_entry.path());
                deleting_dirs.push(old_entry.path().to_owned());
            }

            continue;
        }

        let filename = old_entry.file_name().to_str().unwrap();

        let Some(stripped) = filename.strip_suffix(".br") else {
            warn!("file {:?} does not have a .br suffix", old_entry.path());
            continue;
        };

        let corresponding_identity = if old_entry
            .path()
            .starts_with(Path::new(FRONTEND_DIR).join("brotli").join("assets"))
        {
            // each compressed asset looks like name-hash.suffix.br
            // aside from the .br suffix, we also need to strip the hash

            let original_suffix = Path::new(stripped)
                .extension()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();

            let hash = stripped
                .split('-')
                .last()
                .unwrap()
                .split('.')
                .next()
                .unwrap();

            let split = stripped.split('-').collect::<Vec<_>>();

            if split.len() == 1 {
                warn!(
                    "file {:?} does not have a hash suffix, skipping",
                    old_entry.path()
                );
                continue;
            }

            let mid_diff = {
                pathdiff::diff_paths(
                    old_entry.path().parent().unwrap(),
                    Path::new(FRONTEND_DIR).join("brotli"),
                )
                .unwrap()
            };

            let corresponding_identity = Path::new(FRONTEND_DIR)
                .join("identity")
                .join(mid_diff)
                .join(format!(
                    "{}.{original_suffix}",
                    split[..split.len() - 1].join("")
                ));

            if corresponding_identity.is_file()
                && format!(
                    "{:?}",
                    md5::compute(std::fs::read(corresponding_identity.clone()).unwrap())
                ) == hash
            {
                Some(corresponding_identity)
            } else {
                info!(
                    "removing outdated asset {:?} because of hash mismatch",
                    old_entry.path()
                );
                deleting_files.push(old_entry.path().to_owned());
                None
            }
        } else {
            let identity_file = Path::new(FRONTEND_DIR).join("identity").join(stripped);
            if identity_file.is_file() {
                Some(identity_file)
            } else {
                info!(
                    "removing outdated file {:?} because can't find identity file",
                    old_entry.path()
                );
                deleting_files.push(old_entry.path().to_owned());
                None
            }
        };

        if let Some(corresponding_identity) = corresponding_identity {
            // remove from to_be_generated
            to_be_generated.retain(|e| e.identity_file.path() != corresponding_identity.as_path());
        };
    }

    for dir in deleting_dirs {
        std::fs::remove_dir_all(dir).ok();
    }
    for file in deleting_files {
        std::fs::remove_file(file).ok();
    }

    // compress the to_be_generated files

    for f in to_be_generated {
        let file_name = f.identity_file.file_name().to_str().unwrap().to_owned();
        YewCompressor::new(f).compress().await;
        info!("Done compressing {file_name}");
    }
}

/// compress the files in the identity directory
#[derive(Parser, Debug)]
#[command(author="Mattsy", version, about, long_about = None)]
struct Cli {}

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::TRACE)
        .with_ansi(false)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_err| eprintln!("Unable to set global default subscriber"))
        .unwrap();

    let _ = Cli::parse();

    compress().await;
}
