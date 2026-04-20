use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Mutex;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct DaemonHandle {
    runtime: Runtime,
    cancel: CancellationToken,
    join: JoinHandle<anyhow::Result<()>>,
}

static DAEMON: Mutex<Option<DaemonHandle>> = Mutex::new(None);

/// Start the CclawCore daemon in a background tokio runtime.
///
/// # Parameters
/// - `config_dir` — UTF-8 path to the configuration directory (must not be NULL).
/// - `host` — bind address (UTF-8). Pass NULL to use the value from config.
/// - `port` — gateway port. Pass 0 to use the value from config.
/// - `sw_preset` — if `true`, activate the built-in Seewo preset.
///
/// # Returns
/// `0` on success, `1` if already running, `-1` on error.
#[unsafe(no_mangle)]
pub extern "C" fn cclawcore_start(
    config_dir: *const c_char,
    host: *const c_char,
    port: u16,
    sw_preset: bool,
) -> i32 {
    let mut guard = match DAEMON.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    if guard.is_some() {
        tracing::warn!("cclawcore_start called but daemon is already running");
        return 1;
    }

    let config_dir_str = if config_dir.is_null() {
        tracing::error!("cclawcore_start: config_dir must not be NULL");
        return -1;
    } else {
        match unsafe { CStr::from_ptr(config_dir) }.to_str() {
            Ok(s) => s.to_owned(),
            Err(e) => {
                tracing::error!("cclawcore_start: invalid UTF-8 in config_dir: {e}");
                return -1;
            }
        }
    };

    let host_override = if host.is_null() {
        None
    } else {
        match unsafe { CStr::from_ptr(host) }.to_str() {
            Ok(s) => Some(s.to_owned()),
            Err(e) => {
                tracing::error!("cclawcore_start: invalid UTF-8 in host: {e}");
                return -1;
            }
        }
    };

    let runtime = match Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cclawcore_start: failed to create tokio runtime: {e}");
            return -1;
        }
    };

    let cancel = CancellationToken::new();
    let cancel_clone = cancel.clone();
    let port_arg = port;

    let join = runtime.spawn(async move {
        if let Err(e) = rustls::crypto::ring::default_provider().install_default() {
            tracing::warn!("Failed to install default crypto provider: {e:?}");
        }

        unsafe { std::env::set_var("CCLAWCORE_CONFIG_DIR", &config_dir_str) };

        if sw_preset {
            unsafe { std::env::set_var("CCLAWCORE_PRESET", "seewo") };
        }

        let mut config = Box::pin(cclawcore::Config::load_or_init()).await?;
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

        tracing::info!(
            "cclawcore-ffi: starting daemon on {resolved_host}:{resolved_port}"
        );

        Box::pin(cclawcore::daemon::run_with_shutdown(
            config,
            resolved_host,
            resolved_port,
            Some(cancel_clone),
        ))
        .await
    });

    *guard = Some(DaemonHandle {
        runtime,
        cancel,
        join,
    });

    0
}

/// Stop the running CclawCore daemon.
///
/// Blocks until all daemon components have shut down. Safe to call even if the
/// daemon is not running (no-op in that case).
#[unsafe(no_mangle)]
pub extern "C" fn cclawcore_stop() {
    let handle = {
        let mut guard = match DAEMON.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
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
