//! Test helpers shared by the editor modules: a headless egui frame that records the accessibility tree
//! (so tests can find a control by role and name, click it, and check that every control has a name), and a
//! small CMO3 document for the Live2D tests.

use eframe::egui;
use egui::accesskit::{Node, NodeId, Role};
use std::collections::HashMap;

pub type Out = Vec<(NodeId, Node)>;

pub struct Harness {
    pub ctx: egui::Context,
    all: HashMap<NodeId, Node>,
    root: Option<NodeId>,
    /// Applied to the raw input of the next frame only (a close request, dropped files, a window size).
    pub next_input: Option<Box<dyn FnOnce(&mut egui::RawInput)>>,
    /// The screen size; tall panels need a taller screen for their controls to be reachable.
    pub screen: egui::Vec2,
    pub close_requests: usize,
}

const CONTROLS: [Role; 7] =
    [Role::Button, Role::CheckBox, Role::ComboBox, Role::SpinButton, Role::TextInput, Role::MultilineTextInput, Role::Slider];

impl Harness {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        Self { ctx, all: HashMap::new(), root: None, next_input: None, screen: egui::vec2(900.0, 900.0), close_requests: 0 }
    }

    /// Runs one frame and returns the accessibility tree as the screen reader would see it.
    pub fn frame(&mut self, events: Vec<egui::Event>, mut run: impl FnMut(&egui::Context)) -> Out {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)),
            events,
            ..Default::default()
        };
        if let Some(tweak) = self.next_input.take() {
            tweak(&mut input);
        }
        let output = self.ctx.run(input, |ctx| run(ctx));
        self.close_requests += output.viewport_output.values().flat_map(|viewport| viewport.commands.iter())
            .filter(|command| matches!(command, egui::ViewportCommand::Close)).count();
        let update = output.platform_output.accesskit_update.clone().expect("accesskit is enabled");
        if let Some(tree) = &update.tree {
            self.root = Some(tree.root);
        }
        for (id, node) in update.nodes {
            self.all.insert(id, node);
        }
        let mut out = Vec::new();
        let mut stack = vec![self.root.expect("a root")];
        while let Some(id) = stack.pop() {
            if let Some(node) = self.all.get(&id) {
                stack.extend(node.children().iter().copied());
                out.push((id, node.clone()));
            }
        }
        out
    }

    /// Lays the UI out, clicks the control and returns the frame after the click.
    pub fn click(&mut self, role: Role, label: &str, mut run: impl FnMut(&egui::Context)) -> Out {
        self.frame(Vec::new(), &mut run);
        let laid_out = self.frame(Vec::new(), &mut run);
        let at = center(&laid_out, role, label);
        let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        self.frame(vec![egui::Event::PointerMoved(at), press(true), press(false)], &mut run);
        self.frame(Vec::new(), &mut run)
    }
}

/// The accessible name of a node: its label, else the label of the node that labels it, else its value.
pub fn name_of(output: &Out, node: &Node) -> Option<String> {
    if let Some(label) = node.label().filter(|label| !label.is_empty()) {
        return Some(label.to_owned());
    }
    for id in node.labelled_by() {
        if let Some((_, labelling)) = output.iter().find(|(candidate, _)| candidate == id) {
            if let Some(label) = labelling.label().or_else(|| labelling.value()) {
                return Some(label.to_owned());
            }
        }
    }
    node.value().map(str::to_owned)
}

pub fn find(output: &Out, role: Role, label: &str) -> Option<Node> {
    output
        .iter()
        .find(|(_, node)| {
            node.role() == role && name_of(output, node).is_some_and(|name| name == label || name.starts_with(&format!("{label} ")))
        })
        .map(|(_, node)| node.clone())
}

pub fn center(output: &Out, role: Role, label: &str) -> egui::Pos2 {
    let node = find(output, role, label).unwrap_or_else(|| {
        let have: Vec<_> = output.iter().map(|(_, n)| (n.role(), n.label().map(str::to_owned))).collect();
        panic!("no {role:?} named {label:?}; have {have:?}")
    });
    let bounds = node.bounds().expect("bounds");
    egui::pos2(((bounds.x0 + bounds.x1) / 2.0) as f32, ((bounds.y0 + bounds.y1) / 2.0) as f32)
}

