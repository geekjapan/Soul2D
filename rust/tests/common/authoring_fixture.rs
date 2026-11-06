pub(super) const LARGE_TEXTURE_SIZE: u32 = 2048;
pub(super) const LARGE_TEXTURE_COLORS: [[u8; 4]; 4] = [
    [220, 40, 20, 255], [20, 190, 40, 255], [30, 50, 210, 255], [190, 40, 180, 255],
];

pub(super) fn large_texture_fixture() -> (String, Vec<Vec<u8>>) {
    const SIZE: u32 = LARGE_TEXTURE_SIZE;
    let colors = LARGE_TEXTURE_COLORS;
    let pngs: Vec<Vec<u8>> = colors.iter().map(|color| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(SIZE, SIZE, image::Rgba(*color)))
            .write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }).collect();
    let base = native_artmesh_xml("<f xs.n='opacity'>1</f>");
    let parsed = roxmltree::Document::parse(&base).unwrap();
    let source = &base[parsed.descendants().find(|node| node.attribute("xs.id") == Some("mesh")).unwrap().range()];
    let texture = &base[parsed.descendants().find(|node| node.attribute("xs.id") == Some("texture")).unwrap().range()];
    let sources: String = (0..4).map(|i| {
        let (x, y) = ((i % 2) * 2, (i / 2) * 2);
        source.replace("'mesh'", &format!("'mesh{i}'")).replace("mesh-guid", &format!("mesh-guid-{i}"))
            .replace("form-guid", &format!("form-guid-{i}"))
            .replace("xs.ref='texture'", &format!("xs.ref='texture{i}'"))
            .replace("1 2 3 4 5 6", &format!("{x} {y} {} {y} {x} {}", x + 2, y + 2))
    }).collect();
    let textures: String = (0..4).map(|i| texture.replace("'texture'", &format!("'texture{i}'"))
        .replace("texture-guid", &format!("texture-guid-{i}"))
        .replace("width='2' height='3'", &format!("width='{SIZE}' height='{SIZE}'"))
        .replace("mesh.png", &if i == 0 { "mesh.png".into() } else { format!("tex{}.png", i - 1) })).collect();
    let xml = base.replace(source, &sources).replace(texture, &textures)
        .replace("count='1'><CArtMeshSource xs.ref='mesh'/>", &format!("count='4'>{}",
            (0..4).map(|i| format!("<CArtMeshSource xs.ref='mesh{i}'/>")).collect::<String>()));
    (xml, pngs)
}

pub(super) fn native_artmesh_xml(opacity_field: &str) -> String {
    format!(
        r#"<root fileFormatVersion='401000004'><CModelSource>
      <s xs.n='name'>Fixture</s><CImageCanvas xs.n='canvas'><i xs.n='pixelWidth'>100</i><i xs.n='pixelHeight'>100</i></CImageCanvas>
      <CParameterSourceSet xs.n='parameterSourceSet'><carray_list xs.n='_sources' count='1'><CParameterSource><CParameterGuid xs.n='guid' uuid='parameter-guid'/><CParameterId xs.n='id' idstr='ParamTest'/><f xs.n='minValue'>-1</f><f xs.n='maxValue'>1</f><f xs.n='defaultValue'>0</f></CParameterSource></carray_list></CParameterSourceSet>
      <CDrawableSourceSet xs.n='drawableSourceSet'><carray_list xs.n='_sources' count='1'><CArtMeshSource xs.ref='mesh'/></carray_list></CDrawableSourceSet>
      <CDeformerSourceSet xs.n='deformerSourceSet'><carray_list xs.n='_sources' count='0'/></CDeformerSourceSet>
      <CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/></carray_list></CPartSourceSet>
      <CPartSource xs.n='rootPart' xs.ref='part'/>
    </CModelSource>
    <CArtMeshSource xs.id='mesh'><ACDrawableSource xs.n='super'><CArtMeshGuid xs.n='guid' uuid='mesh-guid'/><CArtMeshId xs.n='id' idstr='mesh'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/></ACDrawableSource>
      <KeyformGridSource xs.n='keyformGridSource'><array_list xs.n='keyformBindings' count='0'/><array_list xs.n='keyformsOnGrid' count='1'><KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='0'/></KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='form-guid'/></KeyformOnGrid></array_list></KeyformGridSource>
      <float-array xs.n='positions' count='6'>0 0 1 0 1 1</float-array><float-array xs.n='uvs' count='6'>0 0 1 0 1 1</float-array><int-array xs.n='indices' count='3'>0 1 2</int-array>
      <GTexture2D xs.n='texture' xs.ref='texture'/><carray_list xs.n='keyforms' count='1'><CArtMeshForm><ACDrawableForm xs.n='super'><ACForm xs.n='super'>
        <CFormGuid xs.n='guid' uuid='form-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/>
      </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType>{opacity_field}<CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>
      </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>
    </CArtMeshSource><CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource>
    <GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode></GTexture>
      <CImageResource xs.n='srcImageResource' width='2' height='3'><file xs.n='imageFileBuf' path='mesh.png'/></CImageResource><null xs.n='transformImageResource01toLogical01'/>
    </GTexture2D><Opaque xs.id='future-data'>keep-this-xml</Opaque></root>"#
    )
}

