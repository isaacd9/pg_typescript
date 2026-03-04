use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR must be set"));
    let snapshot_path = out_dir.join("pg_typescript_runtime.snap");

    deno_runtime::snapshot::create_runtime_snapshot(
        snapshot_path,
        deno_runtime::ops::bootstrap::SnapshotOptions::default(),
        vec![],
    );
}