pub fn has(output: &Out, role: Role, label: &str) -> bool {
    find(output, role, label).is_some()
}

pub fn texts(output: &Out) -> Vec<String> {
    output.iter().flat_map(|(_, node)| [node.label(), node.value()]).flatten().map(str::to_owned).collect()
}

/// Every interactive control has a name; returns how many were checked.
pub fn assert_all_named(output: &Out, context: &str) -> usize {
    let mut count = 0;
    for (_, node) in output {
        if CONTROLS.contains(&node.role()) {
            count += 1;
            let named = node.label().is_some_and(|label| !label.is_empty()) || !node.labelled_by().is_empty();
            assert!(named, "{context}: unlabelled {:?} control {:?}", node.role(), node.value());
        }
    }
    count
}

// ------------------------------------------------------------------------------------------ CMO3 fixture

/// A CMO3 with one ArtMesh (`mesh-guid`) and one Canvas form (`form-guid`) of three vertices and opacity 0.75.
pub fn cmo3_bytes() -> Vec<u8> {
    caff_with_xml(CMO3_XML)
}

/// Existing ArtPath metadata layouts with two listed brushes, packaged by `caff_with_xml`.
pub(crate) fn artpath_cmo3_xml() -> String {
    let point = |guid: &str, x: i32| format!("<CArtPathPoint><CArtPathPointGuid xs.n='ctrlPtId' uuid='{guid}'/>\
        <CSplineCurvePoint xs.n='curvePointPosition'><GVector2 xs.n='point'><f xs.n='x'>{x}</f><f xs.n='y'>2</f></GVector2>\
        <GVector2 xs.n='startVelocity'><f xs.n='x'>0</f><f xs.n='y'>0</f></GVector2>\
        <GVector2 xs.n='endVelocity'><f xs.n='x'>0</f><f xs.n='y'>0</f></GVector2></CSplineCurvePoint>\
        <b xs.n='isCorner'>false</b><f xs.n='colorRed'>1</f><f xs.n='colorGreen'>1</f><f xs.n='colorBlue'>1</f>\
        <f xs.n='colorAlpha'>1</f><f xs.n='opacity'>1</f><f xs.n='width'>3</f></CArtPathPoint>");
    let path = format!("<CArtPathSource xs.id='path'><ACDrawableSource xs.n='super'>\
        <CDrawableGuid xs.n='guid' uuid='path-guid'/><CDrawableId xs.n='id' idstr='path'/><s xs.n='localName'>Path</s>\
        <CPartGuid xs.n='parentGuid' uuid='part-guid'/><carray_list xs.n='clipGuidList' count='0'/>\
        <b xs.n='invertClippingMask'>false</b></ACDrawableSource><CArtPathBrushGuid xs.n='brushGuid' uuid='brush-guid'/>\
        <LineAlgorithm xs.n='lineAlgorithm' v='LCNS'/><LineAlignment xs.n='lineAlignment' v='CENTER'/>\
        <b xs.n='isClosed'>false</b><i xs.n='divNum'>20</i>\
        <carray_list xs.n='keyforms' count='1'><CArtPathForm><ACDrawableForm xs.n='super'>\
        <i xs.n='drawOrder'>950</i><f xs.n='opacity'>1</f><ACForm xs.n='super'>\
        <CFormGuid xs.n='guid' uuid='path-form'/><CArtPathSource xs.n='_source' xs.ref='path'/></ACForm>\
        <CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType></ACDrawableForm>\
        <carray_list xs.n='positions' count='2'>{}{}</carray_list></CArtPathForm></carray_list></CArtPathSource>",
        point("point-a", 1), point("point-b", 5));
    CMO3_XML.replace("<CArtMeshSource xs.id='mesh'><ACDrawableSource xs.n='super'>",
        "<CArtMeshSource xs.id='mesh'><ACDrawableSource xs.n='super'><s xs.n='localName'>Mesh</s>")
        .replace("<carray_list xs.n='_sources' count='1'><CArtMeshSource xs.ref='mesh'/></carray_list>",
        "<carray_list xs.n='_sources' count='2'><CArtMeshSource xs.ref='mesh'/><CArtPathSource xs.ref='path'/></carray_list>")
        .replace("</CArtMeshSource><CPartSource xs.id='part'>", &format!("</CArtMeshSource>{path}<CPartSource xs.id='part'>"))
        .replace("<CPartId xs.n='id' idstr='root'/>", "<CPartId xs.n='id' idstr='root'/>\
            <carray_list xs.n='_childGuids' count='2'><CArtMeshGuid uuid='mesh-guid'/><CDrawableGuid uuid='path-guid'/></carray_list>")
        .replace("</CModelSource>", "<CArtPathBrushSetting xs.n='artPathBrushesSetting'><carray_list xs.n='brushes' count='2'>\
            <CArtPathBrush><CArtPathBrushGuid xs.n='guid' uuid='brush-guid'/><CArtPathBrushId xs.n='id' idstr='Brush'/></CArtPathBrush>\
            <CArtPathBrush><CArtPathBrushGuid xs.n='guid' uuid='brush-b'/><CArtPathBrushId xs.n='id' idstr='BrushB'/></CArtPathBrush>\
            </carray_list></CArtPathBrushSetting></CModelSource>")
}

