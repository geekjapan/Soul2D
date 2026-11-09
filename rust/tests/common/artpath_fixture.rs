pub(crate) fn with_artpath(base: &str) -> String {
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
    base.replace("<CArtMeshSource xs.id='mesh'><ACDrawableSource xs.n='super'>",
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

pub(crate) fn two_forms(mut xml: String) -> String {
    let start = xml.find("<CArtPathForm>").unwrap();
    let end = start + xml[start..].find("</CArtPathForm>").unwrap() + "</CArtPathForm>".len();
    let other = xml[start..end].replace("<s xs.n='coordName'>Canvas</s>",
        "<s xs.n='coordName'>DeformerLocal</s>")
        .replace("<f xs.n='x'>1</f>", "<f xs.n='x'>11</f>")
        .replace("<f xs.n='x'>5</f>", "<f xs.n='x'>15</f>")
        .replace("<f xs.n='width'>3</f>", "<f xs.n='width'>7</f>");
    xml.insert_str(end, &other);
    xml.replace("<carray_list xs.n='keyforms' count='1'><CArtPathForm>",
        "<carray_list xs.n='keyforms' count='2'><CArtPathForm>")
}
