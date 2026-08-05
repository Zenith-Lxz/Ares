fn main() -> anyhow::Result<()> {
    println!("ARES {}", ares_core::version());
    Ok(())
}