/// `cmo3_bytes` with the ArtMesh keyed at -1 and 1 on `ParamTest`.
pub fn keyed_cmo3_bytes() -> Vec<u8> {
    let xml = CMO3_XML
        .replace("<array_list xs.n='keyformBindings' count='0'/><array_list xs.n='keyformsOnGrid' count='1'><KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='0'/></KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='form-guid'/></KeyformOnGrid></array_list>",
            "<array_list xs.n='keyformBindings' count='1'><KeyformBindingSource xs.id='binding'><CParameterGuid xs.n='parameterGuid' uuid='parameter-guid'/>\
             <array_list xs.n='keys' count='2'><f>-1</f><f>1</f></array_list><InterpolationType xs.n='interpolationType' v='LINEAR'/>\
             <ExtendedInterpolationType xs.n='extendedInterpolationType' v='LINEAR'/><i xs.n='insertPointCount'>0</i>\
             <f xs.n='extendedInterpolationScale'>1</f></KeyformBindingSource></array_list><array_list xs.n='keyformsOnGrid' count='2'>\
             <KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='1'><KeyOnParameter>\
             <KeyformBindingSource xs.n='binding' xs.ref='binding'/><i xs.n='keyIndex'>0</i></KeyOnParameter></array_list>\
             </KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='form-guid'/></KeyformOnGrid>\
             <KeyformOnGrid><KeyformGridAccessKey xs.n='accessKey'><array_list xs.n='_keyOnParameterList' count='1'><KeyOnParameter>\
             <KeyformBindingSource xs.n='binding' xs.ref='binding'/><i xs.n='keyIndex'>1</i></KeyOnParameter></array_list>\
             </KeyformGridAccessKey><CFormGuid xs.n='keyformGuid' uuid='upper-form-guid'/></KeyformOnGrid></array_list>")
        .replace("<carray_list xs.n='keyforms' count='1'><CArtMeshForm>", "<carray_list xs.n='keyforms' count='2'><CArtMeshForm>")
        .replace("<float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>",
            "<float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm><CArtMeshForm><ACDrawableForm xs.n='super'>\
             <ACForm xs.n='super'><CFormGuid xs.n='guid' uuid='upper-form-guid'/><CArtMeshSource xs.n='_source' xs.ref='mesh'/></ACForm>\
             <CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType><f xs.n='opacity'>0.75</f>\
             <CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/></ACDrawableForm>\
             <float-array xs.n='positions' count='6'>1 2 30 40 5 6</float-array></CArtMeshForm></carray_list>");
    assert!(xml.contains("upper-form-guid'/></KeyformOnGrid>") && xml.contains("uuid='upper-form-guid'/><CArtMeshSource"));
    caff_with_xml(&xml)
}

