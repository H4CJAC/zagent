//! Global Seewo runtime state — injected via gateway API, readable from any module.

use std::sync::{OnceLock, RwLock};

static SW_TOKEN: OnceLock<RwLock<Option<String>>> = OnceLock::new();

fn state() -> &'static RwLock<Option<String>> {
    SW_TOKEN.get_or_init(|| RwLock::new(None))
}

pub fn get_sw_token() -> Option<String> {
    match state().read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

pub fn set_sw_token(token: String) {
    match state().write() {
        Ok(mut guard) => *guard = Some(token),
        Err(poisoned) => *poisoned.into_inner() = Some(token),
    }
}
