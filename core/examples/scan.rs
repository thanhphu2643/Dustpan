//! Dry run: `cargo run --release --example scan [root…]` prints what the app would list.
use dustpan_core::{scan, ScanConfig};

fn main() {
    let home = std::env::var("HOME").expect("HOME").into();
    let roots = std::env::args().skip(1).map(Into::into).collect();
    let started = std::time::Instant::now();
    let r = scan(&ScanConfig { home, project_roots: roots, large_file_mb: 1024 });
    let gb = |b: u64| b as f64 / 1e9;
    for c in &r.categories {
        let total: u64 = c.items.iter().map(|i| i.size).sum();
        let picked: u64 = c.items.iter().filter(|i| i.suggested).map(|i| i.size).sum();
        println!("{:>8.2} GB  {:<34} {} items, {:.2} GB suggested", gb(total), c.title, c.items.len(), gb(picked));
        for i in c.items.iter().take(4) {
            let mark = if i.suggested { "x" } else { " " };
            println!("   [{mark}] {:>8.2} GB  {}  {}", gb(i.size), i.label, i.note.as_deref().unwrap_or(""));
        }
    }
    println!("unreadable: {}, took {:?}", r.unreadable, started.elapsed());
}