/// `cmo3_bytes` with an empty Part `arm` next to the ArtMesh under the root Part, so the root lists both children.
pub fn two_part_cmo3_bytes() -> Vec<u8> {
    let xml = CMO3_XML
        .replace("<CPartId xs.n='id' idstr='root'/>", "<CPartId xs.n='id' idstr='root'/>\
             <carray_list xs.n='_childGuids' count='2'><CArtMeshGuid uuid='mesh-guid'/><CPartGuid uuid='arm-guid'/></carray_list>")
        .replace("<CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='1'><CPartSource xs.ref='part'/>",
            "<CPartSourceSet xs.n='partSourceSet'><carray_list xs.n='_sources' count='2'><CPartSource xs.ref='part'/>\
             <CPartSource xs.ref='arm'/>")
        .replace("<GTexture2D xs.id='texture'>", "<CPartSource xs.id='arm'><CPartGuid xs.n='guid' uuid='arm-guid'/>\
             <CPartId xs.n='id' idstr='arm'/><CPartGuid xs.n='parentGuid' uuid='part-guid'/>\
             <carray_list xs.n='_childGuids' count='0'/><carray_list xs.n='keyforms' count='0'/></CPartSource>\
             <GTexture2D xs.id='texture'>");
    assert!(xml.contains("xs.ref='arm'/>") && xml.contains("xs.id='arm'") && xml.contains("uuid='arm-guid'/></carray_list>"));
    caff_with_xml(&xml)
}

/// One physics group on `ParamTest` (two vertices, one input, one output, FPS 60), frozen as
/// `rust/tests/fixtures/physics3-editor.cmo3` and shared with the physics3 proof.
pub fn cmo3_physics_bytes() -> Vec<u8> {
    include_bytes!("../tests/fixtures/physics3-editor.cmo3").to_vec()
}

const CMO3_XML: &str = r#"<root fileFormatVersion='401000004'><CModelSource>
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
      </ACForm><CoordType xs.n='coordType'><s xs.n='coordName'>Canvas</s></CoordType><f xs.n='opacity'>0.75</f><CFloatColor xs.n='multiplyColor' red='1' green='1' blue='1' alpha='1'/>
      </ACDrawableForm><float-array xs.n='positions' count='6'>1 2 3 4 5 6</float-array></CArtMeshForm></carray_list>
    </CArtMeshSource><CPartSource xs.id='part'><CPartGuid xs.n='guid' uuid='part-guid'/><CPartId xs.n='id' idstr='root'/><carray_list xs.n='keyforms' count='0'/></CPartSource>
    <GTexture2D xs.id='texture'><GTexture xs.n='super'><GTextureGuid xs.n='guid' uuid='texture-guid'/><FilterMode xs.n='filterMode'><MinFilter xs.n='minFilter' v='LINEAR'/><MagFilter xs.n='magFilter' v='LINEAR'/></FilterMode></GTexture>
      <CImageResource xs.n='srcImageResource' width='2' height='3'><file xs.n='imageFileBuf' path='mesh.png'/></CImageResource><null xs.n='transformImageResource01toLogical01'/>
    </GTexture2D><Opaque xs.id='future-data'>keep-this-xml</Opaque></root>"#;

pub(crate) fn caff_with_xml(xml: &str) -> Vec<u8> {
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
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 255, 255, 255])))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let entries = [
        ("main.xml", "main_xml", [0x01, 0x21], zip),
        ("mesh.png", "", [0x01, 0x10], png.into_inner()),
        ("opaque.bin", "", [0x01, 0x10], vec![0, 1, 2, 0xff]),
    ];
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
        bytes[fields[index].0..fields[index].0 + 4].copy_from_slice(&(offset as u32 ^ seed).to_be_bytes());
        bytes[fields[index].1..fields[index].1 + 4].copy_from_slice(&(payload.len() as u32 ^ seed).to_be_bytes());
        bytes.extend(payload.iter().map(|byte| byte ^ mask));
    }
    bytes.extend_from_slice(b"bc");
    bytes
}

/// A scratch directory unique to the test process and name.
pub fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("soul2d-editor-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // macOS reports temp files under /private; the editor compares canonical paths.
    std::fs::canonicalize(crate::scratch::scratch(dir)).unwrap()
}
