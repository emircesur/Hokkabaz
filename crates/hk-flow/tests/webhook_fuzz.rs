//! Hostile-input smoke test for the webhook surface: random methods, paths, headers, query
//! strings and bodies (valid-ish and garbage) must never panic or hang. Deterministic PRNG so a
//! failure reproduces from the printed seed.

use hk_flow::triggers::{
    WebhookKind, WebhookRegistry, WebhookRequest, parse_waiting_path, request_to_item,
};
use serde_json::Map;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max);
        (0..n).map(|_| self.next() as u8).collect()
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
    fn text(&mut self, max: usize) -> String {
        const ALPHABET: &[char] = &[
            'a', 'b', 'Z', '0', '/', '.', '%', '&', '=', '?', ':', '-', '_', ' ', '\n', '\r', '"',
            '{', '}', '[', ']', '\\', 'é', '\u{0}', '\u{202e}',
        ];
        (0..self.below(max))
            .map(|_| ALPHABET[self.below(ALPHABET.len())])
            .collect()
    }
}

const CTYPES: &[&str] = &[
    "application/json",
    "application/x-www-form-urlencoded",
    "multipart/form-data; boundary=xyz",
    "multipart/form-data; boundary=",
    "multipart/form-data",
    "text/plain",
    "application/octet-stream",
    "application/json; charset=utf-16",
    "",
];

fn json_ish(r: &mut Rng) -> Vec<u8> {
    match r.below(6) {
        0 => b"{\"a\":".repeat(r.below(3000)),
        1 => format!("{}{}", "[".repeat(r.below(5000)), "]".repeat(r.below(5000))).into_bytes(),
        2 => format!(
            "{{\"k\":\"{}\"}}",
            r.text(40).replace(['"', '\\', '\n', '\r', '\u{0}'], "")
        )
        .into_bytes(),
        3 => b"{\"n\":1e999999,\"m\":-0,\"s\":\"\\ud800\"}".to_vec(),
        _ => r.bytes(200),
    }
}

fn multipart_ish(r: &mut Rng) -> Vec<u8> {
    let mut b = Vec::new();
    for _ in 0..r.below(4) {
        b.extend_from_slice(b"--xyz\r\nContent-Disposition: form-data; name=\"");
        b.extend_from_slice(r.text(8).as_bytes());
        b.extend_from_slice(b"\"; filename=\"");
        b.extend_from_slice(r.text(8).as_bytes());
        b.extend_from_slice(b"\"\r\n\r\n");
        b.extend_from_slice(&r.bytes(50));
        b.extend_from_slice(b"\r\n");
    }
    if r.below(2) == 0 {
        b.extend_from_slice(b"--xyz--");
    }
    b
}

#[test]
fn hostile_requests_never_panic() {
    let seed = std::env::var("HK_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x9E37_79B9_7F4A_7C15u64);
    eprintln!("webhook fuzz seed: {seed}");
    let mut r = Rng(seed | 1);
    let reg = WebhookRegistry::new();
    for _ in 0..20_000 {
        let ctype = r.pick(CTYPES).to_owned();
        let body = match ctype.as_str() {
            c if c.starts_with("multipart") => multipart_ish(&mut r),
            "application/x-www-form-urlencoded" => r.text(120).into_bytes(),
            c if c.contains("json") => json_ish(&mut r),
            _ => r.bytes(300),
        };
        let mut headers = vec![("Content-Type".to_owned(), ctype)];
        for _ in 0..r.below(4) {
            headers.push((r.text(10), r.text(30)));
        }
        let req = WebhookRequest {
            method: r
                .pick(&["GET", "POST", "PUT", "DELETE", "HEAD", "PATCH", "", "\u{0}"])
                .to_owned(),
            path: r.text(60),
            headers,
            query: (r.below(2) == 0).then(|| r.text(80)),
            body,
        };
        let _ = request_to_item(&req, Map::new(), r.below(2) == 0);
        for kind in [WebhookKind::Production, WebhookKind::Test] {
            let _ = reg.resolve(kind, &req.method, &req.path);
        }
        let _ = parse_waiting_path(&r.text(80));
    }
}
