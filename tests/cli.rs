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
fn pdf_help_lista_las_opciones_de_reemplazo() {
    let bin = env!("CARGO_BIN_EXE_octxt");
    let help = Command::new(bin)
        .args(["pdf", "--help"])
        .output()
        .expect("ejecutar pdf --help");
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    for flag in [
        "--replace-text",
        "--erase-mode",
        "--erase-margin",
        "--background",
        "--text-color",
        "--font-cjk",
        "--exclude",
    ] {
        assert!(text.contains(flag), "pdf --help debe listar {flag}");
    }
}

#[test]
fn pdf_valida_colores_y_exclusiones_antes_del_ocr() {
    let bin = env!("CARGO_BIN_EXE_octxt");
    let img = fixture("eurotext.tif");

    let out = Command::new(bin)
        .args(["pdf", "--replace-text", "--background", "no-es-color"])
        .arg(&img)
        .output()
        .expect("ejecutar con color inválido");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("color no válido"));

    let out = Command::new(bin)
        .args(["pdf", "--replace-text", "--exclude", "0:1"])
        .arg(&img)
        .output()
        .expect("ejecutar con exclusión inválida");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("empiezan en 1"));
}

#[test]
#[ignore = "genera un PDF visible completo con OCR; ejecutar a mano con --ignored"]
fn pdf_replace_text_fixture() {
    let bin = env!("CARGO_BIN_EXE_octxt");
    let dir = temp_dir();
    let img = fixture("eurotext.tif");
    let out_pdf = dir.join("eurotext.visible.pdf");

    let out = Command::new(bin)
        .args([
            "--tier",
            &tier(),
            "--no-prewarm",
            "pdf",
            "--replace-text",
            "--out",
        ])
        .arg(&out_pdf)
        .arg(&img)
        .output()
        .expect("ejecutar octxt pdf --replace-text");
    assert!(
        out.status.success(),
        "octxt pdf --replace-text falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let doc = lopdf::Document::load(&out_pdf).expect("lopdf debe abrir el PDF visible");
    assert_eq!(doc.get_pages().len(), 1);
    let page_id = *doc.get_pages().values().next().unwrap();
    let content = doc.get_page_content(page_id);
    let text = String::from_utf8_lossy(&content);
    assert!(text.contains("Tj"), "falta el texto visible");
    let raw = std::fs::read(&out_pdf).expect("leer el PDF");
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("FontFile2"), "falta la fuente incrustada");
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
