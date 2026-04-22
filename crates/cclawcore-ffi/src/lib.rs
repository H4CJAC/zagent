//! C ABI for embedding the CclawCore daemon.
//!
//! Exposes two entry points:
//!   * [`cclawcore_start`] — bring the daemon up and block until the HTTP
//!     gateway has actually bound its listener (or failed).
//!   * [`cclawcore_stop`]  — cancel the daemon and wait for a clean shutdown.
//!
//! A single process-wide `tracing` subscriber is installed on first entry so
//! that all underlying `tracing::info!/warn!/error!` events reach logcat
//! (Android) or stderr (other Unix targets).

use std::ffi::CStr;
use std::os::raw::c_char;
use std::panic::AssertUnwindSafe;
use std::sync::{Mutex, Once};

use tokio::runtime::Runtime;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use cclawcore::gateway::GatewayReadySignal;

/// How long the synchronous part of [`cclawcore_start`] blocks waiting for
/// the gateway to finish binding before returning `-2`.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Grace period for the daemon task to observe cancellation during a
/// failure-path teardown before we force the runtime down.
const TEARDOWN_WAIT: Duration = Duration::from_secs(3);

struct DaemonHandle {
    runtime: Runtime,
    cancel: CancellationToken,
    join: JoinHandle<anyhow::Result<()>>,
}

static DAEMON: Mutex<Option<DaemonHandle>> = Mutex::new(None);
static SUBSCRIBER_INIT: Once = Once::new();

/// Install a global `tracing` subscriber exactly once per process.
///
/// - Android: `logcat` via `tracing-android` (tag `"cclawcore"`) plus a
///   stderr fmt layer as a no-cost fallback for Termux-style usage.
/// - Other targets: stderr fmt layer only.
///
/// Filtering reads `RUST_LOG`, falling back to
/// `info,cclawcorelabs=debug,cclawcore_ffi=debug`.
fn install_subscriber() {
    SUBSCRIBER_INIT.call_once(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,cclawcorelabs=debug,cclawcore_ffi=debug")
        });
        let stderr_layer = fmt::layer().with_writer(std::io::stderr).with_ansi(false);
        let registry = tracing_subscriber::registry()
            .with(filter)
            .with(stderr_layer);

        #[cfg(target_os = "android")]
        {
            match tracing_android::layer("cclawcore") {
                Ok(android_layer) => {
                    let _ = registry.with(android_layer).try_init();
                }
                Err(e) => {
                    let _ = registry.try_init();
                    eprintln!("cclawcore-ffi: failed to install logcat layer: {e:?}");
                }
            }
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = registry.try_init();
        }
    });
}

/// Start the CclawCore daemon in a background tokio runtime.
///
/// Blocks the calling thread for up to ~30 s while the daemon initializes
/// and the gateway binds its listener. Callers on platforms with an ANR
/// watchdog (Android) MUST invoke this from a background thread/coroutine.
///
/// # Parameters
/// - `config_dir` — UTF-8 path to the configuration directory (must not be NULL).
/// - `host` — bind address (UTF-8). Pass NULL to use the value from config.
/// - `port` — gateway port. Pass 0 to use the value from config.
/// - `sw_preset` — if `true`, activate the built-in Seewo preset.
///
/// # Returns
/// - `0`  gateway is bound and accepting connections.
/// - `1`  daemon is already running in this process.
/// - `-1` invalid argument / tokio runtime creation failed / internal panic.
/// - `-2` initialization or gateway bind failed (see logcat / stderr).
#[unsafe(no_mangle)]
pub extern "C" fn cclawcore_start(
    config_dir: *const c_char,
    host: *const c_char,
    port: u16,
    sw_preset: bool,
) -> i32 {
    install_subscriber();
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        start_inner(config_dir, host, port, sw_preset)
    })) {
        Ok(code) => code,
        Err(_) => {
            tracing::error!("cclawcore_start: panicked in FFI entry");
            -1
        }
    }
}

fn start_inner(
    config_dir: *const c_char,
    host: *const c_char,
    port: u16,
    sw_preset: bool,
) -> i32 {
    let mut guard = DAEMON
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if guard.is_some() {
        tracing::warn!("cclawcore_start called but daemon is already running");
        return 1;
    }

    let config_dir_str = match cstr_to_owned(config_dir, "config_dir") {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::error!("cclawcore_start: config_dir must not be NULL");
            return -1;
        }
        Err(code) => return code,
    };

    let host_override = match cstr_to_owned(host, "host") {
        Ok(value) => value,
        Err(code) => return code,
    };

    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cclawcore_start: failed to create tokio runtime: {e}");
            return -1;
        }
    };

    let cancel = CancellationToken::new();
    let (ready_tx, ready_rx) = oneshot::channel::<GatewayReadySignal>();

    let join = runtime.spawn(daemon_lifecycle(
        config_dir_str,
        host_override,
        port,
        sw_preset,
        cancel.clone(),
        ready_tx,
    ));

    let outcome = runtime.block_on(async move { tokio::time::timeout(READY_TIMEOUT, ready_rx).await });

    match outcome {
        Ok(Ok(Ok(actual_port))) => {
            tracing::info!("cclawcore-ffi: gateway ready on port {actual_port}");
            *guard = Some(DaemonHandle {
                runtime,
                cancel,
                join,
            });
            0
        }
        Ok(Ok(Err(msg))) => {
            tracing::error!("cclawcore_start: gateway start failed: {msg}");
            drop(guard);
            teardown_failed_runtime(runtime, cancel, join);
            -2
        }
        Ok(Err(_recv)) => {
            tracing::error!(
                "cclawcore_start: daemon task exited before signalling gateway readiness"
            );
            drop(guard);
            teardown_failed_runtime(runtime, cancel, join);
            -2
        }
        Err(_elapsed) => {
            tracing::error!(
                "cclawcore_start: timed out after {:?} waiting for gateway to bind",
                READY_TIMEOUT
            );
            drop(guard);
            teardown_failed_runtime(runtime, cancel, join);
            -2
        }
    }
}

