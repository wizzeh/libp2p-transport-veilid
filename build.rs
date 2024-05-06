fn main() {
    let proto_root = format!("{}/proto", std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = format!("{}/generated", proto_root);

    println!("proto_root {}", proto_root);
    println!("out_dir {}", out_dir);

    prost_build::Config::new()
        .out_dir(out_dir)
        .compile_protos(&[format!("{}/payload.proto", proto_root)], &[&proto_root])
        .unwrap();
}
