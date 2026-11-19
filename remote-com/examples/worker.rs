//! A headless entry for integration tests of the same transport used inside QCCOM.
use anyhow::{ensure, Result};
use qccom_tunnel::mapping::{Mapping, Node};
use std::{path::Path, sync::Arc};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if let [mode, profile, routes, rest @ ..] = args.as_slice() {
        if mode == "mappings" {
            ensure!(rest.is_empty() || rest == ["--relay-only"], "invalid test options");
            let invite = serde_json::from_slice(&std::fs::read(profile)?)?;
            let maps: Vec<Mapping> = serde_json::from_slice(&std::fs::read(routes)?)?;
            return tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?.block_on(async {
                let node = Arc::new(Node::bind(invite, !rest.is_empty()).await?);
                let mut jobs = tokio::task::JoinSet::new();
                for m in maps {
                    let name = m.service.title();
                    let node = node.clone();
                    jobs.spawn(async move { node.run(m, Arc::new(move |s| println!("{name} {s:?}"))).await });
                }
                jobs.join_next().await.expect("at least one mapping")?
            });
        }
    }
    ensure!(args.len() == 2 || args.len() == 3 && args[2] == "--relay-only", "worker PROFILE LOOPBACK_PORT [--relay-only]");
    qccom_tunnel::run_worker(Path::new(&args[0]), args[1].parse()?, args.len() == 3)
}
