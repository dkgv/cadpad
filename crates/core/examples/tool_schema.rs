fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "protocol_version":cadpad_core::tools::PROTOCOL_VERSION,
            "tools":cadpad_core::tools::definitions()
        }))
        .unwrap()
    );
}
