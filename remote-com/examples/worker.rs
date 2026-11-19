//! Headless entry for the same transport used inside qccom.exe.
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    anyhow::ensure!(args.len() == 2 || args.len() == 3 && args[2] == "--relay-only", "worker PROFILE LOOPBACK_PORT [--relay-only]");
    qccom_tunnel::run_worker(args[0].as_ref(), args[1].parse()?, args.len() == 3)
}
