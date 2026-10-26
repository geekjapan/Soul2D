use crate::authoring_values::Budget;
use std::collections::HashMap;

pub(crate) const ROOT_DEFORMER_GUID: &str = "71fae776-e218-4aee-873e-78e8ac0cb48a";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    ArtMesh,
    Warp,
    Rotation,
    Part,
    Glue,
    ArtPath,
}

pub(crate) struct HierarchyNode<'a> {
    pub(crate) guid: &'a str,
    pub(crate) kind: SourceKind,
    pub(crate) parent_part_guid: Option<&'a str>,
    pub(crate) target_deformer_guid: Option<&'a str>,
    pub(crate) glue_target_art_mesh_guids: Option<(&'a str, &'a str)>,
}

pub(crate) fn validate_hierarchy(
    nodes: &[HierarchyNode<'_>],
    root_part_guid: &str,
    budget: &mut Budget,
) -> Result<(), String> {
    if root_part_guid == ROOT_DEFORMER_GUID {
        return Err("authoring root part guid conflicts with root deformer".into());
    }
    if nodes.iter().any(|node| node.guid == ROOT_DEFORMER_GUID) {
        return Err("authoring source shadows root deformer".into());
    }

    let count = nodes.len();
    budget.consume(count)?;
    let mut indices = HashMap::with_capacity(count);
    for (index, node) in nodes.iter().enumerate() {
        if indices.insert(node.guid, index).is_some() {
            return Err("authoring source guids are not unique".into());
        }
    }

    let root = *indices
        .get(root_part_guid)
        .ok_or("authoring root part does not resolve")?;
    if nodes[root].kind != SourceKind::Part {
        return Err("authoring root guid does not identify a part".into());
    }
    if nodes[root].parent_part_guid.is_some() {
        return Err("authoring root part has a parent".into());
    }

    budget.consume(count)?;
    let mut edges = vec![None; count];
    for (index, node) in nodes.iter().enumerate() {
        let Some(parent_guid) = node.parent_part_guid else {
            continue;
        };
        let parent = *indices
            .get(parent_guid)
            .ok_or("authoring parent part does not resolve")?;
        if nodes[parent].kind != SourceKind::Part {
            return Err("authoring parent guid does not identify a part".into());
        }
        edges[index] = Some(parent);
    }

    for node in nodes {
        match (node.kind, node.glue_target_art_mesh_guids) {
            (SourceKind::Glue, Some((target_a_guid, target_b_guid))) => {
                if target_a_guid == target_b_guid {
                    return Err("authoring Glue targets must be distinct ArtMeshes".into());
                }
                for target_guid in [target_a_guid, target_b_guid] {
                    let target = *indices
                        .get(target_guid)
                        .ok_or("authoring Glue target ArtMesh does not resolve")?;
                    if nodes[target].kind != SourceKind::ArtMesh {
                        return Err("authoring Glue target guid does not identify an ArtMesh".into());
                    }
                }
            }
            (SourceKind::Glue, None) => {
                return Err("authoring Glue source lacks target ArtMeshes".into());
            }
            (_, Some(_)) => return Err("non-Glue source has Glue targets".into()),
            (_, None) => {}
        }
    }

    budget.consume(count)?;
    let mut state = vec![0; count];
    budget.consume(count)?;
    let mut work = Vec::with_capacity(count);
    ensure_acyclic(
        &edges,
        &mut state,
        &mut work,
        "authoring parent hierarchy contains a cycle",
    )?;

    edges.fill(None);
    for (index, node) in nodes.iter().enumerate() {
        let Some(target_guid) = node.target_deformer_guid else {
            continue;
        };
        if target_guid == ROOT_DEFORMER_GUID {
            continue;
        }
        // Hiyori Part records serialize an empty deformer GUID rather than an absent field.
        if node.kind == SourceKind::Part && target_guid == "00000000-0000-0000-0000-000000000000" {
            continue;
        }
        let target = *indices
            .get(target_guid)
            .ok_or("authoring target deformer does not resolve")?;
        if !matches!(nodes[target].kind, SourceKind::Warp | SourceKind::Rotation) {
            return Err("authoring target guid does not identify a deformer".into());
        }
        if target == index {
            return Err("authoring deformer targets itself".into());
        }
        edges[index] = Some(target);
    }
    ensure_acyclic(
        &edges,
        &mut state,
        &mut work,
        "authoring deformer targets contain a cycle",
    )
}

fn ensure_acyclic(
    edges: &[Option<usize>],
    state: &mut [u8],
    work: &mut Vec<usize>,
    error: &'static str,
) -> Result<(), String> {
    state.fill(0);
    for start in 0..edges.len() {
        if state[start] != 0 {
            continue;
        }
        work.clear();
        let mut next = Some(start);
        while let Some(index) = next {
            match state[index] {
                0 => {
                    state[index] = 1;
                    work.push(index);
                    next = edges[index];
                }
                1 => return Err(error.into()),
                _ => break,
            }
        }
        for &index in &*work {
            state[index] = 2;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        guid: &'static str,
        kind: SourceKind,
        parent_part_guid: Option<&'static str>,
        target_deformer_guid: Option<&'static str>,
    ) -> HierarchyNode<'static> {
        HierarchyNode {
            guid,
            kind,
            parent_part_guid,
            target_deformer_guid,
            glue_target_art_mesh_guids: None,
        }
    }

    fn invalid(nodes: &[HierarchyNode<'_>], root: &str) {
        assert!(validate_hierarchy(nodes, root, &mut Budget::new(100)).is_err());
    }

    #[test]
    fn validates_nested_references_without_requiring_topological_order() {
        let nodes = [
            node("mesh", SourceKind::ArtMesh, Some("nested"), Some("warp")),
            node("warp", SourceKind::Warp, None, Some("rotation")),
            node("nested", SourceKind::Part, Some("root"), None),
            node(
                "rotation",
                SourceKind::Rotation,
                None,
                Some(ROOT_DEFORMER_GUID),
            ),
            node("root", SourceKind::Part, None, None),
        ];
        assert!(validate_hierarchy(&nodes, "root", &mut Budget::new(20)).is_ok());
    }

    #[test]
    fn preserves_empty_part_target_without_accepting_empty_mesh_target() {
        let empty = "00000000-0000-0000-0000-000000000000";
        let nodes = [node("root", SourceKind::Part, None, Some(empty))];
        assert!(validate_hierarchy(&nodes, "root", &mut Budget::new(4)).is_ok());
        assert_eq!(nodes[0].target_deformer_guid, Some(empty));
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("mesh", SourceKind::ArtMesh, None, Some(empty)),
            ],
            "root",
        );
    }

    #[test]
    fn preserves_missing_optional_references() {
        let nodes = [
            node("root", SourceKind::Part, None, None),
            node("unparented", SourceKind::ArtMesh, None, None),
        ];
        assert!(validate_hierarchy(&nodes, "root", &mut Budget::new(8)).is_ok());
        assert_eq!(nodes[1].parent_part_guid, None);
        assert_eq!(nodes[1].target_deformer_guid, None);
    }

    #[test]
    fn rejects_dangling_and_wrong_kind_owners_and_targets() {
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("mesh", SourceKind::ArtMesh, Some("missing"), None),
            ],
            "root",
        );
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("mesh", SourceKind::ArtMesh, Some("mesh"), None),
            ],
            "root",
        );
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("warp", SourceKind::Warp, None, Some("missing")),
            ],
            "root",
        );
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("mesh", SourceKind::ArtMesh, None, Some("root")),
            ],
            "root",
        );
    }

    #[test]
    fn validates_glue_targets_as_distinct_resolved_art_meshes() {
        let mut nodes = [
            node("root", SourceKind::Part, None, None),
            node("mesh-a", SourceKind::ArtMesh, Some("root"), None),
            node("mesh-b", SourceKind::ArtMesh, Some("root"), None),
            node("glue", SourceKind::Glue, Some("root"), None),
        ];
        nodes[3].glue_target_art_mesh_guids = Some(("mesh-a", "mesh-b"));
        assert!(validate_hierarchy(&nodes, "root", &mut Budget::new(30)).is_ok());

        nodes[3].glue_target_art_mesh_guids = Some(("mesh-a", "missing"));
        invalid(&nodes, "root");
        nodes[3].glue_target_art_mesh_guids = Some(("mesh-a", "root"));
        invalid(&nodes, "root");
        nodes[3].glue_target_art_mesh_guids = Some(("mesh-a", "mesh-a"));
        invalid(&nodes, "root");
        nodes[3].glue_target_art_mesh_guids = None;
        invalid(&nodes, "root");
    }

    #[test]
    fn rejects_self_and_multinode_deformer_cycles() {
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("warp", SourceKind::Warp, None, Some("warp")),
            ],
            "root",
        );
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("warp", SourceKind::Warp, None, Some("rotation")),
                node("rotation", SourceKind::Rotation, None, Some("warp")),
            ],
            "root",
        );
    }

    #[test]
    fn rejects_parent_cycles_even_when_the_root_is_valid() {
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("a", SourceKind::Part, Some("b"), None),
                node("b", SourceKind::Part, Some("a"), None),
            ],
            "root",
        );
    }

    #[test]
    fn keeps_root_part_and_root_deformer_distinct() {
        invalid(
            &[node("root", SourceKind::Part, None, None)],
            ROOT_DEFORMER_GUID,
        );
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node(ROOT_DEFORMER_GUID, SourceKind::Warp, None, None),
            ],
            "root",
        );
        invalid(
            &[
                node("root", SourceKind::Part, Some("parent"), None),
                node("parent", SourceKind::Part, None, None),
            ],
            "root",
        );
        invalid(&[node("root", SourceKind::ArtMesh, None, None)], "root");
    }

    #[test]
    fn validates_unreferenced_sources_and_unique_guids() {
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("unused", SourceKind::Warp, None, Some("missing")),
            ],
            "root",
        );
        invalid(
            &[
                node("root", SourceKind::Part, None, None),
                node("root", SourceKind::Part, None, None),
            ],
            "root",
        );
    }

    #[test]
    fn charges_each_validation_buffer_before_allocation() {
        let nodes = [node("root", SourceKind::Part, None, None)];
        assert!(validate_hierarchy(&nodes, "root", &mut Budget::new(3)).is_err());
        assert!(validate_hierarchy(&nodes, "root", &mut Budget::new(4)).is_ok());
    }
}
