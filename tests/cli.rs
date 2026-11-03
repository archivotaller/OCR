//! Pruebas del binario `octxt` de extremo a extremo.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn tier() -> String {
    std::env::var("OCTXT_TEST_TIER").unwrap_or_else(|_| "tiny".into())
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("octxt-cli-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("crear directorio temporal");
    dir
}

#[test]
fn version_y_help_funcionan_sin_modelos() {
    let bin = env!("CARGO_BIN_EXE_octxt");
    let version = Command::new(bin)
        .arg("--version")
        .output()
        .expect("ejecutar --version");
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains("octxt"));

    let help = Command::new(bin)
        .arg("--help")
        .output()
        .expect("ejecutar --help");
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for cmd in ["txt", "pdf", "boxes", "fetch", "check"] {
        assert!(text.contains(cmd), "--help debe listar el subcomando {cmd}");
    }
}

#[test]
fn cli_txt_json_y_pdf_sobre_fixture() {
    let bin = env!("CARGO_BIN_EXE_octxt");
    let dir = temp_dir();
    let img = fixture("eurotext.tif");
    let tier = tier();

    // --- txt --json: el JSON de stdout debe traer líneas reconocidas.
    let out = Command::new(bin)
        .args(["--tier", &tier, "--no-prewarm", "txt", "--json"])
        .arg(&img)
        .output()
        .expect("ejecutar octxt txt --json");
    assert!(
        out.status.success(),
        "octxt txt --json falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout debe ser JSON válido");
    let files = json.as_array().expect("el JSON debe ser un array");
    assert_eq!(files.len(), 1);
    let lines = files[0]["lines"].as_array().expect("array de líneas");
    assert!(!lines.is_empty(), "el JSON no contiene líneas OCR");
    let all = lines
        .iter()
        .filter_map(|l| l["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    assert!(all.contains("fuchs"), "texto OCR inesperado: {all}");

    // --- txt --out: escribe el texto en un archivo.
    let txt_path = dir.join("salida.txt");
    let out = Command::new(bin)
        .args(["--tier", &tier, "--no-prewarm", "txt", "--out"])
        .arg(&txt_path)
        .arg(&img)
        .output()
        .expect("ejecutar octxt txt --out");
    assert!(
        out.status.success(),
        "octxt txt --out falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = std::fs::read_to_string(&txt_path).expect("leer txt generado");
    assert!(body.to_lowercase().contains("fuchs"));

    // --- pdf --out: un PDF válido y buscable.
    let pdf_path = dir.join("salida.pdf");
    let out = Command::new(bin)
        .args(["--tier", &tier, "--no-prewarm", "pdf", "--out"])
        .arg(&pdf_path)
        .arg(&img)
        .output()
        .expect("ejecutar octxt pdf");
    assert!(
        out.status.success(),
        "octxt pdf falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&pdf_path).expect("leer PDF generado");
    assert!(bytes.starts_with(b"%PDF-"));
    let doc = lopdf::Document::load(&pdf_path).expect("lopdf debe abrir el PDF");
    assert_eq!(doc.get_pages().len(), 1);
}
