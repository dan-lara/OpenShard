use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // Use a bundled protoc if one isn't already provided in the environment.
    // This means protoc does not need to be installed separately on any platform.
    if env::var("PROTOC").is_err() {
        let protoc = protoc_bin_vendored::protoc_bin_path().unwrap();
        env::set_var("PROTOC", protoc);
    }

    tonic_build::configure()
        .file_descriptor_set_path(out_dir.join("tunnel_descriptor.bin"))
        .compile(&["proto/tunnel.proto"], &["proto"])?;
    Ok(())
}
