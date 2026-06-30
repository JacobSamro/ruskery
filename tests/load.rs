//! Load harness (informational): boots the real binary + in-process S3 stub,
//! seeds an image, then hammers the hot path — concurrent presigned-redirect
//! pulls — reporting throughput and latency percentiles. Correctness invariants
//! are asserted (every pull returns the exact bytes); raw timings are not, since
//! CI runners are noisy.
//!
//! Run with: `cargo test --release --test load -- --ignored --nocapture`

mod common;

use common::*;
use serde_json::json;
use std::sync::Arc;
use std::time::Instant;

#[tokio::test(flavor = "multi_thread")]
#[ignore = "informational load test; run explicitly with --ignored"]
async fn pull_throughput() {
    let stub = spawn_s3_stub().await;
    let rk = Ruskery::spawn(&stub).await;
    let base = Arc::new(rk.base.clone());

    let dash = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    dash.post(format!("{base}/api/v1/setup"))
        .json(&json!({
            "email":"a@x.io","username":"admin","password":"supersecret",
            "org_slug":"acme","org_name":"Acme"
        }))
        .send()
        .await
        .unwrap();
    let pat = dash
        .post(format!("{base}/api/v1/tokens"))
        .json(&json!({"name":"load"}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let reg = reqwest::Client::new();
    let jwt =
        Arc::new(registry_token(&reg, &base, "admin", &pat, "repository:acme/app:pull,push").await);

    // Seed a layer to pull.
    let layer = vec![0x5Au8; 256 * 1024];
    let layer_d = Arc::new(push_blob(&reg, &base, &jwt, "acme/app", &layer).await);
    let layer = Arc::new(layer);

    const TOTAL: usize = 2_000;
    const CONCURRENCY: usize = 64;

    let started = Instant::now();
    let mut handles = Vec::new();
    let sem = Arc::new(tokio::sync::Semaphore::new(CONCURRENCY));

    for _ in 0..TOTAL {
        let permit = sem.clone().acquire_owned().await.unwrap();
        let (base, jwt, layer_d, layer) =
            (base.clone(), jwt.clone(), layer_d.clone(), layer.clone());
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            let client = reqwest::Client::new();
            let t = Instant::now();
            let body = client
                .get(format!("{base}/v2/acme/app/blobs/{layer_d}"))
                .bearer_auth(&*jwt)
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(body.as_ref(), layer.as_slice(), "pulled bytes mismatch");
            t.elapsed().as_secs_f64() * 1000.0
        }));
    }

    let mut latencies = Vec::with_capacity(TOTAL);
    for h in handles {
        latencies.push(h.await.unwrap());
    }
    let wall = started.elapsed().as_secs_f64();

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| latencies[((latencies.len() as f64 * p) as usize).min(latencies.len() - 1)];
    let mean = latencies.iter().sum::<f64>() / latencies.len() as f64;

    println!("\n── ruskery pull load ──────────────────────────────");
    println!("requests     : {TOTAL} (concurrency {CONCURRENCY})");
    println!("wall time    : {wall:.2}s");
    println!("throughput   : {:.0} pulls/s", TOTAL as f64 / wall);
    println!("latency mean : {mean:.2} ms");
    println!("latency p50  : {:.2} ms", pct(0.50));
    println!("latency p95  : {:.2} ms", pct(0.95));
    println!("latency p99  : {:.2} ms", pct(0.99));
    println!("latency max  : {:.2} ms", latencies[latencies.len() - 1]);
    println!("───────────────────────────────────────────────────");

    // Correctness invariant only: every concurrent pull returned the right bytes
    // (asserted above). No timing assertions — runners are noisy.
}
