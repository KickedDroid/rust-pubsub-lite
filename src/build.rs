fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::prost::compile_protos("src/pb")?;
    Ok(())
}