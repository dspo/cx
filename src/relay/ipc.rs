// IPC server: a Unix socket per session that external processes connect to
// in order to inject messages into the running agent.
//
// The socket lives at `~/.config/cx/sessions/<id>.sock` and a sibling `<id>.json`
// registry file advertises it to `cx send`. Stale sockets from crashed sessions
// are reclaimed by a connect-probe before bind.

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};

use crate::relay::transfer::WriteReq;
use crate::session;

pub(crate) struct IpcServer {
    listener: UnixListener,
}

impl IpcServer {
    /// Bind the session socket at `sock`, reclaiming it if it belongs to a dead session.
    ///
    /// `sock` is fully resolved by the caller (either the default
    /// `~/.config/cx/sessions/<id>.sock` or a `--socket` override), so this fn is
    /// agnostic to the path's origin.
    pub(crate) fn bind(sock: &Path) -> Result<Self> {
        if let Some(parent) = sock.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建 sessions 目录失败: {}", parent.display()))?;
        }
        if sock.exists() {
            if session::socket_alive(sock) {
                anyhow::bail!("session socket 已被活跃会话占用: {}", sock.display());
            }
            let _ = std::fs::remove_file(sock);
        }
        let listener = UnixListener::bind(sock)
            .with_context(|| format!("bind IPC socket 失败: {}", sock.display()))?;
        Ok(Self { listener })
    }

    /// Spawn the accept loop: each connection is handled on its own thread.
    pub(crate) fn accept_loop(self, tx: mpsc::Sender<WriteReq>) {
        thread::spawn(move || {
            for stream in self.listener.incoming() {
                let Ok(stream) = stream else { continue };
                crate::relay::transfer::forward_ipc_stream(stream, tx.clone());
            }
        });
    }
}
