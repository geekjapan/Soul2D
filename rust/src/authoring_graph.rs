use roxmltree::{Document, Node};
use std::collections::{HashMap, HashSet};

const MAX_ALIAS_DEPTH: usize = 64;
const MAX_SUPER_DEPTH: usize = 64;

pub(crate) struct Graph<'doc, 'input: 'doc> {
    document: &'doc Document<'input>,
    ids: HashMap<&'doc str, Node<'doc, 'input>>,
    model: Option<Node<'doc, 'input>>,
}

impl<'doc, 'input: 'doc> Graph<'doc, 'input> {
    pub(crate) fn new(document: &'doc Document<'input>) -> Result<Self, String> {
        let mut ids = HashMap::new();
        let mut model = None;

        for node in document.descendants().filter(|node| node.is_element()) {
            if let Some(id) = node.attribute("xs.id") {
                if id.is_empty() || ids.insert(id, node).is_some() {
                    return Err("duplicate or empty authoring xs.id".into());
                }
            }
            if node.has_tag_name("CModelSource")
                && node.attribute("xs.ref").is_none()
                && node.children().any(|child| child.is_element())
            {
                if model.replace(node).is_some() {
                    return Err("multiple authoring CModelSource definitions".into());
                }
            }
        }

        let graph = Self {
            document,
            ids,
            model,
        };
        for node in document
            .descendants()
            .filter(|node| node.is_element() && node.attribute("xs.ref").is_some())
        {
            graph.resolve(node)?;
        }
        Ok(graph)
    }

    pub(crate) fn resolve(&self, node: Node<'doc, 'input>) -> Result<Node<'doc, 'input>, String> {
        if !std::ptr::eq(node.document(), self.document) {
            return Err("authoring node belongs to another document".into());
        }

        let mut current = node;
        let mut visited = HashSet::new();
        for _ in 0..=MAX_ALIAS_DEPTH {
            let Some(id) = current.attribute("xs.ref") else {
                return Ok(current);
            };
            if !visited.insert(current.id()) {
                return Err("authoring xs.ref alias cycle".into());
            }
            current = *self
                .ids
                .get(id)
                .ok_or_else(|| "dangling authoring xs.ref".to_owned())?;
        }
        Err("authoring xs.ref chain exceeds 64 links".into())
    }

    pub(crate) fn field(
        &self,
        node: Node<'doc, 'input>,
        name: &str,
    ) -> Result<Option<Node<'doc, 'input>>, String> {
        let mut current = self.resolve(node)?;
        let mut visited = HashSet::new();

        for _ in 0..=MAX_SUPER_DEPTH {
            if !visited.insert(current.id()) {
                return Err("authoring super cycle".into());
            }
            let mut fields = current
                .children()
                .filter(|child| child.is_element() && child.attribute("xs.n") == Some(name));
            if let Some(field) = fields.next() {
                if fields.next().is_some() {
                    return Err(format!("duplicate authoring field {name:?}"));
                }
                return self.resolve(field).map(Some);
            }
            let mut parents = current
                .children()
                .filter(|child| child.is_element() && child.attribute("xs.n") == Some("super"));
            let Some(parent) = parents.next() else {
                return Ok(None);
            };
            if parents.next().is_some() {
                return Err("duplicate authoring super field".into());
            }
            current = self.resolve(parent)?;
        }
        Err("authoring super chain exceeds 64 nodes".into())
    }

    pub(crate) fn guid(&self, node: Node<'doc, 'input>) -> Result<Option<&'doc str>, String> {
        Ok(self
            .field(node, "guid")?
            .and_then(|guid| guid.attribute("uuid"))
            .map(str::trim)
            .filter(|guid| !guid.is_empty()))
    }

    pub(crate) fn model(&self) -> Result<Option<Node<'doc, 'input>>, String> {
        Ok(self.model)
    }

    pub(crate) fn source_list(
        &self,
        node: Node<'doc, 'input>,
        setname: &str,
    ) -> Result<Vec<Node<'doc, 'input>>, String> {
        let Some(set) = self.field(node, setname)? else {
            return Ok(Vec::new());
        };
        let Some(sources) = self.field(set, "_sources")? else {
            return Err(format!("authoring {setname} lacks _sources"));
        };
        sources
            .children()
            .filter(|child| child.is_element())
            .map(|child| self.resolve(child))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_direct_and_inherited_fields() {
        for xml in [
            r#"<root><Item><s xs.n="value">a</s><s xs.n="value">b</s></Item></root>"#,
            r#"<root><Item><Base xs.n="super"/><Base xs.n="super"/></Item></root>"#,
        ] {
            let document = Document::parse(xml).unwrap();
            let graph = Graph::new(&document).unwrap();
            let item = document.descendants().find(|node| node.has_tag_name("Item")).unwrap();
            assert!(graph.field(item, "value").is_err());
        }
    }

    #[test]
    fn reads_inline_and_referenced_guids_and_model_sources() {
        let xml = r#"<root>
            <Item><CFormGuid xs.n="guid" uuid="inline-uuid"/></Item>
            <Item><Guid xs.n="guid" xs.ref="guid-target"/></Item>
            <Guid xs.id="guid-target" uuid="shared-uuid"/>
            <CModelSource xs.id="model">
                <CDrawableSourceSet xs.n="drawableSourceSet">
                    <carray_list xs.n="_sources"><CArtMeshSource xs.ref="mesh"/></carray_list>
                </CDrawableSourceSet>
            </CModelSource>
            <CArtMeshSource xs.id="mesh"/>
        </root>"#;
        let document = Document::parse(xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let mut items = document
            .descendants()
            .filter(|node| node.has_tag_name("Item"));
        assert_eq!(
            graph.guid(items.next().unwrap()).unwrap(),
            Some("inline-uuid")
        );
        assert_eq!(
            graph.guid(items.next().unwrap()).unwrap(),
            Some("shared-uuid")
        );

        let model = graph.model().unwrap().unwrap();
        let sources = graph.source_list(model, "drawableSourceSet").unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].has_tag_name("CArtMeshSource"));
    }

    #[test]
    fn direct_fields_precede_super_fields_and_super_fields_are_inherited() {
        let xml = r#"<root><Derived>
            <s xs.n="value">direct</s>
            <Base xs.n="super"><s xs.n="value">base</s><s xs.n="fallback">inherited</s></Base>
        </Derived></root>"#;
        let document = Document::parse(xml).unwrap();
        let graph = Graph::new(&document).unwrap();
        let derived = document
            .descendants()
            .find(|node| node.has_tag_name("Derived"))
            .unwrap();
        assert_eq!(
            graph.field(derived, "value").unwrap().unwrap().text(),
            Some("direct")
        );
        assert_eq!(
            graph.field(derived, "fallback").unwrap().unwrap().text(),
            Some("inherited")
        );
    }

    #[test]
    fn rejects_missing_duplicate_and_cyclic_references() {
        for xml in [
            r#"<root><Item xs.ref="missing"/></root>"#,
            r#"<root><Item xs.id="same"/><Item xs.id="same"/></root>"#,
            r#"<root><Item xs.id="a" xs.ref="b"/><Item xs.id="b" xs.ref="a"/></root>"#,
        ] {
            let document = Document::parse(xml).unwrap();
            assert!(Graph::new(&document).is_err());
        }
    }

    #[test]
    fn rejects_alias_and_super_chains_past_their_limits() {
        let mut aliases = String::from("<root>");
        for index in 0..=MAX_ALIAS_DEPTH {
            aliases.push_str(&format!(
                r#"<Item xs.id="n{index}" xs.ref="n{}"/>"#,
                index + 1
            ));
        }
        aliases.push_str(&format!(
            r#"<Item xs.id="n{}"/></root>"#,
            MAX_ALIAS_DEPTH + 1
        ));
        let document = Document::parse(&aliases).unwrap();
        assert!(Graph::new(&document).is_err());

        let mut supers = String::from("<root>");
        for index in 0..=MAX_SUPER_DEPTH {
            supers.push_str(&format!(
                r#"<Item xs.id="n{index}"><Base xs.n="super" xs.ref="n{}"/></Item>"#,
                index + 1
            ));
        }
        supers.push_str(&format!(
            r#"<Item xs.id="n{}"/></root>"#,
            MAX_SUPER_DEPTH + 1
        ));
        let document = Document::parse(&supers).unwrap();
        let graph = Graph::new(&document).unwrap();
        let first = document
            .descendants()
            .find(|node| node.has_tag_name("Item"))
            .unwrap();
        assert!(graph.field(first, "missing").is_err());
    }

    #[test]
    fn permits_owner_cycles_that_are_not_alias_cycles() {
        let xml = r#"<root>
            <Item xs.id="a"><Item xs.n="owner" xs.ref="b"/></Item>
            <Item xs.id="b"><Item xs.n="owner" xs.ref="a"/></Item>
        </root>"#;
        let document = Document::parse(xml).unwrap();
        assert!(Graph::new(&document).is_ok());
    }

    #[test]
    fn model_is_optional_and_multiple_definitions_are_rejected() {
        let document = Document::parse("<root/>").unwrap();
        assert!(Graph::new(&document).unwrap().model().unwrap().is_none());

        let document = Document::parse(
            "<root><CModelSource><s xs.n='a'/></CModelSource><CModelSource><s xs.n='b'/></CModelSource></root>",
        )
        .unwrap();
        assert!(Graph::new(&document).is_err());
    }
}
