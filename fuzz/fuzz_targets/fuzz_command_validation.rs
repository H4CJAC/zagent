#![no_main]
use cclawcore::security::SecurityPolicy;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let policy = SecurityPolicy::default();
        let _ = policy.validate_command_execution(s, false);
    }
});
