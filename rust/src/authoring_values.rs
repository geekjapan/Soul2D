use crate::authoring_graph::Graph;
use roxmltree::Node;

#[derive(Clone)]
pub(crate) struct Budget {
    remaining: usize,
    text_remaining: usize,
    exhausted: bool,
}

impl Budget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            text_remaining: 64 * 1024 * 1024,
            exhausted: false,
        }
    }

    pub(crate) fn consume(&mut self, count: usize) -> Result<(), String> {
        let Some(remaining) = self.remaining.checked_sub(count) else {
            self.exhausted = true;
            return Err("authoring source exceeds scalar budget".into());
        };
        self.remaining = remaining;
        Ok(())
    }

    pub(crate) fn text(&mut self, value: &str) -> Result<String, String> {
        let Some(remaining) = self.text_remaining.checked_sub(value.len()) else {
            self.exhausted = true;
            return Err("authoring source exceeds copied-text byte budget".into());
        };
        self.text_remaining = remaining;
        Ok(value.to_owned())
    }

    pub(crate) fn exhausted(&self) -> bool { self.exhausted }

    #[cfg(test)]
    pub(crate) fn remaining(&self) -> usize {
        self.remaining
    }

    #[cfg(test)]
    pub(crate) fn text_remaining(&self) -> usize {
        self.text_remaining
    }

    #[cfg(test)]
    pub(crate) fn with_text_limit(mut self, limit: usize) -> Self {
        self.text_remaining = limit;
        self
    }
}

pub(crate) fn required_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<Node<'d, 'i>, String> {
    graph
        .field(node, name)?
        .ok_or_else(|| format!("authoring {} lacks {name}", node.tag_name().name()))
}

pub(crate) fn text_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<Option<&'d str>, String> {
    Ok(graph.field(node, name)?.and_then(|value| value.text()))
}

pub(crate) fn guid_field<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    name: &str,
) -> Result<Option<&'d str>, String> {
    let Some(value) = graph.field(node, name)? else {
        return Ok(None);
    };
    if value.has_tag_name("null") {
        return Ok(None);
    }
    value
        .attribute("uuid")
        .filter(|value| !value.trim().is_empty())
        .map(Some)
        .ok_or_else(|| format!("authoring {name} lacks uuid"))
}

pub(crate) fn finite(text: &str) -> Result<f64, String> {
    text.trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| "invalid finite authoring number".to_owned())
}

pub(crate) fn number_field(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    default: Option<f64>,
) -> Result<f64, String> {
    match text_field(graph, node, name)? {
        Some(text) => finite(text),
        None => default.ok_or_else(|| format!("authoring field {name} lacks number")),
    }
}

pub(crate) fn bool_text(text: &str) -> Result<bool, String> {
    match text.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err("invalid authoring boolean".into()),
    }
}

pub(crate) fn bool_field(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    name: &str,
    default: bool,
) -> Result<bool, String> {
    text_field(graph, node, name)?.map_or(Ok(default), bool_text)
}

pub(crate) fn list<'d, 'i>(
    graph: &Graph<'d, 'i>,
    node: Node<'d, 'i>,
    budget: &mut Budget,
) -> Result<Vec<Node<'d, 'i>>, String> {
    let node = graph.resolve(node)?;
    let count = node.children().filter(|child| child.is_element()).count();
    check_count(node, count)?;
    budget.consume(count)?;
    node.children()
        .filter(|child| child.is_element())
        .map(|child| graph.resolve(child))
        .collect()
}

fn check_count(node: Node<'_, '_>, actual: usize) -> Result<(), String> {
    if let Some(count) = node.attribute("count") {
        if count.parse::<usize>().ok() != Some(actual) {
            return Err("authoring array count does not match content".into());
        }
    }
    Ok(())
}

pub(crate) fn float_array(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<Vec<f64>, String> {
    let node = graph.resolve(node)?;
    if node.has_tag_name("array_list") {
        return list(graph, node, budget)?
            .into_iter()
            .map(|child| {
                if !child.has_tag_name("f") {
                    return Err("authoring numeric list contains a non-float".into());
                }
                if child.children().any(|node| node.is_element()) {
                    return Err("authoring float list item contains nested elements".into());
                }
                finite(
                    child
                        .text()
                        .ok_or("authoring float list item lacks value")?,
                )
            })
            .collect();
    }
    if !node.has_tag_name("float-array") {
        return Err("authoring positions must be a float-array".into());
    }
    if node.children().any(|node| node.is_element()) {
        return Err("authoring float array contains nested elements".into());
    }
    let text = node.text().unwrap_or("");
    let count = text.split_whitespace().count();
    check_count(node, count)?;
    budget.consume(count)?;
    text.split_whitespace().map(finite).collect()
}

pub(crate) fn index_array(
    graph: &Graph<'_, '_>,
    node: Node<'_, '_>,
    budget: &mut Budget,
) -> Result<Vec<u32>, String> {
    let node = graph.resolve(node)?;
    if !node.has_tag_name("int-array") {
        return Err("authoring indices must be an int-array".into());
    }
    if node.children().any(|node| node.is_element()) {
        return Err("authoring index array contains nested elements".into());
    }
    let text = node.text().unwrap_or("");
    let count = text.split_whitespace().count();
    check_count(node, count)?;
    budget.consume(count)?;
    text.split_whitespace()
        .map(|text| {
            text.parse::<u32>()
                .map_err(|_| "invalid authoring index".into())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use roxmltree::Document;

    #[test]
    fn numeric_arrays_validate_counts_values_and_budget_before_allocation() {
        for (xml, limit) in [
            ("<float-array count='2'>1</float-array>", 10),
            ("<float-array count='1'>NaN</float-array>", 10),
            ("<float-array count='2'>1 2</float-array>", 1),
        ] {
            let document = Document::parse(xml).unwrap();
            let graph = Graph::new(&document).unwrap();
            assert!(float_array(&graph, document.root_element(), &mut Budget::new(limit)).is_err());
        }
        let document = Document::parse("<float-array count='3'>-1 0 1.25</float-array>").unwrap();
        let graph = Graph::new(&document).unwrap();
        assert_eq!(
            float_array(&graph, document.root_element(), &mut Budget::new(3)).unwrap(),
            vec![-1.0, 0.0, 1.25]
        );
    }
}
