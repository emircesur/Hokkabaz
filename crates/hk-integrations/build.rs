//! Embeds every `specs/*.json` (declarative integrations) so adding a service is one new file.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("specs");
    println!("cargo:rerun-if-changed=specs");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut out = String::from("pub const ALL: &[(&str, &str)] = &[\n");
    for f in files {
        let name = f.file_stem().unwrap().to_string_lossy().into_owned();
        let _ = writeln!(
            out,
            "    ({name:?}, include_str!({:?})),",
            f.display().to_string()
        );
    }
    out.push_str("];\n");
    std::fs::write(
        PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("specs.rs"),
        out,
    )
    .unwrap();
}
