pub fn xml(guide: bool, id: &str, named: &str) -> String {
    if guide { return guide_xml(); }
    let xml = include_str!("../fixtures/can3-image-track.xml")
        .replace("<array xs.n=\"effectList\" count=\"1\">",
            "<array xs.n=\"effectList\" count=\"1\" type=\"ICMvEffect\">")
        .replace("<CMvEffect_VisualDefault xs.id=\"image-visual\">", &format!(
            "<CMvEffect_VisualDefault xs.id=\"image-visual\"><CMvAttrF xs.n=\"{named}\" xs.ref=\"image-opacity\"/>"))
        .replace("idstr=\"opacity\"", &format!("idstr=\"{id}\""))
        .replace("<CMutableSequence xs.n=\"valueData\"><CMvAttrF xs.n=\"attr\" xs.ref=\"image-opacity\"/>",
            "<CMutableSequence xs.n=\"valueData\"><ACValueSequence xs.n=\"super\"><d xs.n=\"curMin\">0</d><d xs.n=\"curMax\">0</d><i xs.n=\"keyMin\">0</i><i xs.n=\"keyMax\">0</i><d xs.n=\"baseValue\">1</d></ACValueSequence><CMvAttrF xs.n=\"attr\" xs.ref=\"image-opacity\"/>");
    xml
}

fn guide_xml() -> String {
    let mut xml = include_str!("../fixtures/can3-image-track.xml").to_owned();
    let parsed = roxmltree::Document::parse(&xml).unwrap();
    let mut ranges: Vec<_> = ["image-visual", "image-xy", "image-opacity", "image-frame"].into_iter()
        .map(|id| parsed.descendants().find(|node| node.attribute("xs.id") == Some(id)).unwrap().range()).collect();
    ranges.sort_by_key(|range| std::cmp::Reverse(range.start));
    for range in ranges { xml.replace_range(range, ""); }
    let mut definitions = String::new();
    let mut members = String::new();
    let mut names = String::new();
    for (id, name, tag, sequence, field) in [
        ("xy", "attrXY", "CMvAttrPt", "CXY_TNSSequence", "valueDataXY"),
        ("scalex", "attrScaleX", "CMvAttrF", "CMutableSequence", "valueData"),
        ("scaley", "attrScaleY", "CMvAttrF", "CMutableSequence", "valueData"),
        ("rotate", "attrRotate", "CMvAttrF", "CMutableSequence", "valueData"),
        ("shear", "attrShear", "CMvAttrF", "CMutableSequence", "valueData"),
        ("anchor", "attrAnchorXY", "CMvAttrPt", "CXY_TNSSequence", "valueDataXY"),
        ("opacity", "attrOpacity", "CMvAttrF", "CMutableSequence", "valueData"),
        ("artPathWidth", "attrArtPathWidth", "CMvAttrF", "CMutableSequence", "valueData"),
        ("frameStep", "attrFrameStep", "CMvAttrI", "CIntSequence", "valueData"),
        ("listed", "", "CMvAttrF", "CMutableSequence", "valueData"),
        ("named", "", "CMvAttrF", "CMutableSequence", "valueData"),
    ] {
        if !name.is_empty() {
            members.push_str(&format!("<{tag} xs.ref=\"image-{id}\"/>"));
            names.push_str(&format!("<{tag} xs.n=\"{name}\" xs.ref=\"image-{id}\"/>"));
        }
        let identity = if name.is_empty() { "guideimageno" } else { id };
        let curves = if tag == "CMvAttrF" { "<array xs.n=\"curveTypes\" count=\"0\"/>" } else { "" };
        definitions.push_str(&format!(r#"<{tag} xs.id="image-{id}"><ICMvAttr xs.n="super">
            <CAttrId xs.n="id" idstr="{identity}"/><CMvTrack_Image_Source xs.n="track" xs.ref="image-track"/></ICMvAttr>
            <{sequence} xs.n="{field}"><ACValueSequence xs.n="super"><d xs.n="curMin">0</d><d xs.n="curMax">0</d>
            <i xs.n="keyMin">0</i><i xs.n="keyMax">0</i><d xs.n="baseValue">1</d></ACValueSequence>
            <{tag} xs.n="attr" xs.ref="image-{id}"/><array xs.n="points" count="0"/>{curves}</{sequence}></{tag}>"#));
    }
    definitions.push_str(&format!(r#"<CMvEffect_VisualDefault xs.id="image-visual"><ICMvEffect xs.n="super">
        <CEffectId xs.n="id" idstr="VisualDefault"/><CMvTrack_Image_Source xs.n="track" xs.ref="image-track"/>
        <array xs.n="attrList" count="9" type="ICMvAttr">{members}</array><hash_map xs.n="attrMap" count="0" keyType="string"/>
        </ICMvEffect>{names}</CMvEffect_VisualDefault>
        <CMvEffect_GuideImageDefault xs.id="guide-effect"><ICMvEffect xs.n="super"><CEffectId xs.n="id" idstr="GuideImageDefault"/>
        <CMvTrack_Image_Source xs.n="track" xs.ref="image-track"/><array xs.n="attrList" count="1" type="ICMvAttr">
        <CMvAttrF xs.ref="image-listed"/></array><hash_map xs.n="attrMap" count="0" keyType="string"/></ICMvEffect>
        <CMvAttrF xs.n="attrImageNo" xs.ref="image-named"/></CMvEffect_GuideImageDefault>"#));
    xml.replace("<array xs.n=\"effectList\" count=\"1\">",
            "<array xs.n=\"effectList\" count=\"2\" type=\"ICMvEffect\"><CMvEffect_GuideImageDefault xs.ref=\"guide-effect\"/>")
        .replace("<CMvEffect_VisualDefault xs.n=\"visualEffect\" xs.ref=\"image-visual\"/>",
            "<CMvEffect_VisualDefault xs.n=\"visualEffect\" xs.ref=\"image-visual\"/><CMvEffect_GuideImageDefault xs.n=\"guideImageEffect\" xs.ref=\"guide-effect\"/>")
        .replace("</shared>", &format!("{definitions}</shared>"))
        .replace("CMvTrack_Image_Source", "CMvTrack_GuideImage_Source")
        .replace("CResource_Linked_Image", "CResource_Linked_GuideImages")
}
