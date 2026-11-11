//! BitTorrent downloads through librqbit. The session (DHT, listener) only starts
//! when the first torrent does, so people who never use torrents pay nothing for it.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session, SessionOptions};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::OnceCell;

use super::Ev;

#[derive(Clone, Default)]
pub struct Lazy(Arc<OnceCell<Arc<Session>>>);

impl Lazy {
    pub fn get(&self) -> Option<Arc<Session>> {
        self.0.get().cloned()
    }

    async fn get_or_start(&self, limit_bps: u64) -> Result<Arc<Session>, String> {
        self.0
            .get_or_try_init(|| async {
                let state = super::data_dir().join("torrents");
                let _ = std::fs::create_dir_all(&state);
                let opts = SessionOptions {
                    dht: Some(librqbit::DhtSessionConfig {
                        persistence: Some(librqbit::dht::DhtPersistenceConfig {
                            config_filename: Some(state.join("dht.json")),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    listen: Some(librqbit::ListenerOptions { enable_upnp_port_forwarding: true, ..Default::default() }),
                    client_name_and_version: Some(concat!("Stash ", env!("CARGO_PKG_VERSION")).into()),
                    ..Default::default()
                };
                let s = Session::new_with_opts(crate::shell::downloads_dir(), opts).await.map_err(|e| format!("Can't start BitTorrent: {e:#}"))?;
                set_limit(&s, limit_bps);
                Ok(s)
            })
            .await
            .cloned()
    }
}

pub fn set_limit(s: &Session, bps: u64) {
    s.ratelimits.set_download_bps(NonZeroU32::new(bps.min(u32::MAX as u64) as u32));
}

pub struct Task {
    pub id: u64,
    pub source: String,
    pub dir: PathBuf,
    /// The .torrent saved on an earlier run: re-adding it checks the files on disk and resumes.
    pub torrent_file: Option<PathBuf>,
    pub limit_bps: u64,
    pub session: Lazy,
    pub client: reqwest::Client,
    pub events: UnboundedSender<Ev>,
}

impl Task {
    pub async fn run(self) {
        let (id, events) = (self.id, self.events.clone());
        if let Err(e) = self.go().await {
            let _ = events.send(Ev::Failed { id, error: e });
        }
    }

    async fn go(self) -> Result<(), String> {
        let session = self.session.get_or_start(self.limit_bps).await?;

        // 1. The torrent's metadata: from the saved file, a .torrent (local or on the web),
        //    or from peers for a magnet link.
        let bytes = match &self.torrent_file {
            Some(f) => std::fs::read(f).map_err(|e| e.to_string())?,
            None => self.fetch().await?,
        };
        let listed = match &bytes[..] {
            [] => {
                let opts = AddTorrentOptions { list_only: true, ..Default::default() };
                session.add_torrent(AddTorrent::from_url(self.source.clone()), Some(opts)).await
            }
            b => {
                let opts = AddTorrentOptions { list_only: true, ..Default::default() };
                session.add_torrent(AddTorrent::from_bytes(b.to_vec()), Some(opts)).await
            }
        }
        .map_err(|e| format!("{e:#}"))?;
        let AddTorrentResponse::ListOnly(list) = listed else { return Err("Unexpected answer from BitTorrent".into()) };

        let info_hash = list.info_hash.as_string();
        let name = list.info.name().map(|n| n.into_owned()).unwrap_or_else(|| info_hash.clone());
        let name = name.replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "_");
        let total = list.info.lengths().total_length();
        let multi = list.info.info().files.is_some();
        let output = if multi { self.dir.join(&name) } else { self.dir.clone() };
        let path = if multi { output.clone() } else { self.dir.join(&name) };

        let saved = super::data_dir().join("torrents").join(format!("{info_hash}.torrent"));
        if !saved.exists() {
            let _ = std::fs::write(&saved, &list.torrent_bytes);
        }
        let _ = self.events.send(Ev::TorrentMeta { id: self.id, name, total, info_hash, torrent_file: saved, path });

        // 2. Download into the chosen folder. `overwrite` lets it pick up files it already
        //    has (after a restart), checking every piece first.
        let opts = AddTorrentOptions {
            overwrite: true,
            output_folder: Some(output.to_string_lossy().into_owned()),
            initial_peers: Some(list.seen_peers.clone()),
            ..Default::default()
        };
        let added = session.add_torrent(AddTorrent::from_bytes(list.torrent_bytes.clone()), Some(opts)).await.map_err(|e| format!("{e:#}"))?;
        let handle = match added {
            AddTorrentResponse::Added(_, h) | AddTorrentResponse::AlreadyManaged(_, h) => h,
            AddTorrentResponse::ListOnly(_) => return Err("Unexpected answer from BitTorrent".into()),
        };
        if handle.is_paused() {
            let _ = session.unpause(&handle).await;
        }
        let _ = self.events.send(Ev::TorrentAdded { id: self.id, handle });
        Ok(())
    }

    /// A .torrent's bytes; empty for a magnet link (the session resolves those).
    async fn fetch(&self) -> Result<Vec<u8>, String> {
        let s = self.source.trim();
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("magnet:") {
            Ok(Vec::new())
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            let r = self.client.get(s).send().await.map_err(|e| e.to_string())?;
            if !r.status().is_success() {
                return Err(format!("The server answered {}", r.status()));
            }
            Ok(r.bytes().await.map_err(|e| e.to_string())?.to_vec())
        } else {
            std::fs::read(s).map_err(|e| format!("Can't read the torrent file: {e}"))
        }
    }
}
