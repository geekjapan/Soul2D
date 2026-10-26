use roxmltree::Document;
use serde_json::json;
use std::fs;
use soul2d_core::caff::CaffDocument;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.len() != 2 {
        return Err("usage: caff-roundtrip INPUT UNUSED_OUTPUT_DIRECTORY".into());
    }
    let source = fs::read(&arguments[0])?;
    let container = CaffDocument::parse(&source)?;
    assert_eq!(container.encode(), source);
    let original_xml = String::from_utf8(container.main_xml())?;
    let document = Document::parse(&original_xml)?;
    let parameter = document.descendants().find(|node| {
        node.has_tag_name("CParameterSource")
            && node
                .children()
                .any(|child| child.attribute("idstr") == Some("ParamAngleX"))
    });
    let (edited_xml, edit) = if let Some(parameter) = parameter {
        let field = |name| {
            parameter
                .children()
                .find(|child| child.attribute("xs.n") == Some(name))
                .unwrap()
        };
        let default = field("defaultValue");
        let old: f64 = default.text().unwrap().parse()?;
        let minimum: f64 = field("minValue").text().unwrap().parse()?;
        let maximum: f64 = field("maxValue").text().unwrap().parse()?;
        let new = if old < maximum { maximum } else { minimum };
        assert_ne!(old, new);
        let span = default.range();
        let element = &original_xml[span.clone()];
        let text_start = span.start + element.find('>').unwrap() + 1;
        let text_end = span.start + element.rfind('<').unwrap();
        let mut xml = original_xml.clone();
        xml.replace_range(text_start..text_end, &new.to_string());
        (
            xml,
            json!({"parameter":"ParamAngleX", "oldDefault":old, "newDefault":new}),
        )
    } else {
        (
            format!("{original_xml}\n<!-- VSoul CAFF roundtrip -->\n"),
            json!({"xmlComment":true}),
        )
    };
    Document::parse(&edited_xml)?;
    let encoded = container.replace_main_xml(edited_xml.as_bytes())?;
    let reopened = CaffDocument::parse(&encoded)?;
    assert_eq!(reopened.main_xml(), edited_xml.as_bytes());
    assert_eq!(container.entries().len(), reopened.entries().len());
    for (original, new) in container.entries().iter().zip(reopened.entries()) {
        assert_eq!(original.name, new.name);
        assert_eq!(original.alias, new.alias);
        assert_eq!(original.unknown, new.unknown);
        assert_eq!(original.flags, new.flags);
        assert_eq!(original.reserved, new.reserved);
        if original.name != "main.xml" {
            assert_eq!(
                container.entry_data(&original.name),
                reopened.entry_data(&new.name)
            );
        }
    }
    assert_eq!(fs::read(&arguments[0])?, source);
    fs::create_dir(&arguments[1])?;
    let directory = std::path::Path::new(&arguments[1]);
    fs::write(directory.join("rewritten.caff"), encoded)?;
    fs::write(directory.join("original.xml"), original_xml)?;
    fs::write(directory.join("edited.xml"), edited_xml)?;
    println!(
        "{}",
        json!({"status":"PASS", "entries":container.entries().len(),
        "unchangedByteExact":true, "otherPayloadsExact":true, "edit":edit})
    );
    Ok(())
}
