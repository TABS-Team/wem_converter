use wem_converter::wwriff::{WwiseRiffVorbis, ForcePacketFormat};
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt, Registry};
use std::fs;
use std::io::Cursor;

fn main() {
    let subscriber = Registry::default()
        .with(ErrorLayer::default())
        .with(tracing_subscriber::fmt::Layer::default());
    tracing::subscriber::set_global_default(subscriber)
        .expect("Failed to set global subscriber");

    let input_wem = "input.wem";
    let codebooks_file = "bin/packed_codebooks.bin";

    let buffer = match fs::read(input_wem) {
        Ok(data) => Cursor::new(data),
        Err(e) => {
            eprintln!("Error reading input file {}: {:?}", input_wem, e);
            return;
        }
    };


    let mut vorbis = match WwiseRiffVorbis::<Cursor<Vec<u8>>>::new(
        buffer,
        "input.ogg",
        codebooks_file,
        false,
        false,
        ForcePacketFormat::ModPackets,
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error creating WwiseRiffVorbis: {:?}", e);
            return;
        }
    };

    vorbis.print_info();
    if let Err(e) = vorbis.generate_ogg() {
        eprintln!("Error generating OGG file: {:?}", e);
    } else {
        println!("OGG file generated successfully!");
    }
}