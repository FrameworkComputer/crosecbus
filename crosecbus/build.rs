fn main() -> anyhow::Result<()> {
    Ok(wdk_build::configure_wdk_binary_build()?)
}
