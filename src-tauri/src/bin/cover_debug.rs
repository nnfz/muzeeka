use lofty::file::TaggedFileExt;

fn main() {
    let path = r"Z:\[Psytrance, Frenchcore] Laur - Sound Chimera [Ou_udndPAO4].mp3";
    let out_dir = r"C:\Users\botni\AppData\Roaming\com.nnfz.muzeeka\covers";

    let tagged = lofty::read_from_path(path).expect("lofty read");
    let tag = id3::Tag::read_from_path(path).expect("id3 read");

    let lofty_data = tagged.tags().first().unwrap().pictures().first().unwrap().data();
    let id3_data = &tag.pictures().next().unwrap().data;

    std::fs::write(format!("{out_dir}\\laur_lofty.jpg"), lofty_data).expect("write lofty");
    std::fs::write(format!("{out_dir}\\laur_id3.jpg"), id3_data).expect("write id3");

    println!("lofty: {} bytes, first20={:02X?}", lofty_data.len(), &lofty_data[..20]);
    println!("id3:   {} bytes, first20={:02X?}", id3_data.len(), &id3_data[..20]);
    println!("lofty last8: {:02X?}", &lofty_data[lofty_data.len()-8..]);
    println!("id3   last8: {:02X?}", &id3_data[id3_data.len()-8..]);

    // Find first difference
    let min_len = lofty_data.len().min(id3_data.len());
    for i in 0..min_len {
        if lofty_data[i] != id3_data[i] {
            println!("First diff at byte {i}: lofty={:02X} id3={:02X}", lofty_data[i], id3_data[i]);
            println!("  context lofty: {:02X?}", &lofty_data[i.saturating_sub(4)..=(i+4).min(lofty_data.len()-1)]);
            println!("  context id3:   {:02X?}", &id3_data[i.saturating_sub(4)..=(i+4).min(id3_data.len()-1)]);
            break;
        }
    }

    // Check cached full cover
    let cached = std::fs::read(format!("{out_dir}\\2322e300e99f5093-embedded-full.jpg")).expect("read cached");
    println!("\ncached full: {} bytes", cached.len());
    println!("cached first20: {:02X?}", &cached[..20]);
}
