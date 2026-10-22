//! Speech models for local transcription (whisper.cpp ggml files), served by the same resumable,
//! checksum-verified [`crate::ModelStore`] as the LLMs. Hashes and sizes are the Hugging Face LFS
//! values of `ggerganov/whisper.cpp`.

use crate::catalog::CatalogEntry;

#[allow(clippy::too_many_arguments)]
fn w(
    id: &str,
    name: &str,
    file: &str,
    sha: &str,
    size: u64,
    params_b: f32,
    note: &str,
) -> CatalogEntry {
    CatalogEntry {
        id: id.into(),
        name: name.into(),
        family: "whisper".into(),
        params_b,
        quant: file
            .trim_end_matches(".bin")
            .rsplit('-')
            .next()
            .unwrap_or("")
            .into(),
        file: file.into(),
        url: format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}"),
        sha256: sha.into(),
        size_bytes: size,
        context: 0,
        license: "MIT".into(),
        note: note.into(),
    }
}

/// Multilingual speech models, small to large.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        w(
            "tiny",
            "Whisper tiny",
            "ggml-tiny-q5_1.bin",
            "818710568da3ca15689e31a743197b520007872ff9576237bda97bd1b469c3d7",
            32_152_673,
            0.04,
            "Fastest, rough on accents and noise.",
        ),
        w(
            "base",
            "Whisper base",
            "ggml-base-q5_1.bin",
            "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
            59_707_625,
            0.07,
            "Good default for clean speech on a phone-class CPU.",
        ),
        w(
            "small",
            "Whisper small",
            "ggml-small-q5_1.bin",
            "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
            190_085_487,
            0.24,
            "Solid accuracy for most languages.",
        ),
        w(
            "medium",
            "Whisper medium",
            "ggml-medium-q5_0.bin",
            "19fea4b380c3a618ec4723c3eef2eb785ffba0d0538cf43f8f235e7b3b34220f",
            539_212_467,
            0.77,
            "Better for accents and music beds; slow without a GPU.",
        ),
        w(
            "large-v3-turbo",
            "Whisper large-v3-turbo",
            "ggml-large-v3-turbo-q5_0.bin",
            "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
            574_041_195,
            0.81,
            "Near large-v3 accuracy at medium cost. Best quality per second.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_wellformed() {
        let c = catalog();
        assert_eq!(c.len(), 5);
        for e in c {
            assert_eq!(e.sha256.len(), 64, "{}", e.id);
            assert!(e.url.ends_with(&e.file));
            assert!(e.size_bytes > 1_000_000);
        }
    }
}
