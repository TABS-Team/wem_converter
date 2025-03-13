use wem_converter::wwriff::{WwiseRiffVorbis, ForcePacketFormat};

fn main() {
    let input_wem = "input.wem";
    let codebooks_file = "bin/packed_codebooks.bin";
    let mut vorbis = match WwiseRiffVorbis::new(
        input_wem,
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