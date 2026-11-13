//! Uninstrumented view timing; run repeated alternating release processes.
#[path = "support/owned_view_workloads.rs"]
mod workloads;
fn reset_probe() {}
fn probe_counts() -> Option<(usize, usize)> {
    None
}
fn main() {
    workloads::main();
}