/// Convert a `*const c_char` into an owned UTF-8 `String`.
///
/// - NULL pointer -> `Ok(None)`; callers decide whether that is meaningful.
/// - Invalid UTF-8 -> logs and returns `Err(-1)`.
fn cstr_to_owned(ptr: *const c_char, name: &'static str) -> Result<Option<String>, i32> {
    if ptr.is_null() {
        return Ok(None);
    }
    match unsafe { CStr::from_ptr(ptr) }.to_str() {
        Ok(s) => Ok(Some(s.to_owned())),
        Err(e) => {
            tracing::error!("cclawcore_start: invalid UTF-8 in {name}: {e}");
            Err(-1)
        }
    }
}

/// Cancel + drain the daemon task and drop the runtime, used when
/// `cclawcore_start` is about to return a failure code.
fn teardown_failed_runtime(
    runtime: Runtime,
    cancel: CancellationToken,
    join: JoinHandle<anyhow::Result<()>>,
) {
    cancel.cancel();
    let _ = runtime.block_on(async { tokio::time::timeout(TEARDOWN_WAIT, join).await });
    runtime.shutdown_timeout(TEARDOWN_WAIT);
}

/// Fully-async lifecycle of the daemon task. Guarantees that `ready_tx`
/// receives exactly one signal (success or error) before this function
/// returns, so the FFI caller never waits past [`READY_TIMEOUT`] due to a
/// silently-swallowed early error.
async fn daemon_lifecycle(
    config_dir: String,
    host_override: Option<String>,
    port_arg: u16,
    sw_preset: bool,
    cancel: CancellationToken,
    ready_tx: oneshot::Sender<GatewayReadySignal>,
) -> anyhow::Result<()> {
    let mut ready_slot: Option<oneshot::Sender<GatewayReadySignal>> = Some(ready_tx);

    fn signal_err(
        slot: &mut Option<oneshot::Sender<GatewayReadySignal>>,
        msg: String,
    ) {
        if let Some(tx) = slot.take() {
            let _ = tx.send(Err(msg));
        }
    }

    if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
        tracing::warn!("Failed to install default crypto provider: {e:?}");
    }

    // SAFETY: runs at the top of the daemon task, before any other task on
    // this runtime can concurrently read or write process environment vars.
    unsafe { std::env::set_var("CCLAWCORE_CONFIG_DIR", &config_dir) };
    if sw_preset {
        // SAFETY: same invariant as above.
        unsafe { std::env::set_var("CCLAWCORE_PRESET", "seewo") };
    }

    let mut config = match Box::pin(cclawcore::Config::load_or_init()).await {
        Ok(c) => c,
        Err(e) => {
            signal_err(&mut ready_slot, format!("Config::load_or_init failed: {e:#}"));
            return Err(e);
        }
    };
    config.apply_env_overrides();

    cclawcore::observability::runtime_trace::init_from_config(
        &config.observability,
        &config.workspace_dir,
    );

    let resolved_port = if port_arg == 0 {
        config.gateway.port
    } else {
        port_arg
    };
    let resolved_host = host_override.unwrap_or_else(|| config.gateway.host.clone());

    tracing::info!("cclawcore-ffi: starting daemon on {resolved_host}:{resolved_port}");

    let result = Box::pin(cclawcore::daemon::run_with_shutdown(
        config,
        resolved_host,
        resolved_port,
        Some(cancel),
        ready_slot.take(),
    ))
    .await;

    // Safety net: if run_with_shutdown returned before the gateway supervisor
    // had a chance to signal (for example, an early error inside it), make
    // sure the FFI caller still observes a definitive failure instead of
    // waiting for the 30s timeout.
    if let Some(tx) = ready_slot.take() {
        let msg = match &result {
            Ok(()) => "daemon exited before signalling gateway readiness".to_string(),
            Err(e) => format!("daemon exited with error: {e:#}"),
        };
        let _ = tx.send(Err(msg));
    }

    result
}

/// Stop the running CclawCore daemon.
///
/// Blocks until all daemon components have shut down. Safe to call even if
/// the daemon is not running (no-op).
#[unsafe(no_mangle)]
pub extern "C" fn cclawcore_stop() {
    install_subscriber();
    if std::panic::catch_unwind(AssertUnwindSafe(stop_inner)).is_err() {
        tracing::error!("cclawcore_stop: panicked in FFI entry");
    }
}

fn stop_inner() {
    let handle = {
        let mut guard = DAEMON
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.take()
    };

    let Some(handle) = handle else {
        tracing::info!("cclawcore_stop: daemon is not running, nothing to do");
        return;
    };

    handle.cancel.cancel();

    handle.runtime.block_on(async {
        match handle.join.await {
            Ok(Ok(())) => tracing::info!("cclawcore-ffi: daemon stopped cleanly"),
            Ok(Err(e)) => tracing::warn!("cclawcore-ffi: daemon exited with error: {e}"),
            Err(e) => tracing::warn!("cclawcore-ffi: daemon task panicked: {e}"),
        }
    });
}
