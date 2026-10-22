//! Curated GGUF models (Q4_K_M quantisation, HuggingFace) with the size and SHA-256 that HF
//! publishes for each file, plus a fit check against the device's RAM.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// Stable id used in `local/<id>` refs.
    pub id: String,
    pub name: String,
    pub family: String,
    pub params_b: f32,
    pub quant: String,
    /// File name on disk.
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
    /// Context length the model was trained for.
    pub context: u32,
    pub license: String,
    pub note: String,
}

#[allow(clippy::too_many_arguments)]
fn e(id: &str, name: &str, family: &str, params_b: f32, repo: &str, file: &str, sha: &str, size: u64, ctx: u32, license: &str, note: &str) -> CatalogEntry {
    CatalogEntry {
        id: id.into(),
        name: name.into(),
        family: family.into(),
        params_b,
        quant: "Q4_K_M".into(),
        file: file.into(),
        url: format!("https://huggingface.co/{repo}/resolve/main/{file}"),
        sha256: sha.into(),
        size_bytes: size,
        context: ctx,
        license: license.into(),
        note: note.into(),
    }
}

/// Smallest first.
pub fn builtin() -> Vec<CatalogEntry> {
    vec![
        e("qwen2.5-0.5b", "Qwen2.5 0.5B Instruct", "qwen2.5", 0.5, "Qwen/Qwen2.5-0.5B-Instruct-GGUF", "qwen2.5-0.5b-instruct-q4_k_m.gguf",
          "74a4da8c9fdbcd15bd1f6d01d621410d31c6fc00986f5eb687824e7b93d7a9db", 491_400_032, 32_768, "Apache-2.0", "Tiny; chat and simple tool calls on any phone."),
        e("llama3.2-1b", "Llama 3.2 1B Instruct", "llama3.2", 1.2, "bartowski/Llama-3.2-1B-Instruct-GGUF", "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
          "6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83", 807_694_464, 131_072, "Llama 3.2 Community", "Small and fast; good for short answers."),
        e("qwen2.5-coder-1.5b", "Qwen2.5 Coder 1.5B Instruct", "qwen2.5", 1.5, "bartowski/Qwen2.5-Coder-1.5B-Instruct-GGUF", "Qwen2.5-Coder-1.5B-Instruct-Q4_K_M.gguf",
          "f530705d447660a4336c329981af164b471b60b974b1d808d57e8ec9fe23b239", 986_048_800, 32_768, "Apache-2.0", "Code-focused."),
        e("qwen2.5-1.5b", "Qwen2.5 1.5B Instruct", "qwen2.5", 1.5, "Qwen/Qwen2.5-1.5B-Instruct-GGUF", "qwen2.5-1.5b-instruct-q4_k_m.gguf",
          "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e", 1_117_320_736, 32_768, "Apache-2.0", "Best small all-rounder; follows tool-call format well."),
        e("gemma2-2b", "Gemma 2 2B Instruct", "gemma2", 2.6, "bartowski/gemma-2-2b-it-GGUF", "gemma-2-2b-it-Q4_K_M.gguf",
          "e0aee85060f168f0f2d8473d7ea41ce2f3230c1bc1374847505ea599288a7787", 1_708_582_752, 8_192, "Gemma", "Strong writing for its size; no system role."),
        e("llama3.2-3b", "Llama 3.2 3B Instruct", "llama3.2", 3.2, "bartowski/Llama-3.2-3B-Instruct-GGUF", "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
          "6c1a2b41161032677be168d354123594c0e6e67d2b9227c84f296ad037c728ff", 2_019_377_696, 131_072, "Llama 3.2 Community", "Good general model for phones with 6 GB+."),
        e("qwen2.5-3b", "Qwen2.5 3B Instruct", "qwen2.5", 3.1, "Qwen/Qwen2.5-3B-Instruct-GGUF", "qwen2.5-3b-instruct-q4_k_m.gguf",
          "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d", 2_104_932_768, 32_768, "Qwen Research", "Better tool use than 1.5B; 6 GB+ devices."),
        e("qwen2.5-7b", "Qwen2.5 7B Instruct", "qwen2.5", 7.6, "bartowski/Qwen2.5-7B-Instruct-GGUF", "Qwen2.5-7B-Instruct-Q4_K_M.gguf",
          "65b8fcd92af6b4fefa935c625d1ac27ea29dcb6ee14589c55a8f115ceaaa1423", 4_683_074_240, 32_768, "Apache-2.0", "Laptop/desktop class; solid agent model."),
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub total_ram: u64,
    pub available_ram: u64,
    pub cores: usize,
}

impl Device {
    pub fn probe() -> Self {
        use sysinfo::{MemoryRefreshKind, RefreshKind, System};
        let sys = System::new_with_specifics(RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()));
        Self {
            total_ram: sys.total_memory(),
            available_ram: sys.available_memory(),
            cores: std::thread::available_parallelism().map_or(2, usize::from),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Runs well: weights + cache stay under ~55% of RAM.
    Good,
    /// Runs, but leaves little headroom (up to ~80% of RAM).
    Tight,
    TooBig,
}

/// RAM needed: weights + KV cache and runtime overhead (~25%, at least 300 MB).
pub fn ram_needed(size_bytes: u64) -> u64 {
    size_bytes + (size_bytes / 4).max(300 * 1024 * 1024)
}

pub fn fit(size_bytes: u64, dev: &Device) -> Fit {
    let need = ram_needed(size_bytes);
    if dev.total_ram == 0 {
        return Fit::Tight;
    }
    // Pages of the weights are mmapped, so the budget is the total, not what is free right now.
    if need as f64 <= dev.total_ram as f64 * 0.55 && need <= dev.available_ram.max(need.min(dev.total_ram / 2)) {
        Fit::Good
    } else if need as f64 <= dev.total_ram as f64 * 0.8 {
        Fit::Tight
    } else {
        Fit::TooBig
    }
}

/// The largest model that fits comfortably; falls back to the smallest that is at least tight.
pub fn recommend(dev: &Device) -> Option<CatalogEntry> {
    let all = builtin();
    all.iter().rev().find(|m| fit(m.size_bytes, dev) == Fit::Good).or_else(|| all.iter().find(|m| fit(m.size_bytes, dev) != Fit::TooBig)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn catalog_is_consistent() {
        let all = builtin();
        let mut ids: Vec<_> = all.iter().map(|m| m.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for m in &all {
            assert_eq!(m.sha256.len(), 64, "{}", m.id);
            assert!(m.url.starts_with("https://huggingface.co/") && m.url.ends_with(&m.file));
        }
        assert!(all.windows(2).all(|w| w[0].size_bytes <= w[1].size_bytes), "sorted smallest first");
    }

    #[test]
    fn fit_and_recommendation_follow_ram() {
        let phone = Device { total_ram: 4 * GB, available_ram: 2 * GB, cores: 8 };
        let laptop = Device { total_ram: 16 * GB, available_ram: 10 * GB, cores: 8 };
        let m7 = builtin().into_iter().find(|m| m.id == "qwen2.5-7b").unwrap();
        assert_eq!(fit(m7.size_bytes, &phone), Fit::TooBig);
        assert_eq!(fit(m7.size_bytes, &laptop), Fit::Good);
        assert_eq!(recommend(&laptop).unwrap().id, "qwen2.5-7b");
        let small = recommend(&phone).unwrap();
        assert!(small.size_bytes < 2 * GB, "{}", small.id);
    }
}
