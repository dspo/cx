// PTY relay: cx owns the master, the slave becomes the agent's controlling
// terminal, and the user terminal is bridged to it byte-for-byte. A Unix socket
// per session lets external processes inject messages (see `cx send`).
//
// This module owns the full lifecycle for the PTY launch path and returns `!`:
// spawn → raw mode → relay → EOF → wait → finalize (summary + warp stop +
// IPC cleanup) → process::exit.

pub(crate) mod ipc;
pub(crate) mod pty;
pub(crate) mod transfer;

use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::{Instant, SystemTime};

use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use portable_pty::ExitStatus as PtyExitStatus;
use signal_hook::consts::signal::SIGWINCH;
use signal_hook::flag as sig_flag;

use crate::LaunchSpec;
use crate::session::{self, SessionRegistry};
use crate::warp::WarpSession;

use pty::{PtySession, spawn_pty};
use transfer::{WriteReq, master_to_stdout, stdin_forward, writer_loop};

/// Run the agent through a PTY relay, accepting IPC injections.
///
/// Owns the lifecycle and returns `!` via `finalize_exit_common`.
pub(crate) fn run(spec: &LaunchSpec, warp_session: &Option<WarpSession>) -> ! {
    let session_id = session::generate_session_id();
    let started_at = Instant::now();
    let started_sys = SystemTime::now();

    // Launch banner in cooked mode before raw mode swallows the newlines. The socket
    // line is appended once the IPC bind succeeds (below), so only a live socket is
    // advertised; a trailing blank separates the banner from the agent's raw output.
    println!();
    println!("{}", spec.summary);
    let _ = std::io::stdout().flush();

    let pty = match spawn_pty(spec, &warp_env(warp_session)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cx: {e:#}");
            session::cleanup_session(&session_id);
            std::process::exit(1);
        }
    };
    let PtySession {
        master,
        mut child,
        reader,
        writer,
    } = pty;

    // IPC is best-effort: the relay still works without external injection.
    //
    // The socket path is a `--socket` override when given, else the default
    // ~/.config/cx/sessions/<id>.sock. The registry advertises whichever is bound,
    // so `cx send` finds the session regardless of which path was used.
    let (tx, rx) = mpsc::channel::<WriteReq>();
    let sock_path = match spec.socket.as_deref() {
        Some(p) => Ok(std::path::PathBuf::from(p)),
        None => session::socket_path(&session_id),
    };
    match sock_path {
        Ok(sock) => match ipc::IpcServer::bind(&sock) {
            Ok(ipc) => {
                ipc.accept_loop(tx.clone());
                let reg = SessionRegistry {
                    id: session_id.clone(),
                    socket: sock.to_string_lossy().into_owned(),
                    pid: std::process::id(),
                    agent: spec.agent_id.clone(),
                    model: spec.model_id.clone(),
                    provider: spec.provider_name.clone(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    cwd: std::env::current_dir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                };
                let _ = session::write_registry(&reg);
                // Advertise the injection socket while still in cooked mode, before raw
                // mode swallows the output. External processes (e.g. `cx send`) connect
                // here to inject messages as if the user typed them.
                println!("socket: {}", sock.display());
                let _ = std::io::stdout().flush();
            }
            Err(e) => eprintln!("cx: IPC 绑定失败（外部注入不可用）: {e}"),
        },
        Err(e) => eprintln!("cx: 解析 socket 路径失败（外部注入不可用）: {e}"),
    }

    // Blank separator between the banner and the agent's raw output, flushed so it
    // lands before raw mode takes over.
    println!();
    let _ = std::io::stdout().flush();

    // Enter raw mode so keystrokes pass through verbatim (Ctrl+C = 0x03, not SIGINT).
    if let Err(e) = enable_raw_mode() {
        eprintln!("cx: 进入 raw mode 失败: {e}");
        let _ = child.kill();
        let status = child
            .wait()
            .unwrap_or_else(|_| PtyExitStatus::with_exit_code(1));
        finalize_relay(
            spec,
            &status,
            started_at,
            started_sys,
            warp_session,
            &session_id,
        );
    }
    let _raw_guard = RawGuard;

    // SIGWINCH → resize flag, polled by the writer loop.
    let resize_flag = Arc::new(AtomicBool::new(false));
    if let Err(e) = sig_flag::register(SIGWINCH, Arc::clone(&resize_flag)) {
        eprintln!("cx: 注册 SIGWINCH 失败（窗口缩放将不生效）: {e}");
    }

    stdin_forward(tx);
    writer_loop(writer, rx, master, resize_flag);

    // Main thread blocks here until the agent closes its stdout (EOF = exit).
    if let Err(e) = master_to_stdout(reader) {
        eprintln!("cx: master 读取失败: {e}");
    }

    let status = child
        .wait()
        .unwrap_or_else(|_| PtyExitStatus::with_exit_code(1));
    let _ = disable_raw_mode();
    finalize_relay(
        spec,
        &status,
        started_at,
        started_sys,
        warp_session,
        &session_id,
    );
}

/// Build the relay-time env slice (currently just the Warp session id, if any).
fn warp_env(warp_session: &Option<WarpSession>) -> Vec<(&'static str, String)> {
    match warp_session {
        Some(ws) => vec![("CX_WARP_SESSION_ID", ws.session_id().to_string())],
        None => Vec::new(),
    }
}

/// Restore the terminal on panic; the normal path disables raw mode explicitly
/// before finalizing. `disable_raw_mode` is idempotent, so a double call is harmless.
struct RawGuard;
impl Drop for RawGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

/// Map a `portable_pty::ExitStatus` to a shell-style exit code.
///
/// `portable_pty` collapses signal deaths to `exit_code()==1` and keeps only the
/// `strsignal` description (e.g. "Terminated"), so the 128+signal convention is
/// best-effort; unmapped signals fall back to the reported exit code.
fn pty_exit_code(status: &PtyExitStatus) -> i32 {
    if status.success() {
        return 0;
    }
    if let Some(sig) = status.signal() {
        if let Some(n) = signal_number(sig) {
            return 128 + n;
        }
    }
    status.exit_code() as i32
}

/// Best-effort `strsignal` description → signal number for the common cases.
fn signal_number(desc: &str) -> Option<i32> {
    // strsignal returns localized descriptions; match the C-locale strings.
    match desc {
        "Hangup" => Some(libc::SIGHUP),
        "Interrupt" => Some(libc::SIGINT),
        "Quit" => Some(libc::SIGQUIT),
        "Killed" => Some(libc::SIGKILL),
        "Segmentation fault" | "Segmentation Fault" => Some(libc::SIGSEGV),
        "Terminated" => Some(libc::SIGTERM),
        _ => None,
    }
}

/// Relay-side exit finalize: compute exit code/termination, then delegate to the
/// shared `finalize_exit_common` which prints the summary, emits Warp stop,
/// cleans up the IPC session, and `process::exit`s.
fn finalize_relay(
    spec: &LaunchSpec,
    status: &PtyExitStatus,
    started_at: Instant,
    started_sys: SystemTime,
    warp_session: &Option<WarpSession>,
    session_id: &str,
) -> ! {
    let exit_code = pty_exit_code(status);
    let termination: Option<String> = if status.success() && exit_code == 0 {
        None
    } else {
        Some(status.to_string())
    };
    crate::finalize_exit_common(
        &spec.agent_id,
        &spec.provider_name,
        spec.model_id.as_deref(),
        started_at,
        started_sys,
        exit_code,
        termination.as_deref(),
        warp_session,
        Some(session_id),
    )
}
