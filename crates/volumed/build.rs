use tonic_build::compile_protos;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Automatically generate csi spec structures in Rust using tonic
    compile_protos("../../protos/volumed.proto")?;

    Ok(())
}
