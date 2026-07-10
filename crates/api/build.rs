// Force Cargo to watch the migrations folder.
// Without this: adding a new .sql file WITHOUT changing any .rs file doesn't trigger
// a recompile, so the migrate! macro keeps using the old migration set and your new
// migration "silently" gets left out. This is a confusing bug that would only bite in
// Phase 4/5, so we put the guard in place now (a real, near-term need, not a guess).
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
