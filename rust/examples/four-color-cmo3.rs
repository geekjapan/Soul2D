#[path = "../tests/common/authoring_fixture.rs"]
mod authoring_fixture;
#[cfg(test)]
#[path = "../tests/common/mod.rs"]
mod common;

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

fn write_fixture(destination: &Path) -> io::Result<()> {
    let (xml, pngs) = authoring_fixture::large_texture_fixture();
    let bytes = authoring_fixture::authoring_bytes_with_pngs(&xml, &pngs);
    let mut file = OpenOptions::new().write(true).create_new(true).open(destination)?;
    file.write_all(&bytes)?;
    file.flush()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let destination = arguments.next().ok_or("usage: four-color-cmo3 OUTPUT.cmo3")?;
    if arguments.next().is_some() {
        return Err("usage: four-color-cmo3 OUTPUT.cmo3".into());
    }
    write_fixture(Path::new(&destination))?;
    println!("Wrote {}", Path::new(&destination).display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soul2d_core::caff::CaffDocument;

    #[test]
    fn generated_file_matches_the_fixed_fixture_and_preserves_every_texture() {
        let directory = common::scratch(std::env::temp_dir().join(format!(
            "soul2d-four-color-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        )));
        std::fs::create_dir(&directory).unwrap();
        let destination = directory.join("four-color-日本語.cmo3");
        write_fixture(&destination).unwrap();
        let bytes = std::fs::read(&destination).unwrap();
        let (xml, pngs) = authoring_fixture::large_texture_fixture();
        assert_eq!(bytes, authoring_fixture::authoring_bytes_with_pngs(&xml, &pngs));
        let container = CaffDocument::parse(&bytes).unwrap();
        assert_eq!(container.encode(), bytes);
        assert_eq!(container.main_xml(), xml.as_bytes());
        assert_eq!(container.entries().len(), 6);
        assert_eq!(container.entry_data("opaque.bin").unwrap(), vec![0, 1, 2, 255]);
        let parsed = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(parsed.descendants()
            .filter(|n| n.has_tag_name("GTexture2D") && n.attribute("xs.id").is_some()).count(), 4);
        for (i, color) in authoring_fixture::LARGE_TEXTURE_COLORS.iter().enumerate() {
            let name = if i == 0 { "mesh.png".into() } else { format!("tex{}.png", i - 1) };
            let png = container.entry_data(&name).unwrap();
            assert_eq!(png, pngs[i]);
            let image = image::load_from_memory(&png).unwrap().into_rgba8();
            assert_eq!(image.dimensions(),
                (authoring_fixture::LARGE_TEXTURE_SIZE, authoring_fixture::LARGE_TEXTURE_SIZE));
            assert!(image.pixels().all(|pixel| pixel.0 == *color));
        }
    }
}
