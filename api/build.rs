// Rebuild when migrations change so `sqlx::migrate!` embeds new files.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