pub(super) fn authoring_bytes_with_pngs(xml: &str, pngs: &[Vec<u8>]) -> Vec<u8> {
    use flate2::write::DeflateEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(xml.as_bytes()).unwrap();
    let compressed = encoder.finish().unwrap();
    let mut zip = Vec::new();
    zip.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0x0808u16.to_le_bytes());
    zip.extend_from_slice(&8u16.to_le_bytes());
    zip.extend_from_slice(&[0; 4]);
    zip.extend_from_slice(&[0; 12]);
    zip.extend_from_slice(&8u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(b"contents");
    zip.extend_from_slice(&compressed);
    zip.extend_from_slice(b"PK\x07\x08");
    zip.extend_from_slice(&crc32fast::hash(xml.as_bytes()).to_le_bytes());
    zip.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
    zip.extend_from_slice(&(xml.len() as u32).to_le_bytes());

    let seed = 0x562e_099f_u32;
    let mask = seed as u8;
    let names: Vec<String> = (0..pngs.len() - 1).map(|i| format!("tex{i}.png")).collect();
    let mut entries = vec![
        ("main.xml", "main_xml", [0x01u8, 0x21], zip),
        ("mesh.png", "", [0x01, 0x10], pngs[0].clone()),
        ("opaque.bin", "", [0x01, 0x10], vec![0, 1, 2, 0xff]),
    ];
    entries.extend(names.iter().zip(&pngs[1..]).map(|(name, png)| (name.as_str(), "", [0x01, 0x10], png.clone())));
    let mut bytes = vec![0; 58];
    bytes[..4].copy_from_slice(b"CAFF");
    bytes[4..8].copy_from_slice(&45u32.to_be_bytes());
    bytes[14..18].copy_from_slice(&seed.to_be_bytes());
    bytes[54..58].copy_from_slice(&((entries.len() as u32) ^ seed).to_be_bytes());
    let mut fields = Vec::new();
    for (name, alias, flags, _) in &entries {
        bytes.push(name.len() as u8 ^ mask);
        bytes.extend(name.as_bytes().iter().map(|byte| byte ^ mask));
        bytes.push(alias.len() as u8 ^ mask);
        bytes.extend(alias.as_bytes().iter().map(|byte| byte ^ mask));
        bytes.extend_from_slice(&(0x1234_5678u32 ^ seed).to_be_bytes());
        let offset_field = bytes.len();
        bytes.extend_from_slice(&seed.to_be_bytes());
        let size_field = bytes.len();
        bytes.extend_from_slice(&seed.to_be_bytes());
        bytes.extend(flags.iter().map(|byte| byte ^ mask));
        bytes.extend([0; 8].iter().map(|byte| byte ^ mask));
        fields.push((offset_field, size_field));
    }
    for (index, (_, _, _, payload)) in entries.iter().enumerate() {
        let offset = bytes.len();
        bytes[fields[index].0..fields[index].0 + 4]
            .copy_from_slice(&((offset as u32 ^ seed).to_be_bytes()));
        bytes[fields[index].1..fields[index].1 + 4]
            .copy_from_slice(&((payload.len() as u32 ^ seed).to_be_bytes()));
        bytes.extend(payload.iter().map(|byte| byte ^ mask));
    }
    bytes.extend_from_slice(b"bc");
    bytes
}
