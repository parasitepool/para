use std::sync::OnceLock;
use sysinfo::System;

static SYSTEM: OnceLock<System> = OnceLock::new();

pub fn get_cpu_count() -> usize {
    SYSTEM
        .get_or_init(|| {
            let mut sys = System::new();
            sys.refresh_cpu_all();
            sys
        })
        .cpus()
        .len()
        .max(1)
}

pub fn get_total_memory() -> u64 {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.total_memory()
}

pub fn get_available_memory() -> u64 {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.available_memory()
}
