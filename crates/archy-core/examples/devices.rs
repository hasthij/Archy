fn main() {
    for (label, input) in [("Microphones", true), ("Outputs", false)] {
        println!(
            "{label}: (default = {:?})",
            archy_audio::default_device_id(input)
        );
        for d in archy_audio::list_devices(input) {
            println!("  {}  [{}]", d.name, d.id);
        }
    }
}
