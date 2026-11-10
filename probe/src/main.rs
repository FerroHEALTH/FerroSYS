// A throwaway probe for FerroHEALTH/FerroSYS#357: what the open-loop generator's lag is made of.
use std::sync::{Arc, Mutex};
use std::time::Duration;
use hdrhistogram::Histogram;
use tokio::time::Instant;

type H = Arc<Mutex<Histogram<u64>>>;
fn h() -> H { Arc::new(Mutex::new(Histogram::new_with_bounds(1, 60_000_000, 3).unwrap())) }
fn rec(h: &H, d: Duration) { h.lock().unwrap().saturating_record(d.as_micros().max(1) as u64); }
fn show(name: &str, h: &H) {
    let h = h.lock().unwrap();
    let q = |p: f64| h.value_at_quantile(p) as f64 / 1000.0;
    println!("{name:28} n={:6} p50={:7.3} p90={:7.3} p99={:7.3} p99.9={:7.3} max={:7.3} ms", h.len(), q(0.5), q(0.9), q(0.99), q(0.999), h.max() as f64 / 1000.0);
}

async fn send(client: reqwest::Client, url: String, intended: Instant, start: H, lat: H) {
    rec(&start, Instant::now().saturating_duration_since(intended));
    if let Ok(r) = client.get(&url).send().await { let _ = r.bytes().await; }
    rec(&lat, Instant::now().saturating_duration_since(intended));
}

// The 0.8.0 shape: a tokio task sleeps to each instant and spawns the send.
async fn tokio_arm(client: reqwest::Client, url: String, t0: Instant, rate: f64, n: u64, wake: H, start: H, lat: H) {
    for i in 0..n {
        let intended = t0 + Duration::from_secs_f64(i as f64 / rate);
        tokio::time::sleep_until(intended).await;
        rec(&wake, Instant::now().saturating_duration_since(intended));
        tokio::spawn(send(client.clone(), url.clone(), intended, start.clone(), lat.clone()));
    }
}

// A dedicated OS thread sleeps to each instant with the OS clock and spawns the send onto the runtime.
fn thread_arm(handle: tokio::runtime::Handle, client: reqwest::Client, url: String, t0: Instant, rate: f64, n: u64, wake: H, start: H, lat: H) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for i in 0..n {
            let intended = t0 + Duration::from_secs_f64(i as f64 / rate);
            let now = Instant::now();
            if intended > now { std::thread::sleep(intended - now); }
            rec(&wake, Instant::now().saturating_duration_since(intended));
            handle.spawn(send(client.clone(), url.clone(), intended, start.clone(), lat.clone()));
        }
    })
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a[1].clone();
    let rate: f64 = a[2].parse().unwrap();
    let secs: f64 = a[3].parse().unwrap();
    let urls = [a[4].clone(), a[5].clone()];
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let n = (rate * secs) as u64;
    let hs: Vec<(H, H, H)> = (0..2).map(|_| (h(), h(), h())).collect();
    rt.block_on(async {
        let client = reqwest::Client::new();
        let t0 = Instant::now() + Duration::from_millis(200);
        let half = Duration::from_secs_f64(0.5 / rate);
        match mode.as_str() {
            "tokio" => {
                let (w0, s0, l0) = hs[0].clone();
                let (w1, s1, l1) = hs[1].clone();
                tokio::join!(
                    tokio_arm(client.clone(), urls[0].clone(), t0, rate, n, w0, s0, l0),
                    tokio_arm(client.clone(), urls[1].clone(), t0 + half, rate, n, w1, s1, l1),
                );
            }
            _ => {
                let handle = tokio::runtime::Handle::current();
                let js: Vec<_> = (0..2).map(|k| {
                    let (w, s, l) = hs[k].clone();
                    thread_arm(handle.clone(), client.clone(), urls[k].clone(), t0 + half * k as u32, rate, n, w, s, l)
                }).collect();
                tokio::task::spawn_blocking(move || for j in js { j.join().unwrap() }).await.unwrap();
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    });
    for (k, (w, s, l)) in hs.iter().enumerate() {
        println!("-- {mode} arm {k}");
        show("wake (clock lateness)", w);
        show("start (send task first poll)", s);
        show("latency", l);
    }
}
