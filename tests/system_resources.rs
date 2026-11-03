//! Test de recursos del sistema con hardware real.
//!
//! Ejecuta el pipeline OCR completo (modelos PP-OCRv6 + tract) y mide, con la
//! API de Windows, el uso real de memoria y CPU del proceso, además del estado
//! global de CPU y RAM de la máquina. Solo compila en Windows.
//!
//! Ejecutar mostrando el informe:
//! `cargo test --test system_resources -- --nocapture`

#![cfg(windows)]

use octxt::image_io::load_images;
use octxt::models::Tier;
use octxt::ocr::engine::{Engine, EngineConfig};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::time::Instant;

// --- FFI mínima de Windows (sin dependencias nuevas) -----------------------

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    /// Tiempo en unidades de 100 ns (FILETIME).
    fn ticks(self) -> u64 {
        ((self.high as u64) << 32) | self.low as u64
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct MemoryStatusEx {
    length: u32,
    memory_load: u32,
    total_phys: u64,
    avail_phys: u64,
    total_page_file: u64,
    avail_page_file: u64,
    total_virtual: u64,
    avail_virtual: u64,
    avail_extended_virtual: u64,
}

#[link(name = "psapi")]
extern "system" {
    fn GetProcessMemoryInfo(
        process: *mut c_void,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn GetProcessTimes(
        process: *mut c_void,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
}

fn process_memory() -> ProcessMemoryCounters {
    let mut counters = ProcessMemoryCounters {
        cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
        ..Default::default()
    };
    // SAFETY: el handle pseudo-proceso es válido y `counters.cb` describe el
    // tamaño exacto de la estructura que escribe la API.
    unsafe {
        assert_ne!(
            GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb),
            0,
            "GetProcessMemoryInfo falló"
        );
    }
    counters
}

/// (kernel, user) acumulados del proceso, en segundos.
fn process_cpu_seconds() -> (f64, f64) {
    let mut creation = FileTime::default();
    let mut exit = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: punteros a estructuras propias correctamente inicializadas.
    unsafe {
        assert_ne!(
            GetProcessTimes(
                GetCurrentProcess(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            ),
            0,
            "GetProcessTimes falló"
        );
    }
    (
        kernel.ticks() as f64 / 10_000_000.0,
        user.ticks() as f64 / 10_000_000.0,
    )
}

/// (idle, kernel, user) acumulados de todo el sistema, en segundos.
fn system_cpu_seconds() -> (f64, f64, f64) {
    let mut idle = FileTime::default();
    let mut kernel = FileTime::default();
    let mut user = FileTime::default();
    // SAFETY: punteros a estructuras propias correctamente inicializadas.
    unsafe {
        assert_ne!(
            GetSystemTimes(&mut idle, &mut kernel, &mut user),
            0,
            "GetSystemTimes falló"
        );
    }
    (
        idle.ticks() as f64 / 10_000_000.0,
        kernel.ticks() as f64 / 10_000_000.0,
        user.ticks() as f64 / 10_000_000.0,
    )
}

fn global_memory() -> MemoryStatusEx {
    let mut status = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        ..Default::default()
    };
    // SAFETY: `status.length` describe el tamaño exacto de la estructura.
    unsafe {
        assert_ne!(
            GlobalMemoryStatusEx(&mut status),
            0,
            "GlobalMemoryStatusEx falló"
        );
    }
    status
}

// --- Test -----------------------------------------------------------------

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("eurotext.tif")
}

fn tier() -> Tier {
    match std::env::var("OCTXT_TEST_TIER")
        .unwrap_or_else(|_| "tiny".into())
        .to_lowercase()
        .as_str()
    {
        "tiny" => Tier::Tiny,
        "small" => Tier::Small,
        "medium" => Tier::Medium,
        other => panic!("OCTXT_TEST_TIER desconocido: {other}"),
    }
}

fn mib(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[test]
fn recursos_reales_durante_ocr() {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let ram = global_memory();
    let mem_before = process_memory();
    let (k0, u0) = process_cpu_seconds();
    let (sys_idle0, sys_kernel0, sys_user0) = system_cpu_seconds();
    let t0 = Instant::now();

    // --- Carga real de modelos y OCR ---
    let cfg = EngineConfig {
        tier: tier(),
        threads: 0, // todos los núcleos lógicos: hardware real
        ..Default::default()
    };
    let engine = Engine::load(&cfg, None).expect("cargar modelos PP-OCRv6");
    let pool_threads = engine.pool_threads();

    let pages = load_images(&fixture()).expect("cargar fixture");
    let mut lines_total = 0usize;
    for page in &pages {
        let lines = engine.ocr(page).expect("ejecutar OCR");
        lines_total += lines.len();
    }

    let elapsed = t0.elapsed();
    let mem_after = process_memory();
    let (k1, u1) = process_cpu_seconds();
    let (sys_idle1, sys_kernel1, sys_user1) = system_cpu_seconds();

    // --- Cálculos ---
    let wall = elapsed.as_secs_f64().max(1e-9);
    let proc_cpu = (k1 - k0) + (u1 - u0);
    let proc_cpu_pct = proc_cpu / wall * 100.0;

    let sys_idle = sys_idle1 - sys_idle0;
    let sys_busy = (sys_kernel1 - sys_kernel0 - sys_idle) + (sys_user1 - sys_user0);
    let sys_total = (sys_kernel1 - sys_kernel0) + (sys_user1 - sys_user0);
    let sys_cpu_pct = if sys_total > 0.0 {
        (sys_busy / sys_total * 100.0).clamp(0.0, 100.0)
    } else {
        0.0
    };

    let peak_mib = mib(mem_after.peak_working_set_size);
    let working_mib = mib(mem_after.working_set_size);
    let before_mib = mib(mem_before.working_set_size);
    let ram_total_gib = ram.total_phys as f64 / (1024.0 * 1024.0 * 1024.0);
    let ram_avail_gib = ram.avail_phys as f64 / (1024.0 * 1024.0 * 1024.0);
    let used_pct = if ram.total_phys > 0 {
        (ram.total_phys - ram.avail_phys) as f64 / ram.total_phys as f64 * 100.0
    } else {
        0.0
    };

    println!("=== Informe de recursos (hardware real) ===");
    println!("CPU lógica disponible : {cpus}");
    println!("Hilos del pool OCR    : {pool_threads}");
    println!("Páginas procesadas    : {}", pages.len());
    println!("Líneas OCR            : {lines_total}");
    println!("Tiempo pared          : {wall:.2} s");
    println!("CPU del proceso       : {proc_cpu:.2} s ({proc_cpu_pct:.1} % de un núcleo)");
    println!(
        "CPU del sistema       : {sys_cpu_pct:.1} % (idle {:.2} s / busy {:.2} s)",
        sys_idle, sys_busy
    );
    println!("RSS antes / ahora     : {before_mib:.1} / {working_mib:.1} MiB");
    println!("RSS pico del proceso  : {peak_mib:.1} MiB");
    println!(
        "RAM del sistema       : {ram_total_gib:.1} GiB total, {ram_avail_gib:.1} GiB libre ({used_pct:.0} % en uso)"
    );
    println!("Fallos de página      : {}", mem_after.page_fault_count);

    // --- Asserts deliberadamente generosos (dependen de la máquina) ---
    assert!(!pages.is_empty(), "debe haber al menos una página");
    assert!(lines_total > 0, "el OCR real debe producir texto");
    assert!(peak_mib > 1.0, "el proceso debe usar memoria real");
    assert!(
        mem_after.working_set_size > 0,
        "working set del proceso vacío"
    );
    assert!(proc_cpu > 0.0, "el proceso debe consumir CPU real");
    assert!(pool_threads >= 1, "el pool debe tener al menos un hilo");
    assert!(
        (0.0..=100.0).contains(&sys_cpu_pct),
        "porcentaje de CPU del sistema fuera de rango: {sys_cpu_pct}"
    );
    assert!(ram.total_phys > 0, "no se pudo leer la RAM total");
    assert!(
        ram.avail_phys <= ram.total_phys,
        "RAM disponible mayor que la total"
    );
    assert!(wall < 180.0, "el OCR real tardó demasiado ({wall:.1} s)");
}
